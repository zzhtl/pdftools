//! PDF 页面：合并、拆分、删页、重排、旋转。
//!
//! 加进来的 PDF 在后台打开，打开后它的每一页成为网格里的一格。格子可以拖动排序、
//! 旋转、删除；导出时按格子的顺序拼成新文件（见 `pdfcore::ops::pdf_pages`）。
//! 网格只画看得见的那几行，缩略图也只画看得见的，几百页的文件照样顺。

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;

use pdfcore::fsio::OutputNamer;
use pdfcore::ops::pdf_pages::{assemble, PagePick, Source};
use pdfcore::pdf::render::Document;

use crate::app::App;
use crate::job::{human_size, write_atomic, Done, ItemResult, Job, Stop};
use crate::loader::Loader;
use crate::thumbs::{Thumb, ThumbCache};

use super::common::{self, FileList, PDF_EXTENSIONS};
use super::{file_label, FOOTER};

/// 一格的大小（点）。
const CELL: egui::Vec2 = egui::vec2(150.0, 196.0);
/// 缩略图占的正方形（点）。
const THUMB_BOX: f32 = 128.0;
/// 缩略图画多大（长边，像素）：高分屏上也清楚。
const THUMB_PX: f32 = 256.0;

/// 缩略图的键：哪份文档的哪一页。按文档对象认，同一个文件加两次是两份。
#[derive(Clone)]
pub struct PageKey {
    doc: Arc<Document>,
    page: usize,
}

impl PartialEq for PageKey {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.doc, &other.doc) && self.page == other.page
    }
}

impl Eq for PageKey {}

impl Hash for PageKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.doc).hash(state);
        self.page.hash(state);
    }
}

/// 网格里的一格：第 `doc` 份 PDF 的第 `page` 页（都从 0 开始），在原有方向上再顺时针
/// 转 `rotate` 度。相等只看 `id`：转过方向，选中的还是这一格。
#[derive(Clone, Debug)]
pub struct Tile {
    id: u64,
    doc: usize,
    page: usize,
    rotate: i32,
}

impl PartialEq for Tile {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for Tile {}

impl Hash for Tile {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

/// 加进来的一份 PDF。
struct Added {
    path: PathBuf,
    /// 还在后台打开时是 None。
    doc: Option<Arc<Document>>,
}

pub struct State {
    docs: Vec<Added>,
    pub tiles: FileList<Tile>,
    next_id: u64,
    opening: Loader<PathBuf, Result<Arc<Document>, String>>,
    thumbs: ThumbCache<PageKey>,
    /// 格子上次对过的版本（见 `FileList::changes`）。
    seen: u64,
    /// 拆分时每几页一个文件。
    pub split_every: usize,
    /// 打不开的文件，一句话一个。
    pub failed: Vec<String>,
    /// 上次添加 PDF、保存结果的文件夹。
    pub open_dir: Option<PathBuf>,
    pub out_dir: Option<PathBuf>,
}

impl State {
    pub fn new(ctx: &egui::Context) -> Self {
        Self {
            docs: Vec::new(),
            tiles: FileList::default(),
            next_id: 0,
            opening: Loader::new(ctx, 2, open),
            thumbs: ThumbCache::new(ctx, draw_page),
            seen: 0,
            split_every: 1,
            failed: Vec::new(),
            open_dir: None,
            out_dir: None,
        }
    }

    /// 加进来，返回不认的文件有几个。在后台打开，打开了才出现在网格里。
    pub fn add_paths(&mut self, paths: Vec<PathBuf>) -> usize {
        let mut rejected = 0;
        for path in paths {
            if !common::has_extension(&path, PDF_EXTENSIONS) {
                rejected += 1;
                continue;
            }
            let known = self
                .docs
                .iter()
                .find(|d| d.path == path)
                .and_then(|d| d.doc.clone());
            self.docs.push(Added {
                path: path.clone(),
                doc: None,
            });
            match known {
                Some(doc) => self.opened(self.docs.len() - 1, doc),
                None => self.opening.request(&path),
            }
        }
        rejected
    }

