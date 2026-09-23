//! Word 转 PDF。

use std::path::PathBuf;

use pdfcore::ops::docx_to_pdf;
use pdfcore::Progress;

use crate::app::App;
use crate::job::{human_size, write_atomic, Done, Job};

use super::{draggable_list, file_label, FileList};

#[derive(Default)]
pub struct State {
    pub files: FileList,
}

impl State {
    pub fn add_paths(&mut self, paths: Vec<PathBuf>) {
        self.files.add(paths, |p| {
            matches!(
                p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_ascii_lowercase())
                    .as_deref(),
                Some("docx" | "doc")
            )
        });
    }
}

pub fn ui(app: &mut App, ui: &mut egui::Ui) {
    let busy = app.busy();
    let ctx = ui.ctx().clone();

    ui.horizontal(|ui| {
        ui.add_enabled_ui(!busy, |ui| {
            if ui.button("添加 Word 文档…").clicked() {
                if let Some(picked) = rfd::FileDialog::new()
                    .add_filter("Word 文档", &["docx"])
                    .pick_files()
                {
                    app.docx.add_paths(picked);
                }
            }
            if ui.button("清空").clicked() {
                app.docx.files.items.clear();
            }
        });
    });
    ui.weak("把 .docx 拖进窗口也可以添加。");

    ui.add_space(4.0);
    egui::CollapsingHeader::new("支持范围（请先看这里）")
        .default_open(false)
        .show(ui, |ui| {
            ui.label(
                "本程序用纯 Rust 自己排版，不依赖 Word 或 LibreOffice，因此保真度有明确边界：",
            );
            ui.label("✔ 支持：段落、粗体/斜体/下划线/删除线、字号、颜色、对齐、缩进、行距、页面尺寸与页边距、分页符、中西文混排");
            ui.label("✖ 不渲染：表格线（但会保留单元格文字）、图片、文本框、分栏、页眉页脚、脚注、页码域、多级编号");
            ui.colored_label(
                egui::Color32::from_rgb(0x8D, 0x6E, 0x00),
                "遇到不渲染的内容时，程序会在结果里逐条列出，不会悄悄丢掉。",
            );
        });
    ui.separator();

    if app.docx.files.items.is_empty() {
        ui.weak("还没有添加文档。");
        return;
    }

    draggable_list(ui, "docx", &mut app.docx.files, !busy, |ui, _i, path| {
        ui.label(file_label(path));
    });

    ui.separator();
    let can_run = !busy && !app.docx.files.items.is_empty();
    let single = app.docx.files.items.len() == 1;
    let label = if single {
        "转换为 PDF…"
    } else {
        "批量转换到文件夹…"
    };
    if ui.add_enabled(can_run, egui::Button::new(label)).clicked() {
        start(app, &ctx, single);
    }
}

fn start(app: &mut App, ctx: &egui::Context, single: bool) {
    let files = app.docx.files.items.clone();

    // 单个文件让用户直接决定文件名；多个文件只问目录，输出名沿用原名。
    let target: Target = if single {
        let default = files[0].with_extension("pdf");
        let Some(out) = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .set_file_name(file_label(&default))
            .save_file()
        else {
            return;
        };
        Target::File(out)
    } else {
        let Some(dir) = rfd::FileDialog::new().pick_folder() else {
            return;
        };
        Target::Dir(dir)
    };

    app.job = Some(Job::spawn(ctx, move |sink| {
        let mut pages = 0usize;
        let mut bytes = 0usize;
        let mut last_out = None;
        let mut namer = pdfcore::fsio::OutputNamer::new(&files);
        if let Target::File(p) = &target {
            if namer.is_input(p) {
                return Err("输出文件不能是原文档本身，请换一个文件名".into());
            }
        }

        for (i, path) in files.iter().enumerate() {
            if sink.is_cancelled() {
                return Err("已取消".into());
            }
            sink.emit(Progress::Item {
                done: i,
                total: files.len(),
                label: file_label(path),
            });

            let report =
                docx_to_pdf::run(path, sink).map_err(|e| format!("{}：{e}", file_label(path)))?;
            for w in report.warnings {
                // 批量转换时，警告要带上是哪个文件的。
                let mut w = w;
                w.detail = format!("{}：{}", file_label(path), w.detail);
                sink.emit(Progress::Warn(w));
            }

            let out = match &target {
                Target::File(p) => p.clone(),
                // 批量输出不覆盖任何东西：不同目录下的同名文档、目录里原有的 PDF 都会自动改名。
                Target::Dir(d) => namer.name(
                    d,
                    &path.file_stem().unwrap_or_default().to_string_lossy(),
                    "pdf",
                ),
            };
            write_atomic(&out, &report.value.pdf)?;

            pages += report.value.pages;
            bytes += report.value.pdf.len();
            last_out = Some(out);
        }

        Ok(Done {
            summary: format!(
                "已转换 {} 个文档，共 {pages} 页，{}",
                files.len(),
                human_size(bytes as u64)
            ),
            output: last_out,
        })
    }));
}

enum Target {
    File(PathBuf),
    Dir(PathBuf),
}
