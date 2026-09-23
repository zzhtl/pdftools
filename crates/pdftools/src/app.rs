//! 主界面骨架：四个 Tab、任务状态条、警告面板。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use pdfcore::imaging::Tier;
use pdfcore::{DatedFile, TimeSource, Warning, WarningKind};

use crate::job::{Job, State};
use crate::loader::Loader;
use crate::prefs;
use crate::tabs;
use crate::theme;
use crate::thumbs::{self, ThumbCache};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    ImagesToPdf,
    DocxToPdf,
    PdfCompress,
    ImagesCompress,
    PdfPages,
    PdfToImages,
}

impl Tab {
    pub const ALL: [Tab; 6] = [
        Tab::ImagesToPdf,
        Tab::DocxToPdf,
        Tab::PdfCompress,
        Tab::ImagesCompress,
        Tab::PdfPages,
        Tab::PdfToImages,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Tab::ImagesToPdf => "图片转 PDF",
            Tab::DocxToPdf => "Word 转 PDF",
            Tab::PdfCompress => "PDF 压缩",
            Tab::ImagesCompress => "图片压缩",
            Tab::PdfPages => "PDF 页面",
            Tab::PdfToImages => "PDF 转图片",
        }
    }

    /// 拖进来的 PDF、图片落到哪一页：当前页签收这类文件就留在当前页签，
    /// 否则按类型去默认的页签。
    fn for_pdfs(self) -> Tab {
        match self {
            Tab::PdfCompress | Tab::PdfPages | Tab::PdfToImages => self,
            _ => Tab::PdfCompress,
        }
    }

    fn for_images(self) -> Tab {
        match self {
            Tab::ImagesCompress => self,
            _ => Tab::ImagesToPdf,
        }
    }
}

pub struct App {
    pub tab: Tab,
    pub images: tabs::img2pdf::State,
    pub docx: tabs::docx2pdf::State,
    pub compress: tabs::pdf_compress::State,
    pub img_compress: tabs::img_compress::State,
    pub pages: tabs::pdf_pages::State,
    pub pdf2img: tabs::pdf2img::State,
    pub job: Option<Job>,
    pub thumbs: ThumbCache<PathBuf>,
    /// 在后台读图片的拍摄时间。
    pub times: Loader<PathBuf, Option<DatedFile>>,
    /// 拖进来、命令行给的文件里有多少没认出来：一句提示，用户点掉为止。
    pub notice: Option<String>,
    /// 启动时探测到的界面字体名；None 表示本机没有中文字体。
    pub ui_font: Option<String>,
    /// 图片列表上次对过的版本（见 `FileList::changes`）。
    images_seen: u64,
    /// 首帧耗时只记一次。
    first_frame_logged: bool,
}

impl App {
    /// `storage` 里有上次记下的设置就接着用（见 `prefs`）。
    pub fn new(
        ctx: &egui::Context,
        ui_font: Option<String>,
        storage: Option<&dyn eframe::Storage>,
    ) -> Self {
        let mut app = Self {
            tab: Tab::ImagesToPdf,
            images: Default::default(),
            docx: Default::default(),
            compress: Default::default(),
            img_compress: Default::default(),
            pages: tabs::pdf_pages::State::new(ctx),
            pdf2img: Default::default(),
            job: None,
            thumbs: thumbs::images(ctx),
            times: Loader::new(ctx, 2, |p: &PathBuf| pdfcore::imaging::read_time_quick(p)),
            notice: None,
            ui_font,
            images_seen: 0,
            first_frame_logged: false,
        };
        if let Some(storage) = storage {
            prefs::load(&mut app, storage);
        }
        app
    }