    pub fn clear(&mut self) {
        self.docs.clear();
        self.tiles.clear();
        self.failed.clear();
        self.opening.retain(|_| false);
    }

    /// 在 `App::logic` 里调：收下打开好的文档，上传画好的缩略图。
    pub fn pump(&mut self, ctx: &egui::Context) {
        self.thumbs.pump(ctx);
        for (path, result) in self.opening.drain() {
            // 同一个文件加了几次，每一份都在等它。
            let waiting: Vec<usize> = (0..self.docs.len())
                .filter(|&i| self.docs[i].path == path && self.docs[i].doc.is_none())
                .collect();
            match result {
                Ok(doc) => {
                    for i in waiting {
                        self.opened(i, doc.clone());
                    }
                }
                Err(e) if !waiting.is_empty() => {
                    self.failed.push(format!("{}：{e}", file_label(&path)));
                }
                Err(_) => {}
            }
        }
        // 格子变了才对一遍：删掉的页不再画，纹理释放。
        if self.tiles.changes() != self.seen {
            self.seen = self.tiles.changes();
            let keep: HashSet<(*const Document, usize)> = self
                .tiles
                .items
                .iter()
                .filter_map(|t| {
                    self.docs[t.doc]
                        .doc
                        .as_ref()
                        .map(|d| (Arc::as_ptr(d), t.page))
                })
                .collect();
            self.thumbs
                .retain(|k| keep.contains(&(Arc::as_ptr(&k.doc), k.page)));
        }
    }

    /// 第 `index` 份打开好了：它的页排在先加进来的那些文件的页后面。
    fn opened(&mut self, index: usize, doc: Arc<Document>) {
        let count = doc.page_count();
        self.docs[index].doc = Some(doc);
        let at = self
            .tiles
            .items
            .iter()
            .rposition(|t| t.doc < index)
            .map_or(0, |p| p + 1);
        let tiles: Vec<Tile> = (0..count)
            .map(|page| {
                self.next_id += 1;
                Tile {
                    id: self.next_id,
                    doc: index,
                    page,
                    rotate: 0,
                }
            })
            .collect();
        self.tiles.insert(at, tiles);
    }

    fn rotate_selected(&mut self, degrees: i32) {
        for i in 0..self.tiles.items.len() {
            if self.tiles.is_selected(i) {
                let t = &mut self.tiles.items[i];
                t.rotate = (t.rotate + degrees).rem_euclid(360);
            }
        }
    }

    fn selected_count(&self) -> usize {
        (0..self.tiles.items.len())
            .filter(|&i| self.tiles.is_selected(i))
            .count()
    }
}

fn open(path: &PathBuf) -> Result<Arc<Document>, String> {
    let data = std::fs::read(path).map_err(|e| format!("读不出来：{e}"))?;
    // 解析器碰到坏文件 panic 的话，只算这一份打不开，别让后台线程就此停掉。
    std::panic::catch_unwind(move || Document::open(data))
        .map_err(|_| "解析时出错".to_string())?
        .map(Arc::new)
        .map_err(|e| e.to_string())
}

fn draw_page(key: &PageKey) -> Option<egui::ColorImage> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let (w, h) = key.doc.page_size(key.page);
        let scale = THUMB_PX / w.max(h).max(1.0);
        let cache = key.doc.cache();
        let img = key.doc.render(key.page, scale, &cache);
        egui::ColorImage::from_rgb([img.width() as usize, img.height() as usize], img.as_raw())
    }))
    .ok()
}

