//! 图片批量压缩。

use std::path::PathBuf;

use pdfcore::imaging::{probe, Tier};
use pdfcore::ops::images_compress;
use pdfcore::Progress;

use crate::app::{tier_selector, App};
use crate::job::{human_size, Done, Job};

use super::{draggable_list, file_label, FileList};

pub struct State {
    pub files: FileList,
    pub tier: Tier,
}

impl Default for State {
    fn default() -> Self {
        Self {
            files: FileList::default(),
            tier: Tier::Balanced,
        }
    }
}

impl State {
    pub fn add_paths(&mut self, paths: Vec<PathBuf>) {
        self.files.add(paths, probe::looks_like_image);
    }
}

pub fn ui(app: &mut App, ui: &mut egui::Ui) {
    let busy = app.busy();
    let ctx = ui.ctx().clone();

    ui.horizontal(|ui| {
        ui.add_enabled_ui(!busy, |ui| {
            if ui.button("添加图片…").clicked() {
                if let Some(picked) = rfd::FileDialog::new()
                    .add_filter(
                        "图片",
                        &["jpg", "jpeg", "png", "gif", "bmp", "tif", "tiff", "webp"],
                    )
                    .pick_files()
                {
                    app.img_compress.add_paths(picked);
                }
            }
            if ui.button("清空").clicked() {
                app.img_compress.files.items.clear();
            }
        });
    });
    ui.weak("输出到你选的目录，原文件不会被改动。带透明通道的图会保持 PNG。");
    ui.separator();

    if app.img_compress.files.items.is_empty() {
        ui.weak("还没有添加图片。");
        return;
    }

    draggable_list(
        ui,
        "imgc",
        &mut app.img_compress.files,
        !busy,
        |ui, _i, path| {
            ui.label(file_label(path));
            if let Ok(m) = std::fs::metadata(path) {
                ui.weak(human_size(m.len()));
            }
        },
    );

    ui.separator();
    tier_selector(ui, &mut app.img_compress.tier, !busy);

    ui.add_space(6.0);
    let can_run = !busy && !app.img_compress.files.items.is_empty();
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
    let files = app.img_compress.files.items.clone();
    let tier = app.img_compress.tier;

    app.job = Some(Job::spawn(ctx, move |sink| {
        let report = images_compress::run(&files, &dir, tier, sink).map_err(|e| e.to_string())?;
        for w in report.warnings {
            sink.emit(Progress::Warn(w));
        }
        let before = report.value.total_before();
        let after = report.value.total_after();
        let saved = if before > 0 {
            100.0 * (1.0 - after as f32 / before as f32)
        } else {
            0.0
        };
        Ok(Done {
            summary: format!(
                "{} 张图片：{} → {}（{saved:.0}%）",
                report.value.items.len(),
                human_size(before),
                human_size(after)
            ),
            output: report.value.items.first().map(|i| i.output.clone()),
        })
    }));
}