    /// 按扩展名把文件分派到对应的 Tab，并切换过去；文件夹展开成里面的文件。
    /// 拖放和命令行参数共用这条路径。
    pub fn open_paths(&mut self, paths: Vec<PathBuf>) {
        let paths = expand_folders(paths);
        if paths.is_empty() {
            return;
        }
        let here = self.tab;
        let ext = |p: &Path| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase())
                .unwrap_or_default()
        };
        // HEIC 图片转 PDF 收下（给一句有用的提示），图片压缩不收。
        let image = |p: &Path| {
            pdfcore::imaging::probe::looks_like_image(p)
                || (here.for_images() == Tab::ImagesToPdf && pdfcore::imaging::probe::is_heif(p))
        };
        let (mut docx, mut pdfs, mut images, mut other) = (vec![], vec![], vec![], vec![]);
        for p in paths {
            match ext(&p).as_str() {
                "docx" | "doc" => docx.push(p),
                "pdf" => pdfs.push(p),
                _ if image(&p) => images.push(p),
                _ => other.push(p),
            }
        }

        if !images.is_empty() {
            self.tab = here.for_images();
            match self.tab {
                Tab::ImagesCompress => self.img_compress.add_paths(images),
                _ => self.images.add_paths(images),
            };
        }
        if !docx.is_empty() {
            self.tab = Tab::DocxToPdf;
            self.docx.add_paths(docx);
        }
        if !pdfs.is_empty() {
            self.tab = here.for_pdfs();
            match self.tab {
                Tab::PdfPages => self.pages.add_paths(pdfs),
                Tab::PdfToImages => self.pdf2img.add_paths(pdfs),
                _ => self.compress.add_paths(pdfs),
            };
        }
        self.note_skipped(&other);
    }

    /// 没认出来的文件：说一声，列几个名字。
    fn note_skipped(&mut self, skipped: &[PathBuf]) {
        if skipped.is_empty() {
            return;
        }
        let names: Vec<String> = skipped
            .iter()
            .take(3)
            .map(|p| tabs::file_label(p))
            .collect();
        let more = if skipped.len() > 3 { " 等" } else { "" };
        self.notice = Some(format!(
            "已跳过 {} 个不支持的文件：{}{more}",
            skipped.len(),
            names.join("、")
        ));
    }

    pub fn busy(&self) -> bool {
        self.job.as_ref().is_some_and(|j| j.is_active())
    }
}

impl eframe::App for App {
    /// 通道排空放在这里而不是 `ui`：窗口最小化时 eframe 不跑 egui pass，
    /// 因而不调 `ui`，但仍会调 `logic`。放错地方的话，用户一最小化，
    /// 长任务就会看起来卡死。
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(job) = &mut self.job {
            job.pump();
        }
        self.thumbs.pump(ctx);
        self.pages.pump(ctx);
        // 图片列表的成员变了才对一遍：新来的排进后台读时间，移走的不再读、纹理释放。
        let files = &self.images.files;
        if files.changes() != self.images_seen {
            self.images_seen = files.changes();
            for p in &files.items {
                if !self.images.times.contains_key(p) && !self.images.no_time.contains(p) {
                    self.times.request(p);
                }
            }
            let keep: HashSet<PathBuf> = files.items.iter().cloned().collect();
            self.times.retain(|p| keep.contains(p));
            self.thumbs.retain(|p| keep.contains(p));
        }
        // 读出来的时间放进缓存，手动填过的不覆盖。
        for (path, time) in self.times.drain() {
            let Some(t) = time else {
                self.images.no_time.insert(path);
                continue;
            };
            let manual = self
                .images
                .times
                .get(&path)
                .is_some_and(|c| c.source == TimeSource::Manual);
            if !manual {
                self.images.times.insert(path, t);
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if !self.first_frame_logged {
            self.first_frame_logged = true;
            log::debug!("启动：开始绘制首帧 {:?}", crate::since_start());
        }
        let ctx = ui.ctx().clone();
        self.handle_dropped_files(&ctx);

        egui::Panel::top("tabs").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                for tab in Tab::ALL {
                    if ui.selectable_label(self.tab == tab, tab.title()).clicked() {
                        self.tab = tab;
                    }
                }
            });
            ui.add_space(4.0);
        });

        egui::Panel::bottom("status").show(ui, |ui| {
            self.status_bar(ui);
        });

        egui::CentralPanel::default().show(ui, |ui| {
            if self.ui_font.is_none() {
                ui.colored_label(
                    crate::theme::palette(ui).caution,
                    "未找到中文字体，界面与导出的 PDF 都可能显示为方块。\
                     请安装 Noto Sans CJK 或思源黑体。",
                );
                ui.separator();
            }
            if let Some(notice) = &self.notice {
                let mut close = false;
                ui.horizontal(|ui| {
                    ui.colored_label(crate::theme::palette(ui).caution, notice);
                    close = ui.small_button("✖").clicked();
                });
                if close {
                    self.notice = None;
                }
            }
            // 列表自己滚动：只画看得见的行，上千个文件也不卡。
            match self.tab {
                Tab::ImagesToPdf => tabs::img2pdf::ui(self, ui),
                Tab::DocxToPdf => tabs::docx2pdf::ui(self, ui),
                Tab::PdfCompress => tabs::pdf_compress::ui(self, ui),
                Tab::ImagesCompress => tabs::img_compress::ui(self, ui),
                Tab::PdfPages => tabs::pdf_pages::ui(self, ui),
                Tab::PdfToImages => tabs::pdf2img::ui(self, ui),
            }
        });
        drop_overlay(&ctx, self.tab);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        prefs::save(self, storage);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // 别让后台线程在进程退出后还往磁盘上写东西。
        if let Some(job) = &mut self.job {
            job.request_cancel();
        }
    }
}