pub fn ui(app: &mut App, ui: &mut egui::Ui) {
    let busy = app.busy();
    let ctx = ui.ctx().clone();
    let pal = crate::theme::palette(ui);
    let st = &mut app.pages;
    let selected = st.selected_count();

    ui.horizontal(|ui| {
        if ui.button("添加 PDF…").on_hover_text("Ctrl+O").clicked() || common::open_shortcut(ui)
        {
            if let Some(picked) = common::pick_files(&mut st.open_dir, "PDF", PDF_EXTENSIONS) {
                st.add_paths(picked);
            }
        }
        if ui.button("清空").clicked() {
            st.clear();
        }
        ui.separator();
        ui.add_enabled_ui(selected > 0, |ui| {
            if ui
                .button("↺ 左转")
                .on_hover_text("选中的页逆时针转 90°")
                .clicked()
            {
                st.rotate_selected(-90);
            }
            if ui
                .button("↻ 右转")
                .on_hover_text("选中的页顺时针转 90°")
                .clicked()
            {
                st.rotate_selected(90);
            }
            if ui.button("删除选中").on_hover_text("Delete").clicked() {
                st.tiles.remove_selected();
            }
        });
    });
    ui.weak(
        "每一页是一格：拖动调整顺序，点选后可旋转、删除（Ctrl 加选，Shift 连选）。\
         导出时按格子的顺序拼成新文件，原文件不动。",
    );
    if st.opening.pending() > 0 {
        ui.weak(format!("正在打开 {} 个文件…", st.opening.pending()));
    }
    let mut dismiss = false;
    for msg in &st.failed {
        ui.horizontal(|ui| {
            ui.colored_label(pal.error, format!("打不开 {msg}"));
            dismiss |= ui.small_button("✖").clicked();
        });
    }
    if dismiss {
        st.failed.clear();
    }
    ui.separator();

    if st.tiles.items.is_empty() {
        ui.weak("还没有添加 PDF。");
        return;
    }

    let height = ui.available_height() - FOOTER;
    grid(ui, st, height);

    ui.separator();
    let mut action = None;
    ui.horizontal(|ui| {
        if common::run_button(ui, "导出为一个 PDF…", !busy) {
            action = Some(Export::All);
        }
        let label = format!("导出选中的 {selected} 页…");
        if ui
            .add_enabled(!busy && selected > 0, egui::Button::new(label))
            .clicked()
        {
            action = Some(Export::Selected);
        }
        ui.separator();
        ui.label("每");
        ui.add(egui::DragValue::new(&mut st.split_every).range(1..=9999));
        ui.label("页一个文件");
        if ui
            .add_enabled(!busy, egui::Button::new("拆分到文件夹…"))
            .clicked()
        {
            action = Some(Export::Split);
        }
    });
    if let Some(a) = action {
        start(app, &ctx, a);
    }
}

fn grid(ui: &mut egui::Ui, st: &mut State, height: f32) {
    let n = st.tiles.items.len();
    let gap = ui.spacing().item_spacing.x;
    let cols = (((ui.available_width() + gap) / (CELL.x + gap)) as usize).max(1);
    let rows = n.div_ceil(cols);
    let modifiers = ui.input(|i| i.modifiers);
    let mut clicked: Option<usize> = None;
    let mut drag: Option<(usize, usize)> = None;
    let mut remove: Option<usize> = None;
    let mut turn: Option<(usize, i32)> = None;

    egui::ScrollArea::vertical()
        .id_salt("pdf-pages")
        .max_height(height.max(CELL.y))
        .auto_shrink([false, true])
        .show_rows(ui, CELL.y, rows, |ui, range| {
            for row in range {
                ui.horizontal(|ui| {
                    for i in row * cols..((row + 1) * cols).min(n) {
                        let tile = st.tiles.items[i].clone();
                        let selected = st.tiles.is_selected(i);
                        let added = &st.docs[tile.doc];
                        let key = added.doc.clone().map(|doc| PageKey {
                            doc,
                            page: tile.page,
                        });
                        let thumb = key.map_or(Thumb::Failed, |k| st.thumbs.get(&k));
                        let id = egui::Id::new(("page-tile", tile.id));
                        let resp = common::drag_source(ui, id, i, CELL, |ui| {
                            cell(ui, i, &tile, &added.path, selected, thumb)
                        });
                        match resp.inner {
                            CellAction::Select => clicked = Some(i),
                            CellAction::Turn(d) => turn = Some((i, d)),
                            CellAction::Remove => remove = Some(i),
                            CellAction::None => {}
                        }
                        let resp = resp.response;
                        // 拖着别的格子经过时，画一条竖线指出会插到哪一侧。
                        if let Some(payload) = resp.dnd_hover_payload::<usize>() {
                            let rect = resp.rect;
                            let x = ui.input(|s| s.pointer.interact_pos()).map(|p| p.x);
                            let before = x.is_none_or(|x| x < rect.center().x);
                            let line_x = if before {
                                rect.left() - gap / 2.0
                            } else {
                                rect.right() + gap / 2.0
                            };
                            ui.painter().vline(
                                line_x,
                                rect.y_range(),
                                egui::Stroke::new(3.0, ui.visuals().selection.bg_fill),
                            );
                            if resp.dnd_release_payload::<usize>().is_some() {
                                drag = Some((*payload, if before { i } else { i + 1 }));
                            }
                        }
                    }
                });
            }
        });

    if let Some(i) = clicked {
        st.tiles.click(i, modifiers.command, modifiers.shift);
    }
    if let Some((i, d)) = turn {
        let t = &mut st.tiles.items[i];
        t.rotate = (t.rotate + d).rem_euclid(360);
    }
    if let Some(i) = remove {
        st.tiles.remove(i);
    }
    if let Some((from, to)) = drag {
        st.tiles.move_item(from, to);
    }
    let typing = ui.ctx().memory(|m| m.focused().is_some());
    if !typing && ui.input(|i| i.key_pressed(egui::Key::Delete)) {
        st.tiles.remove_selected();
    }
}

