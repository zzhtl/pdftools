//! 图片批量压缩。

use std::path::PathBuf;

use pdfcore::imaging::{probe, Tier};
use pdfcore::ops::images_compress;

use crate::app::{tier_selector, App};
use crate::job::{human_size, run_batch, Done, Job};

use super::common::{self, FileList};
use super::{file_label, FOOTER};

pub struct State {
    pub files: FileList,
    pub tier: Tier,
    /// 上次添加图片、保存结果的文件夹。
    pub open_dir: Option<PathBuf>,
    pub out_dir: Option<PathBuf>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            files: FileList::default(),
            tier: Tier::Balanced,
            open_dir: None,
            out_dir: None,
        }
    }
}

impl State {
    /// 返回不认的文件有几个。
    pub fn add_paths(&mut self, paths: Vec<PathBuf>) -> usize {
        self.files.add(paths, |p| probe::looks_like_image(p))
    }
}

pub fn ui(app: &mut App, ui: &mut egui::Ui) {
    let busy = app.busy();
    let ctx = ui.ctx().clone();

    ui.horizontal(|ui| {
        if ui.button("添加图片…").on_hover_text("Ctrl+O").clicked() || common::open_shortcut(ui)
        {
            if let Some(picked) = common::pick_files(
                &mut app.img_compress.open_dir,
                "图片",
                probe::IMAGE_EXTENSIONS,
            ) {
                app.img_compress.add_paths(picked);
            }
        }
        if ui.button("按文件名排序").clicked() {
            app.img_compress.files.sort_by_name();
        }
        if ui.button("清空").clicked() {
            app.img_compress.files.clear();
        }
    });
    ui.weak("输出到你选的目录，原文件不会被改动。带透明通道的图会保持 PNG。");
    tier_selector(ui, &mut app.img_compress.tier, true);
    ui.separator();

    if app.img_compress.files.items.is_empty() {
        ui.weak("还没有添加图片。");
        return;
    }

    let height = ui.available_height() - FOOTER;
    common::file_list(
        ui,
        "imgc",
        &mut app.img_compress.files,
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
    let Some(dir) = common::pick_folder(&mut app.img_compress.out_dir) else {
        return;
    };
    let files = app.img_compress.files.items.clone();
    let tier = app.img_compress.tier;

    app.job = Some(Job::spawn(ctx, files.len(), move |worker| {
        let quality = tier.for_images();
        let mut namer = pdfcore::fsio::OutputNamer::new(&files);
        let (mut before, mut after) = (0u64, 0u64);
        let batch = run_batch(worker, &files, |path, _| {
            let (item, notes) = images_compress::compress_file(path, &dir, &quality, &mut namer)?;
            // 这些提示本身已经带着文件名。
            for w in notes {
                worker.warn(w);
            }
            before += item.before;
            after += item.after;
            let detail = format!("{} → {}", human_size(item.before), human_size(item.after));
            Ok((item.output, detail))
        })?;

        let saved = if before > 0 {
            100.0 * (1.0 - after as f32 / before as f32)
        } else {
            0.0
        };
        let mut summary = format!(
            "{} 张图片：{} → {}（{saved:.0}%）",
            batch.succeeded,
            human_size(before),
            human_size(after)
        );
        if batch.failed > 0 {
            summary += &format!("；{} 张没有压成", batch.failed);
        }
        Ok(Done {
            summary,
            output: batch.last_output,
        })
    }));
}