impl App {
    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if !dropped.is_empty() {
            self.open_paths(dropped);
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        // 先把要执行的动作算出来，再动 self —— 否则闭包会和 `&self.job` 抢借用。
        let mut dismiss = false;
        let mut cancel = false;
        let mut action: Option<FileAction> = None;

        let Some(job) = self.job.as_ref() else {
            ui.horizontal(|ui| {
                ui.label("就绪");
                if let Some(font) = &self.ui_font {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.weak(format!("界面字体：{font}"));
                    });
                }
            });
            return;
        };
        let pal = theme::palette(ui);
        ui.add_space(4.0);
        match &job.state {
            State::Running | State::Cancelling => {
                let cancelling = job.state == State::Cancelling;
                ui.horizontal(|ui| {
                    let bar = match job.progress.fraction {
                        Some(f) => egui::ProgressBar::new(f).show_percentage(),
                        None => egui::ProgressBar::new(0.0).animate(true),
                    };
                    ui.add_sized([(ui.available_width() - 90.0).max(60.0), 18.0], bar);
                    if ui
                        .add_enabled(!cancelling, egui::Button::new("取消"))
                        .on_hover_text("Esc")
                        .clicked()
                    {
                        cancel = true;
                    }
                });
                if cancelling {
                    ui.weak("正在取消…");
                } else {
                    ui.weak(&job.progress.text);
                }
            }
            State::Finished(msg) => {
                ui.horizontal(|ui| {
                    ui.colored_label(pal.ok, "✔");
                    ui.label(msg);
                    if let Some(out) = &job.output {
                        if ui.button("在文件夹中显示").clicked() {
                            action = Some(FileAction::Reveal(out.clone()));
                        }
                    }
                    dismiss |= ui.button("知道了").clicked();
                });
            }
            State::Cancelled => {
                ui.horizontal(|ui| {
                    // 取消是用户自己的决定，不是出错，不用红色。
                    let text = if job.planned > 0 {
                        format!("已取消，已完成 {}/{}", job.succeeded(), job.planned)
                    } else {
                        "已取消".to_string()
                    };
                    ui.label(text);
                    dismiss |= ui.button("知道了").clicked();
                });
            }
            State::Failed(msg) => {
                ui.horizontal(|ui| {
                    ui.colored_label(pal.error, "✖");
                    ui.label(msg);
                    dismiss |= ui.button("知道了").clicked();
                });
            }
        }

        // 批量任务逐个文件的结果：哪个成了、落在哪，哪个没成、为什么。
        if !job.items.is_empty() {
            egui::ScrollArea::vertical()
                .max_height(150.0)
                .id_salt("job-items")
                .show(ui, |ui| {
                    for r in &job.items {
                        ui.horizontal(|ui| {
                            match &r.error {
                                None => ui.colored_label(pal.ok, "✔"),
                                Some(_) => ui.colored_label(pal.error, "✖"),
                            };
                            ui.label(tabs::file_label(&r.input));
                            match (&r.error, &r.output) {
                                (Some(e), _) => {
                                    ui.colored_label(pal.error, e);
                                }
                                (None, Some(out)) => {
                                    ui.weak(&r.detail);
                                    if ui.small_button("打开").clicked() {
                                        action = Some(FileAction::Open(out.clone()));
                                    }
                                    if ui.small_button("在文件夹中显示").clicked() {
                                        action = Some(FileAction::Reveal(out.clone()));
                                    }
                                }
                                (None, None) => {
                                    ui.weak(&r.detail);
                                }
                            }
                        });
                    }
                });
        }

        // 任务产生的所有提示都摊开给用户看，不折叠、不省略。
        if !job.warnings.is_empty() {
            egui::ScrollArea::vertical()
                .max_height(140.0)
                .id_salt("job-warnings")
                .show(ui, |ui| warnings_panel(ui, &job.warnings));
        }
        ui.add_space(4.0);

        if cancel || (job.is_active() && ui.input(|i| i.key_pressed(egui::Key::Escape))) {
            if let Some(job) = &mut self.job {
                job.request_cancel();
            }
        }
        if dismiss {
            self.job = None;
        }
        match action {
            Some(FileAction::Open(p)) => open_file(&p),
            Some(FileAction::Reveal(p)) => reveal(&p),
            None => {}
        }
    }
}