enum CellAction {
    None,
    Select,
    Turn(i32),
    Remove,
}

/// 一格：缩略图（按要转的方向画），下面是序号、出处和三个小按钮。
fn cell(
    ui: &mut egui::Ui,
    index: usize,
    tile: &Tile,
    path: &std::path::Path,
    selected: bool,
    thumb: Thumb,
) -> CellAction {
    let mut action = CellAction::None;
    let layout = egui::Layout::top_down(egui::Align::Center);
    ui.allocate_ui_with_layout(CELL, layout, |ui| {
        ui.set_min_size(CELL);
        // 选中时的框画在缩略图外面一圈，留出地方，第一行的框才不会被网格顶边切掉。
        ui.add_space(6.0);
        let (rect, resp) =
            ui.allocate_exact_size(egui::vec2(THUMB_BOX, THUMB_BOX), egui::Sense::click());
        match thumb {
            Thumb::Ready(tex) => {
                let size = tex.size_vec2();
                let s = THUMB_BOX / size.x.max(size.y);
                let quarter = tile.rotate.rem_euclid(360) / 90;
                // 转 90° 或 270° 时显示出来的宽高对调。
                let shown = if quarter % 2 == 1 {
                    egui::vec2(size.y, size.x) * s
                } else {
                    size * s
                };
                egui::Image::new(&tex)
                    .rotate(
                        quarter as f32 * std::f32::consts::FRAC_PI_2,
                        egui::Vec2::splat(0.5),
                    )
                    .paint_at(ui, egui::Rect::from_center_size(rect.center(), size * s));
                ui.painter().rect_stroke(
                    egui::Rect::from_center_size(rect.center(), shown),
                    0.0,
                    ui.visuals().widgets.noninteractive.bg_stroke,
                    egui::StrokeKind::Outside,
                );
            }
            Thumb::Pending => {
                egui::Spinner::new().paint_at(
                    ui,
                    egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(20.0)),
                );
            }
            Thumb::Failed => {
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "无法预览",
                    egui::FontId::proportional(13.0),
                    ui.visuals().weak_text_color(),
                );
            }
        }
        if selected {
            ui.painter().rect_stroke(
                rect.expand(4.0),
                4.0,
                egui::Stroke::new(2.5, ui.visuals().selection.bg_fill),
                egui::StrokeKind::Outside,
            );
        }
        if resp.clicked() {
            action = CellAction::Select;
        }
        let stem = path.file_stem().unwrap_or_default().to_string_lossy();
        ui.add(
            egui::Label::new(format!("{}. {stem} · 第 {} 页", index + 1, tile.page + 1)).truncate(),
        )
        .on_hover_text(path.display().to_string());
        ui.horizontal(|ui| {
            if ui.small_button("↺").on_hover_text("逆时针转 90°").clicked() {
                action = CellAction::Turn(-90);
            }
            if ui.small_button("↻").on_hover_text("顺时针转 90°").clicked() {
                action = CellAction::Turn(90);
            }
            if ui.small_button("✖").on_hover_text("删掉这一页").clicked() {
                action = CellAction::Remove;
            }
        });
    });
    action
}

