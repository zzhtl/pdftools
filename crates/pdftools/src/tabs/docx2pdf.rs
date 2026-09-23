//! Word 转 PDF。

use std::path::PathBuf;

use pdfcore::ops::docx_to_pdf;
use pdfcore::{Progress, ProgressSink};

use crate::app::App;
use crate::job::{human_size, run_batch, write_atomic, Done, Job};

use super::common::{self, FileList, WORD_EXTENSIONS};
use super::{file_label, FOOTER};

#[derive(Default)]
pub struct State {
    pub files: FileList,
    /// 上次添加文档、保存 PDF 的文件夹。
    pub open_dir: Option<PathBuf>,
    pub out_dir: Option<PathBuf>,
}

impl State {
    /// 返回不认的文件有几个。`.doc` 也收下：转换时会明确告诉用户先另存为 `.docx`。
    pub fn add_paths(&mut self, paths: Vec<PathBuf>) -> usize {
        self.files.add(paths, |p| {
            common::has_extension(p, WORD_EXTENSIONS) || common::has_extension(p, &["doc"])
        })
    }
}

pub fn ui(app: &mut App, ui: &mut egui::Ui) {
    let busy = app.busy();
    let ctx = ui.ctx().clone();

    ui.horizontal(|ui| {
        if ui
            .button("添加 Word 文档…")
            .on_hover_text("Ctrl+O")
            .clicked()
            || common::open_shortcut(ui)
        {
            if let Some(picked) =
                common::pick_files(&mut app.docx.open_dir, "Word 文档", WORD_EXTENSIONS)
            {
                app.docx.add_paths(picked);
            }
        }
        if ui.button("按文件名排序").clicked() {
            app.docx.files.sort_by_name();
        }
        if ui.button("清空").clicked() {
            app.docx.files.clear();
        }
    });
    ui.weak("把 .docx 或装着它们的文件夹拖进窗口也可以添加。");

    ui.add_space(4.0);
    egui::CollapsingHeader::new("支持范围（请先看这里）")
        .default_open(false)
        .show(ui, |ui| {
            ui.label(
                "本程序用纯 Rust 自己排版，不依赖 Word 或 LibreOffice，因此保真度有明确边界：",
            );
            ui.label("✔ 支持：字符与段落格式、样式、行距与行网格、制表位、自动编号、表格（合并单元格、跨页）、图片、文本框与直线、页眉页脚与页码、多节、中西文混排");
            ui.label("✖ 近似或不支持：脚注尾注、分栏（按单栏排）、文字环绕（按上下型排）、组合图形与图表（画成灰框）、竖排文字");
            ui.colored_label(
                crate::theme::palette(ui).note_fg,
                "遇到排不了或只能近似的内容时，程序会在结果里列出，不会悄悄丢掉。",
            );
        });
    ui.separator();

    if app.docx.files.items.is_empty() {
        ui.weak("还没有添加文档。");
        return;
    }

    let height = ui.available_height() - FOOTER;
    common::file_list(
        ui,
        "docx",
        &mut app.docx.files,
        height,
        24.0,
        |ui, _i, path| {
            ui.label(file_label(path));
        },
    );

    ui.separator();
    let single = app.docx.files.items.len() == 1;
    let label = if single {
        "转换为 PDF…"
    } else {
        "批量转换到文件夹…"
    };
    if common::run_button(ui, label, !busy) {
        start(app, &ctx, single);
    }
}

fn start(app: &mut App, ctx: &egui::Context, single: bool) {
    let files = app.docx.files.items.clone();

    // 单个文件让用户直接决定文件名；多个文件只问目录，输出名沿用原名。
    let target: Target = if single {
        let default = file_label(&files[0].with_extension("pdf"));
        match common::pick_save(&mut app.docx.out_dir, &default, "PDF", &["pdf"]) {
            Some(out) => Target::File(out),
            None => return,
        }
    } else {
        match common::pick_folder(&mut app.docx.out_dir) {
            Some(dir) => Target::Dir(dir),
            None => return,
        }
    };

    app.job = Some(Job::spawn(ctx, files.len(), move |worker| {
        let mut namer = pdfcore::fsio::OutputNamer::new(&files);
        if let Target::File(p) = &target {
            if namer.is_input(p) {
                return Err("输出文件不能是原文档本身，请换一个文件名".into());
            }
        }
        let (mut pages, mut bytes) = (0usize, 0usize);
        let batch = run_batch(worker, &files, |path, sink| {
            let report = docx_to_pdf::run(path, sink)?;
            for w in report.warnings {
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
            let detail = format!(
                "{} 页，{}",
                report.value.pages,
                human_size(report.value.pdf.len() as u64)
            );
            Ok((out, detail))
        })?;

        let mut summary = format!(
            "已转换 {} 个文档，共 {pages} 页，{}",
            batch.succeeded,
            human_size(bytes as u64)
        );
        if batch.failed > 0 {
            summary += &format!("；{} 个没有转成", batch.failed);
        }
        Ok(Done {
            summary,
            output: batch.last_output,
        })
    }));
}

enum Target {
    File(PathBuf),
    Dir(PathBuf),
}
