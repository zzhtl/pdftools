//! PDF 压缩。

use std::path::PathBuf;

use pdfcore::imaging::Tier;
use pdfcore::ops::pdf_compress as compress;
use pdfcore::{Progress, ProgressSink};

use crate::app::{tier_selector, App};
use crate::job::{human_size, run_batch, write_atomic, Done, Job};

use super::common::{self, FileList, PDF_EXTENSIONS};
use super::{file_label, FOOTER};

pub struct State {
    pub files: FileList,
    pub tier: Tier,
    pub grayscale: bool,
    /// 上次添加 PDF、保存结果的文件夹。
    pub open_dir: Option<PathBuf>,
    pub out_dir: Option<PathBuf>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            files: FileList::default(),
            tier: Tier::Balanced,
            grayscale: false,
            open_dir: None,
            out_dir: None,
        }
    }
}

impl State {
    /// 返回不认的文件有几个。
    pub fn add_paths(&mut self, paths: Vec<PathBuf>) -> usize {
        self.files
            .add(paths, |p| common::has_extension(p, PDF_EXTENSIONS))
    }
}

pub fn ui(app: &mut App, ui: &mut egui::Ui) {
    let busy = app.busy();
    let ctx = ui.ctx().clone();

    ui.horizontal(|ui| {
        if ui.button("添加 PDF…").on_hover_text("Ctrl+O").clicked() || common::open_shortcut(ui)
        {
            if let Some(picked) =
                common::pick_files(&mut app.compress.open_dir, "PDF", PDF_EXTENSIONS)
            {
                app.compress.add_paths(picked);
            }
        }
        if ui.button("清空").clicked() {
            app.compress.files.clear();
        }
    });
    ui.weak("把 PDF 或装着它们的文件夹拖进窗口也可以添加。");

    ui.add_space(4.0);
    ui.colored_label(
        crate::theme::palette(ui).note_fg,
        "这四个档位压缩的是 PDF 里的图像。以文字/矢量为主的 PDF（Word 导出的那种）\
         只能省下结构开销，通常 0-5%，这是正常的。",
    );
    tier_selector(ui, &mut app.compress.tier, true);
    ui.checkbox(&mut app.compress.grayscale, "转为灰度")
        .on_hover_text(
            "会丢失所有颜色。扫描件上的红色公章、签名笔迹都会变灰，\
             法律文书慎用。默认关闭。",
        );
    ui.separator();

    if app.compress.files.items.is_empty() {
        ui.weak("还没有添加 PDF。");
        return;
    }

    let height = ui.available_height() - FOOTER;
    common::file_list(
        ui,
        "pdfc",
        &mut app.compress.files,
        height,
        24.0,
        |ui, _i, path| {
            ui.label(file_label(path));
            if let Ok(m) = std::fs::metadata(path) {
                ui.weak(human_size(m.len()));
            }
        },
    );

    ui.separator();
    if common::run_button(ui, "压缩到文件夹…", !busy) {
        start(app, &ctx);
    }
}

fn start(app: &mut App, ctx: &egui::Context) {
    let Some(dir) = common::pick_folder(&mut app.compress.out_dir) else {
        return;
    };
    let files = app.compress.files.items.clone();
    let tier = app.compress.tier;
    let grayscale = app.compress.grayscale;

    app.job = Some(Job::spawn(ctx, files.len(), move |worker| {
        let (mut before, mut after) = (0u64, 0u64);
        // 输出目录选成源目录是很自然的事，那时同名输出会覆盖原件 —— 一律自动改名。
        let mut namer = pdfcore::fsio::OutputNamer::new(&files);
        let batch = run_batch(worker, &files, |path, sink| {
            let data = std::fs::read(path).map_err(|e| e.to_string())?;
            let report = compress::run(&data, tier, grayscale, sink)?;
            for w in report.warnings {
                sink.emit(Progress::Warn(w));
            }
            if report.value.is_mostly_vector() {
                sink.emit(Progress::Warn(pdfcore::Warning::new(
                    pdfcore::WarningKind::ImageKeptOriginal,
                    "这份 PDF 以文字/矢量为主，几乎没有可压缩的图像",
                )));
            }
            let out = namer.name(
                &dir,
                &path.file_stem().unwrap_or_default().to_string_lossy(),
                "pdf",
            );
            write_atomic(&out, &report.value.pdf)?;
            before += data.len() as u64;
            after += report.value.pdf.len() as u64;
            let detail = format!(
                "{} → {}（{:.0}%）",
                human_size(data.len() as u64),
                human_size(report.value.pdf.len() as u64),
                report.value.saved_ratio() * 100.0
            );
            Ok((out, detail))
        })?;

        let saved = if before > 0 {
            100.0 * (1.0 - after as f32 / before as f32)
        } else {
            0.0
        };
        let mut summary = format!(
            "{} 个文件：{} → {}（{saved:.0}%）",
            batch.succeeded,
            human_size(before),
            human_size(after)
        );
        if batch.failed > 0 {
            summary += &format!("；{} 个没有压成", batch.failed);
        }
        Ok(Done {
            summary,
            output: batch.last_output,
        })
    }));
}
