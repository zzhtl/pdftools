//! 四个页签共用的部件：文件列表、添加文件的对话框、开始按钮。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// 各类输入文件的扩展名，对话框的过滤器与拖进来的文件都按它认。
pub const WORD_EXTENSIONS: &[&str] = &["docx"];
pub const PDF_EXTENSIONS: &[&str] = &["pdf"];

pub fn has_extension(path: &Path, exts: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| exts.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// 一个页签的文件列表：去重、排序、选中。
#[derive(Default)]
pub struct FileList {
    pub items: Vec<PathBuf>,
    /// 选中的文件。按路径记：列表重排、删除之后选中的还是那几个文件。
    selected: HashSet<PathBuf>,
    /// Shift 连选的起点。
    anchor: Option<usize>,
    /// 成员每变一次（增、删、清空；排序不算）就加一。跟着列表走的后台活（读时间、
    /// 缩略图）靠它判断要不要重新对一遍，不必每帧把上千个路径过一遍。
    changes: u64,
}

impl FileList {
    /// 加进来，`accept` 不认的与已经在列表里的跳过。返回跳过了几个不认的。
    pub fn add(
        &mut self,
        paths: impl IntoIterator<Item = PathBuf>,
        accept: impl Fn(&Path) -> bool,
    ) -> usize {
        // 查重用集合：一次拖进上万个文件时，逐个在列表里线性查找要卡上好几秒。
        let mut have: HashSet<PathBuf> = self.items.iter().cloned().collect();
        let before = self.items.len();
        let mut rejected = 0;
        for p in paths {
            if !accept(&p) {
                rejected += 1;
            } else if have.insert(p.clone()) {
                self.items.push(p);
            }
        }
        if self.items.len() != before {
            self.changes += 1;
        }
        rejected
    }

    pub fn changes(&self) -> u64 {
        self.changes
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.selected.clear();
        self.anchor = None;
        self.changes += 1;
    }

    pub fn move_item(&mut self, from: usize, to: usize) {
        if from >= self.items.len() || from == to {
            return;
        }
        let item = self.items.remove(from);
        // 移除之后，落点在原位置之后的话下标要回退一格。
        let to = if to > from { to - 1 } else { to };
        self.items.insert(to.min(self.items.len()), item);
        self.anchor = None;
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.items.len() {
            let p = self.items.remove(index);
            self.selected.remove(&p);
            self.anchor = None;
            self.changes += 1;
        }
    }

    /// 删掉选中的。返回删了几个。
    pub fn remove_selected(&mut self) -> usize {
        let before = self.items.len();
        let selected = std::mem::take(&mut self.selected);
        self.items.retain(|p| !selected.contains(p));
        self.anchor = None;
        if self.items.len() != before {
            self.changes += 1;
        }
        before - self.items.len()
    }

    pub fn is_selected(&self, index: usize) -> bool {
        self.items
            .get(index)
            .is_some_and(|p| self.selected.contains(p))
    }

    /// 点了第 `index` 行：单击只选它，Ctrl（macOS 上 ⌘）单击加选或取消，Shift 单击
    /// 从上次点的那行连选到这行。
    pub fn click(&mut self, index: usize, toggle: bool, range: bool) {
        let Some(path) = self.items.get(index).cloned() else {
            return;
        };
        match (range, self.anchor) {
            (true, Some(a)) => {
                let (lo, hi) = (a.min(index), a.max(index).min(self.items.len() - 1));
                if !toggle {
                    self.selected.clear();
                }
                self.selected.extend(self.items[lo..=hi].iter().cloned());
            }
            _ if toggle => {
                if !self.selected.remove(&path) {
                    self.selected.insert(path);
                }
                self.anchor = Some(index);
            }
            _ => {
                self.selected.clear();
                self.selected.insert(path);
                self.anchor = Some(index);
            }
        }
    }

    /// 按文件名自然排序：`第2章` 要排在 `第10章` 前面，
    /// 而字典序会把 `10` 排到 `2` 前面 —— 对「照片 1.jpg ... 照片 10.jpg」这种命名尤其致命。
    pub fn sort_by_name(&mut self) {
        self.items.sort_by(|a, b| natural_cmp(a, b));
        self.anchor = None;
    }
}

/// 按文件名自然顺序比较（「2」在「10」前面）。
pub fn natural_cmp(a: &Path, b: &Path) -> std::cmp::Ordering {
    let key = |p: &Path| natural_key(&p.file_name().unwrap_or_default().to_string_lossy());
    key(a).cmp(&key(b))
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
enum Chunk {
    Text(String),
    Num(u64),
}

/// 画文件列表：只画看得见的那些行，上千个文件也不卡。每行前面是序号（点它来选），
/// 可以拖动排序，✖ 移出列表；有选中的行时按 Delete 删掉它们。`height` 是列表可用的
/// 高度，`row_height` 是每行的高度（行必须一样高）。`row` 画每行其余的内容。
pub fn file_list(
    ui: &mut egui::Ui,
    id_salt: &str,
    list: &mut FileList,
    height: f32,
    row_height: f32,
    mut row: impl FnMut(&mut egui::Ui, usize, &PathBuf),
) {
    let mut remove: Option<usize> = None;
    let mut drag: Option<(usize, usize)> = None;
    let mut clicked: Option<(usize, bool, bool)> = None;
    let modifiers = ui.input(|i| i.modifiers);

    egui::ScrollArea::vertical()
        .id_salt(id_salt)
        .max_height(height.max(row_height * 3.0))
        .auto_shrink([false, true])
        .show_rows(ui, row_height, list.items.len(), |ui, range| {
            for i in range {
                let path = list.items[i].clone();
                let selected = list.is_selected(i);
                let resp = ui
                    .dnd_drag_source(egui::Id::new((id_salt, "row", i)), i, |ui| {
                        // 每行一样高：只画看得见的行，靠的就是按行高算出哪些行在视口里。
                        let size = egui::vec2(ui.available_width(), row_height);
                        let layout = egui::Layout::left_to_right(egui::Align::Center);
                        ui.allocate_ui_with_layout(size, layout, |ui| {
                            ui.set_height(row_height);
                            if ui.small_button("✖").on_hover_text("移出列表").clicked() {
                                remove = Some(i);
                            }
                            let index = egui::Button::selectable(selected, format!("{:>3}", i + 1))
                                .min_size(egui::vec2(34.0, 0.0));
                            if ui
                                .add(index)
                                .on_hover_text("点选；Ctrl 加选，Shift 连选，Delete 删除选中的")
                                .clicked()
                            {
                                clicked = Some((i, modifiers.command, modifiers.shift));
                            }
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
        });

    if let Some(i) = remove {
        list.remove(i);
    }
    if let Some((from, to)) = drag {
        list.move_item(from, to);
    }
    if let Some((i, toggle, range)) = clicked {
        list.click(i, toggle, range);
    }
    // 输入框有焦点时 Delete 是删字，不是删文件。
    let typing = ui.ctx().memory(|m| m.focused().is_some());
    if !typing
        && ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace))
    {
        list.remove_selected();
    }
}

/// 「添加…」对话框：记住上次打开的文件夹。`remember` 是这个页签上次的文件夹。
pub fn pick_files(
    remember: &mut Option<PathBuf>,
    filter: &str,
    exts: &[&str],
) -> Option<Vec<PathBuf>> {
    let dialog = start_in(rfd::FileDialog::new().add_filter(filter, exts), remember);
    let picked = dialog.pick_files()?;
    *remember = picked
        .first()
        .and_then(|p| p.parent())
        .map(Path::to_path_buf);
    Some(picked)
}

/// 记住的文件夹可能是上次启动时的，已经删了、U 盘也拔了：不在了就让系统对话框
/// 自己挑起点，别指望各平台对不存在的起点处理得一样。
fn start_in(dialog: rfd::FileDialog, remember: &Option<PathBuf>) -> rfd::FileDialog {
    match remember.as_deref().filter(|d| d.is_dir()) {
        Some(dir) => dialog.set_directory(dir),
        None => dialog,
    }
}

/// 选输出的文件夹，记住上次的。
pub fn pick_folder(remember: &mut Option<PathBuf>) -> Option<PathBuf> {
    let dir = start_in(rfd::FileDialog::new(), remember).pick_folder()?;
    *remember = Some(dir.clone());
    Some(dir)
}

/// 选输出的文件（另存为），记住上次的文件夹。
pub fn pick_save(
    remember: &mut Option<PathBuf>,
    default_name: &str,
    filter: &str,
    exts: &[&str],
) -> Option<PathBuf> {
    let dialog = rfd::FileDialog::new()
        .add_filter(filter, exts)
        .set_file_name(default_name);
    let out = start_in(dialog, remember).save_file()?;
    *remember = out.parent().map(Path::to_path_buf);
    Some(out)
}

/// 页签底部的开始按钮。也响应 Enter（输入框没有焦点时）。
pub fn run_button(ui: &mut egui::Ui, label: &str, enabled: bool) -> bool {
    let typing = ui.ctx().memory(|m| m.focused().is_some());
    let enter = enabled && !typing && ui.input(|i| i.key_pressed(egui::Key::Enter));
    let clicked = ui
        .add_enabled(
            enabled,
            egui::Button::new(label).min_size(egui::vec2(140.0, 28.0)),
        )
        .on_hover_text("Enter")
        .clicked();
    clicked || enter
}

/// Ctrl+O（macOS 上 ⌘O）：打开当前页签的「添加…」对话框。
pub fn open_shortcut(ui: &egui::Ui) -> bool {
    ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::O))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(names: &[&str]) -> FileList {
        let mut l = FileList::default();
        l.add(names.iter().map(PathBuf::from), |_| true);
        l
    }

    fn names(l: &FileList) -> Vec<String> {
        l.items
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn natural_order_puts_2_before_10() {
        let mut l = list(&["照片 10.jpg", "照片 2.jpg", "照片 1.jpg", "b.jpg", "A.jpg"]);
        l.sort_by_name();
        assert_eq!(
            names(&l),
            ["A.jpg", "b.jpg", "照片 1.jpg", "照片 2.jpg", "照片 10.jpg"]
        );
    }

    #[test]
    fn duplicates_and_rejected_files_are_skipped() {
        let mut l = list(&["a.pdf"]);
        let rejected = l.add(["a.pdf", "b.pdf", "c.txt"].iter().map(PathBuf::from), |p| {
            has_extension(p, PDF_EXTENSIONS)
        });
        assert_eq!(rejected, 1);
        assert_eq!(names(&l), ["a.pdf", "b.pdf"]);
    }

    /// 增、删、清空才算成员变了；排序、挪位置、加进来的全是重复的都不算。
    #[test]
    fn only_membership_changes_are_counted() {
        let mut l = list(&["b", "a", "b"]);
        assert_eq!(names(&l), ["b", "a"]);
        let mut seen = l.changes();
        let mut changed = |l: &FileList| {
            let c = l.changes() != seen;
            seen = l.changes();
            c
        };
        l.add([PathBuf::from("a")], |_| true);
        l.sort_by_name();
        l.move_item(0, 2);
        l.click(0, false, false);
        assert!(!changed(&l));
        l.add([PathBuf::from("c")], |_| true);
        assert!(changed(&l));
        l.remove(0);
        assert!(changed(&l));
        l.click(0, false, false);
        l.remove_selected();
        assert!(changed(&l));
        l.remove_selected();
        assert!(!changed(&l));
        l.clear();
        assert!(changed(&l));
    }

    /// 单击只选一个，Ctrl 加选，Shift 连选；删掉选中的以后列表与选中都对。
    #[test]
    fn selecting_and_deleting() {
        let mut l = list(&["a", "b", "c", "d", "e"]);
        l.click(1, false, false);
        l.click(3, false, true);
        assert_eq!(
            (0..5).map(|i| l.is_selected(i)).collect::<Vec<_>>(),
            [false, true, true, true, false]
        );
        l.click(2, true, false);
        assert!(!l.is_selected(2));
        l.click(0, false, false);
        l.click(4, true, false);
        assert_eq!(l.remove_selected(), 2);
        assert_eq!(names(&l), ["b", "c", "d"]);
        assert!((0..3).all(|i| !l.is_selected(i)));
    }

    /// 选中的跟着文件走：拖动排序以后还是那个文件被选中。
    #[test]
    fn selection_follows_the_file_when_reordered() {
        let mut l = list(&["a", "b", "c"]);
        l.click(0, false, false);
        l.move_item(0, 3);
        assert_eq!(names(&l), ["b", "c", "a"]);
        assert!(l.is_selected(2) && !l.is_selected(0));
    }
}