/// 导出要用到哪些源文件（`docs` 里的下标，从小到大），以及每一格取自其中第几份的
/// 哪一页、再转多少度。
fn plan(tiles: &[Tile]) -> (Vec<usize>, Vec<PagePick>) {
    let mut used: Vec<usize> = tiles.iter().map(|t| t.doc).collect();
    used.sort_unstable();
    used.dedup();
    let picks = tiles
        .iter()
        .map(|t| PagePick {
            source: used.binary_search(&t.doc).expect("用到的文件都在里面"),
            page: t.page,
            rotate: t.rotate,
        })
        .collect();
    (used, picks)
}

#[derive(Clone, Copy)]
enum Export {
    /// 全部格子拼成一个文件。
    All,
    /// 选中的格子拼成一个文件。
    Selected,
    /// 每 `split_every` 格一个文件。
    Split,
}

enum Target {
    File(PathBuf),
    Folder(PathBuf, usize),
}

fn start(app: &mut App, ctx: &egui::Context, export: Export) {
    let st = &mut app.pages;
    let tiles: Vec<Tile> = match export {
        Export::Selected => (0..st.tiles.items.len())
            .filter(|&i| st.tiles.is_selected(i))
            .map(|i| st.tiles.items[i].clone())
            .collect(),
        Export::All | Export::Split => st.tiles.items.clone(),
    };
    if tiles.is_empty() {
        return;
    }
    let (used, picks) = plan(&tiles);
    let docs: Vec<Arc<Document>> = used
        .iter()
        .filter_map(|&d| st.docs[d].doc.clone())
        .collect();
    let inputs: Vec<PathBuf> = used.iter().map(|&d| st.docs[d].path.clone()).collect();
    let first = &st.docs[tiles[0].doc].path;
    let stem = first
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();

    let target = match export {
        Export::Split => match common::pick_folder(&mut st.out_dir) {
            Some(dir) => Target::Folder(dir, st.split_every.max(1)),
            None => return,
        },
        Export::All | Export::Selected => {
            let name = match export {
                Export::Selected => format!("{stem}_选中的页.pdf"),
                _ if used.len() > 1 => "合并.pdf".to_string(),
                _ => format!("{stem}_整理.pdf"),
            };
            match common::pick_save(&mut st.out_dir, &name, "PDF", &["pdf"]) {
                Some(out) => Target::File(out),
                None => return,
            }
        }
    };
    let planned = match &target {
        Target::Folder(_, every) => picks.len().div_ceil(*every),
        Target::File(_) => 0,
    };

    app.job = Some(Job::spawn(ctx, planned, move |worker| {
        worker.set(None, "正在读取原文件…");
        let sources = docs
            .iter()
            .map(|d| Source::open(d.data()))
            .collect::<Result<Vec<_>, _>>()?;
        let mut namer = OutputNamer::new(&inputs);
        match target {
            Target::File(out) => {
                if namer.is_input(&out) {
                    return Err("输出文件不能是某个原文件本身，请换一个文件名".into());
                }
                worker.set(None, format!("正在拼接 {} 页…", picks.len()));
                let report = assemble(&sources, &picks)?;
                for w in report.warnings {
                    worker.warn(w);
                }
                write_atomic(&out, &report.value)?;
                Ok(Done {
                    summary: format!(
                        "已导出 {} 页，{}",
                        picks.len(),
                        human_size(report.value.len() as u64)
                    ),
                    output: Some(out),
                })
            }
            Target::Folder(dir, every) => {
                let parts: Vec<&[PagePick]> = picks.chunks(every).collect();
                // 每一份都会报一遍同样的提示（丢了书签之类），只说一次。
                let mut said = HashSet::new();
                for (k, part) in parts.iter().enumerate() {
                    if worker.is_cancelled() {
                        return Err(Stop::Cancelled);
                    }
                    worker.set(
                        Some(k as f32 / parts.len() as f32),
                        format!("{}/{}", k + 1, parts.len()),
                    );
                    let (a, b) = (k * every + 1, k * every + part.len());
                    let name = if a == b {
                        format!("{stem}_p{a}")
                    } else {
                        format!("{stem}_p{a}-{b}")
                    };
                    let out = namer.name(&dir, &name, "pdf");
                    let report = assemble(&sources, part)?;
                    for w in report.warnings {
                        if said.insert(w.detail.clone()) {
                            worker.warn(w);
                        }
                    }
                    write_atomic(&out, &report.value)?;
                    worker.item(ItemResult {
                        input: out.clone(),
                        output: Some(out),
                        error: None,
                        detail: format!(
                            "{} 页，{}",
                            part.len(),
                            human_size(report.value.len() as u64)
                        ),
                    });
                }
                Ok(Done {
                    summary: format!("已拆成 {} 个文件", parts.len()),
                    output: None,
                })
            }
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `pages` 页的 PDF，第 i 页宽 20 + i 点，认得出是哪一页。
    fn pdf(pages: usize) -> Arc<Document> {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("pdftools-pages-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let images: Vec<PathBuf> = (0..pages)
            .map(|i| {
                let p = dir.join(format!("{i}.png"));
                image::RgbImage::new(20 + i as u32, 30).save(&p).unwrap();
                p
            })
            .collect();
        let pdf = pdfcore::ops::images_to_pdf::run(
            &images,
            pdfcore::imaging::Tier::Lossless,
            &Default::default(),
            &pdfcore::NoProgress,
        )
        .unwrap()
        .value
        .pdf;
        let _ = std::fs::remove_dir_all(&dir);
        Arc::new(Document::open(pdf).unwrap())
    }

    fn pages_of(st: &State) -> Vec<(usize, usize)> {
        st.tiles.items.iter().map(|t| (t.doc, t.page)).collect()
    }

    fn state_with(files: &[&str]) -> State {
        let mut st = State::new(&egui::Context::default());
        for f in files {
            st.docs.push(Added {
                path: PathBuf::from(f),
                doc: None,
            });
        }
        st
    }

    /// 后加的文件先打开好了，它的页也排在先加的文件后面。
    #[test]
    fn pages_follow_the_order_files_were_added() {
        let mut st = state_with(&["a.pdf", "b.pdf", "c.pdf"]);
        st.opened(2, pdf(1));
        st.opened(0, pdf(2));
        st.opened(1, pdf(1));
        assert_eq!(pages_of(&st), [(0, 0), (0, 1), (1, 0), (2, 0)]);
    }

    /// 转过方向，选中的还是这几格；没选中的不动。
    #[test]
    fn turning_keeps_the_selection() {
        let mut st = state_with(&["a.pdf"]);
        st.opened(0, pdf(3));
        st.tiles.click(0, false, false);
        st.tiles.click(2, true, false);
        st.rotate_selected(90);
        st.rotate_selected(90);
        st.rotate_selected(-270);
        let turned: Vec<i32> = st.tiles.items.iter().map(|t| t.rotate).collect();
        assert_eq!(turned, [270, 0, 270]);
        assert!(st.tiles.is_selected(0) && !st.tiles.is_selected(1) && st.tiles.is_selected(2));
        assert_eq!(st.selected_count(), 2);
    }

    /// 导出只带上用到的文件；每一格指向其中的第几份。
    #[test]
    fn export_plan_names_only_the_files_in_use() {
        let tile = |id, doc, page, rotate| Tile {
            id,
            doc,
            page,
            rotate,
        };
        let tiles = [tile(1, 2, 0, 0), tile(2, 0, 3, 90), tile(3, 2, 1, 0)];
        let (used, picks) = plan(&tiles);
        assert_eq!(used, [0, 2]);
        let picks: Vec<(usize, usize, i32)> =
            picks.iter().map(|p| (p.source, p.page, p.rotate)).collect();
        assert_eq!(picks, [(1, 0, 0), (0, 3, 90), (1, 1, 0)]);
    }
}
