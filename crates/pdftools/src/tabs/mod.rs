pub mod docx2pdf;
pub mod img2pdf;
pub mod img_compress;
pub mod pdf_compress;

use std::path::PathBuf;

/// 四个 Tab 共用的文件列表编辑：去重、拖拽重排、删除。
#[derive(Default)]
pub struct FileList {
    pub items: Vec<PathBuf>,
}

impl FileList {
    pub fn add(
        &mut self,
        paths: impl IntoIterator<Item = PathBuf>,
        accept: impl Fn(&std::path::Path) -> bool,
    ) {
        for p in paths {
            if accept(&p) && !self.items.contains(&p) {
                self.items.push(p);
            }
        }
    }

    pub fn move_item(&mut self, from: usize, to: usize) {
        if from >= self.items.len() || from == to {
            return;
        }
        let item = self.items.remove(from);
        // 移除之后，落点在原位置之后的话下标要回退一格。
        let to = if to > from { to - 1 } else { to };
        self.items.insert(to.min(self.items.len()), item);
    }

    /// 按文件名自然排序：`第2章` 要排在 `第10章` 前面，
    /// 而字典序会把 `10` 排到 `2` 前面 —— 对「照片 1.jpg ... 照片 10.jpg」这种命名尤其致命。
    pub fn sort_by_name(&mut self) {
        self.items.sort_by(|a, b| {
            natural_key(&a.file_name().unwrap_or_default().to_string_lossy()).cmp(&natural_key(
                &b.file_name().unwrap_or_default().to_string_lossy(),
            ))
        });
    }

    pub fn sort_by_mtime(&mut self) {
        self.items.sort_by_key(|p| {
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
        });
    }
}

/// 把字符串切成「非数字片段 / 数字」交替的序列，数字按数值比较。
fn natural_key(s: &str) -> Vec<Chunk> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            let mut n = 0u64;
            while let Some(&d) = chars.peek() {
                let Some(v) = d.to_digit(10) else { break };
                n = n.saturating_mul(10).saturating_add(v as u64);
                chars.next();
            }
            out.push(Chunk::Num(n));
        } else {
            let mut t = String::new();
            while let Some(&d) = chars.peek() {
                if d.is_ascii_digit() {
                    break;
                }
                t.push(d.to_lowercase().next().unwrap_or(d));
                chars.next();
            }
            out.push(Chunk::Text(t));
        }
    }
    out
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub enum Chunk {
    Text(String),
    Num(u64),
}

/// 带拖拽重排的文件列表。返回 true 表示列表被改动过。
pub fn draggable_list(
    ui: &mut egui::Ui,
    id_salt: &str,
    list: &mut FileList,
    enabled: bool,
    mut row: impl FnMut(&mut egui::Ui, usize, &PathBuf),
) -> bool {
    let mut remove: Option<usize> = None;
    let mut drag: Option<(usize, usize)> = None;

    for i in 0..list.items.len() {
        let path = list.items[i].clone();
        let id = egui::Id::new((id_salt, i));

        let resp = ui
            .dnd_drag_source(id, i, |ui| {
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(enabled, |ui| {
                        if ui.small_button("✖").on_hover_text("移出列表").clicked() {
                            remove = Some(i);
                        }
                    });
                    ui.label(format!("{:>3}.", i + 1));
                    row(ui, i, &path);
                });
            })
            .response;

        // 拖拽经过时画一条插入位置指示线，不然用户不知道会插到哪。
        if let Some(payload) = resp.dnd_hover_payload::<usize>() {
            let rect = resp.rect;
            let pointer_y = ui.input(|s| s.pointer.interact_pos()).map(|p| p.y);
            let before = pointer_y.is_none_or(|y| y < rect.center().y);
            let line_y = if before { rect.top() } else { rect.bottom() };
            ui.painter().hline(
                rect.x_range(),
                line_y,
                egui::Stroke::new(2.0, ui.visuals().selection.bg_fill),
            );
            if resp.dnd_release_payload::<usize>().is_some() {
                drag = Some((*payload, if before { i } else { i + 1 }));
            }
        }
    }

    let mut changed = false;
    if let Some(i) = remove {
        list.items.remove(i);
        changed = true;
    }
    if let Some((from, to)) = drag {
        list.move_item(from, to);
        changed = true;
    }
    changed
}

pub fn file_label(path: &std::path::Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}
