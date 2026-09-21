//! 图片转 PDF。

use std::path::PathBuf;

use pdfcore::imaging::{probe, Fidelity, Tier};
use pdfcore::ops::images_to_pdf;
use pdfcore::Progress;

use crate::app::{tier_selector, App};
use crate::job::{human_size, write_atomic, Done, Job};
use crate::thumbs::Thumb;

use super::{draggable_list, file_label, FileList};

pub struct State {
    pub files: FileList,
    pub tier: Tier,
    pub skipped_heif: Vec<String>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            files: FileList::default(),
            // 默认「高质量」而不是「无损」：无损档对高分辨率照片会产出非常大的 PDF，
            // 而 300 DPI 上限在肉眼上没有差别。
            tier: Tier::HighQuality,
            skipped_heif: Vec::new(),
        }
    }
}

impl State {
    pub fn add_paths(&mut self, paths: Vec<PathBuf>) {
        // HEIC 单独挑出来给一句有用的提示，而不是等到转换时才报「解码失败」。
        for p in &paths {
            if probe::is_heif(p) {
                let name = file_label(p);
                if !self.skipped_heif.contains(&name) {
                    self.skipped_heif.push(name);
                }
            }
        }
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
                    app.images.add_paths(picked);
                }
            }
            if ui.button("按文件名排序").clicked() {
                app.images.files.sort_by_name();
            }
            if ui.button("按修改时间排序").clicked() {
                app.images.files.sort_by_mtime();
            }
            if ui.button("清空").clicked() {
                app.images.files.items.clear();
                app.images.skipped_heif.clear();
            }
        });
    });

    ui.weak("把图片拖进窗口也可以添加。列表里上下拖动可调整顺序，顺序即页序。");
    ui.weak("每页的尺寸与比例都跟随该张图片，不会出现白边。");
    ui.separator();

    if !app.images.skipped_heif.is_empty() {
        ui.colored_label(
            egui::Color32::from_rgb(0xC0, 0x50, 0x20),
            format!(
                "已忽略 {} 个 HEIC/HEIF 文件（本程序不支持该格式）：{}。\
                 请先在系统相册里导出为 JPEG。",
                app.images.skipped_heif.len(),
                app.images.skipped_heif.join("、")
            ),
        );
        ui.separator();
    }

    if app.images.files.items.is_empty() {
        ui.weak("还没有添加图片。");
        return;
    }

    let thumbs = &mut app.thumbs;
    draggable_list(ui, "img", &mut app.images.files, !busy, |ui, _i, path| {
        match thumbs.get(path) {
            Thumb::Ready(tex) => {
                ui.add(egui::Image::new(&tex).max_height(48.0).max_width(64.0));
            }
            Thumb::Pending => {
                ui.add_sized([64.0, 48.0], egui::Spinner::new());
            }
            Thumb::Failed => {
                ui.add_sized([64.0, 48.0], egui::Label::new("无法预览"));
            }
        }
        ui.label(file_label(path));
    });
    let keep = app.images.files.items.clone();
    app.thumbs.retain(&keep);

    ui.separator();
    tier_selector(ui, &mut app.images.tier, !busy);

    ui.add_space(6.0);
    let can_run = !busy && !app.images.files.items.is_empty();
    if ui
        .add_enabled(can_run, egui::Button::new("生成 PDF…"))
        .clicked()
    {
        start(app, &ctx);
    }
}

fn start(app: &mut App, ctx: &egui::Context) {
    let Some(out) = rfd::FileDialog::new()
        .add_filter("PDF", &["pdf"])
        .set_file_name("合并.pdf")
        .save_file()
    else {
        return;
    };

    let paths = app.images.files.items.clone();
    let tier = app.images.tier;

    app.job = Some(Job::spawn(ctx, move |sink| {
        let report = images_to_pdf::run(&paths, tier, sink).map_err(|e| e.to_string())?;
        // 把核心层收集到的警告转发给界面。静默丢弃是不允许的。
        for w in report.warnings {
            sink.emit(Progress::Warn(w));
        }
        write_atomic(&out, &report.value.pdf)?;

        // 如实汇报每张图走了哪条路径。「不失真」不是口号，是可核对的事实。
        let passthrough = count(&report.value.fidelity, Fidelity::Passthrough);
        let lossless = count(&report.value.fidelity, Fidelity::Lossless);
        let reencoded = count(&report.value.fidelity, Fidelity::Reencoded);
        let mut parts = Vec::new();
        if passthrough > 0 {
            parts.push(format!("{passthrough} 张原图直通（零损失）"));
        }
        if lossless > 0 {
            parts.push(format!("{lossless} 张无损存储"));
        }
        if reencoded > 0 {
            parts.push(format!("{reencoded} 张重新编码"));
        }

        Ok(Done {
            summary: format!(
                "已生成 {}：{} 页，{}；{}",
                out.file_name().unwrap_or_default().to_string_lossy(),
                report.value.fidelity.len(),
                human_size(report.value.pdf.len() as u64),
                parts.join("，")
            ),
            output: Some(out),
        })
    }));
}

fn count(list: &[(PathBuf, Fidelity)], want: Fidelity) -> usize {
    list.iter().filter(|(_, f)| *f == want).count()
}
