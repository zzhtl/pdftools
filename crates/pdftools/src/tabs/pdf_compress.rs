//! PDF 压缩。

use std::path::PathBuf;

use pdfcore::imaging::Tier;
use pdfcore::ops::pdf_compress as compress;
use pdfcore::Progress;

use crate::app::{tier_selector, App};
use crate::job::{human_size, write_atomic, Done, Job};

use super::{draggable_list, file_label, FileList};

pub struct State {
    pub files: FileList,
    pub tier: Tier,
    pub grayscale: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            files: FileList::default(),
            tier: Tier::Balanced,
            grayscale: false,
        }
    }
}

impl State {
    pub fn add_paths(&mut self, paths: Vec<PathBuf>) {
        self.files.add(paths, |p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
        });
    }
}

pub fn ui(app: &mut App, ui: &mut egui::Ui) {
    let busy = app.busy();
    let ctx = ui.ctx().clone();

    ui.horizontal(|ui| {
        ui.add_enabled_ui(!busy, |ui| {
            if ui.button("添加 PDF…").clicked() {
                if let Some(picked) = rfd::FileDialog::new()
                    .add_filter("PDF", &["pdf"])
                    .pick_files()
                {
                    app.compress.add_paths(picked);
                }
            }
            if ui.button("清空").clicked() {
                app.compress.files.items.clear();
            }
        });
    });
    ui.weak("把 PDF 拖进窗口也可以添加。");

    ui.add_space(4.0);
    ui.colored_label(
        egui::Color32::from_rgb(0x8D, 0x6E, 0x00),
        "这四个档位压缩的是 PDF 里的图像。以文字/矢量为主的 PDF（Word 导出的那种）\
         只能省下结构开销，通常 0-5%，这是正常的。",
    );
    ui.separator();

    if app.compress.files.items.is_empty() {
        ui.weak("还没有添加 PDF。");
        return;
    }

    draggable_list(
        ui,
        "pdfc",
        &mut app.compress.files,
        !busy,
        |ui, _i, path| {
            ui.label(file_label(path));
            if let Ok(m) = std::fs::metadata(path) {
                ui.weak(human_size(m.len()));
            }
        },
    );

    ui.separator();
    tier_selector(ui, &mut app.compress.tier, !busy);
    ui.add_enabled_ui(!busy, |ui| {
        ui.checkbox(&mut app.compress.grayscale, "转为灰度")
            .on_hover_text(
                "会丢失所有颜色。扫描件上的红色公章、签名笔迹都会变灰，\
                 法律文书慎用。默认关闭。",
            );
    });

    ui.add_space(6.0);
    let can_run = !busy && !app.compress.files.items.is_empty();
    if ui
        .add_enabled(can_run, egui::Button::new("压缩到文件夹…"))
        .clicked()
    {
        start(app, &ctx);
    }
}

fn start(app: &mut App, ctx: &egui::Context) {
    let Some(dir) = rfd::FileDialog::new().pick_folder() else {
        return;
    };
    let files = app.compress.files.items.clone();
    let tier = app.compress.tier;
    let grayscale = app.compress.grayscale;

    app.job = Some(Job::spawn(ctx, move |sink| {
        let (mut before, mut after) = (0u64, 0u64);
        let mut last_out = None;
        // 输出目录选成源目录是很自然的事，那时同名输出会覆盖原件 —— 一律自动改名。
        let mut namer = pdfcore::fsio::OutputNamer::new(&files);

        for (i, path) in files.iter().enumerate() {
            if sink.is_cancelled() {
                return Err("已取消".into());
            }
            sink.emit(Progress::Item {
                done: i,
                total: files.len(),
                label: file_label(path),
            });

            let data = std::fs::read(path).map_err(|e| format!("{}：{e}", file_label(path)))?;
            let report = compress::run(&data, tier, grayscale, sink)
                .map_err(|e| format!("{}：{e}", file_label(path)))?;
            for w in report.warnings {
                let mut w = w;
                w.detail = format!("{}：{}", file_label(path), w.detail);
                sink.emit(Progress::Warn(w));
            }

            if report.value.is_mostly_vector() {
                sink.emit(Progress::Warn(pdfcore::Warning::new(
                    pdfcore::WarningKind::ImageKeptOriginal,
                    format!(
                        "{}：这份 PDF 以文字/矢量为主，几乎没有可压缩的图像",
                        file_label(path)
                    ),
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
            last_out = Some(out);
        }

        let saved = if before > 0 {
            100.0 * (1.0 - after as f32 / before as f32)
        } else {
            0.0
        };
        Ok(Done {
            summary: format!(
                "{} 个文件：{} → {}（{saved:.0}%）",
                files.len(),
                human_size(before),
                human_size(after)
            ),
            output: last_out,
        })
    }));
}