enum FileAction {
    Open(std::path::PathBuf),
    Reveal(std::path::PathBuf),
}

/// 转换报告。**必须在保存之前展示**，这样用户发现丢了东西还来得及放弃。
pub fn warnings_panel(ui: &mut egui::Ui, warnings: &[Warning]) {
    if warnings.is_empty() {
        return;
    }
    let unsupported = warnings
        .iter()
        .filter(|w| w.kind == WarningKind::UnsupportedElement)
        .count();
    let title = if unsupported > 0 {
        format!(
            "本次转换有 {unsupported} 处内容未能完整还原（共 {} 条提示）",
            warnings.len()
        )
    } else {
        format!("{} 条提示", warnings.len())
    };

    let pal = theme::palette(ui);
    egui::Frame::new()
        .fill(pal.note_bg)
        .inner_margin(8.0)
        .corner_radius(4.0)
        .show(ui, |ui| {
            ui.colored_label(pal.note_fg, egui::RichText::new(title).strong());
            for w in warnings {
                let line = match w.page {
                    Some(p) => format!("· 第 {p} 页：{}", w.detail),
                    None => format!("· {}", w.detail),
                };
                ui.colored_label(pal.note_fg, line);
            }
        });
}

/// 档位选择器。四个功能共用。
pub fn tier_selector(ui: &mut egui::Ui, tier: &mut Tier, enabled: bool) {
    ui.horizontal(|ui| {
        ui.label("质量档位：");
        ui.add_enabled_ui(enabled, |ui| {
            for t in Tier::ALL {
                if ui
                    .selectable_label(*tier == t, t.label())
                    .on_hover_text(t.description())
                    .clicked()
                {
                    *tier = t;
                }
            }
        });
    });
    ui.weak(tier.description());
}

/// 文件拖到窗口上方还没松开时，盖一层说明：松开就添加，按类型分到各页。
fn drop_overlay(ctx: &egui::Context, here: Tab) {
    if ctx.input(|i| i.raw.hovered_files.is_empty()) {
        return;
    }
    let rect = ctx.content_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("drop-overlay"),
    ));
    painter.rect_filled(rect, 0.0, egui::Color32::from_black_alpha(170));
    let text = format!(
        "松开以添加\n图片 → {}　　Word → Word 转 PDF　　PDF → {}\n文件夹会展开成里面的文件",
        here.for_images().title(),
        here.for_pdfs().title()
    );
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(20.0),
        egui::Color32::WHITE,
    );
}

/// 文件夹展开成里面的文件（递归，各层按文件名自然排序）；文件原样。展开得太多
/// （一次拖进整个磁盘）就停在上限，免得界面卡死。
fn expand_folders(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    const MAX_FILES: usize = 10_000;
    const MAX_DEPTH: usize = 8;
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        entries.sort_by(|a, b| tabs::common::natural_cmp(a, b));
        for p in entries {
            if out.len() >= MAX_FILES {
                return;
            }
            // 隐藏文件（.DS_Store 之类）不算。
            if p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with('.'))
            {
                continue;
            }
            if p.is_dir() {
                if depth < MAX_DEPTH {
                    walk(&p, depth + 1, out);
                }
            } else {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            walk(&p, 1, &mut out);
        } else {
            out.push(p);
        }
    }
    out
}

/// 用系统默认的程序打开。
fn open_file(path: &std::path::Path) {
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

/// 在文件管理器里显示：Windows、macOS 上选中这个文件，Linux 上打开它所在的文件夹
/// （各家文件管理器选中文件的办法不统一）。
fn reveal(path: &std::path::Path) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // explorer 要的是 `/select,"C:\a b\c.pdf"` 这一整段，不能让标准库替它整体加引号。
        let _ = std::process::Command::new("explorer")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn();
    }
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open")
        .arg(path.parent().unwrap_or(path))
        .spawn();
}
