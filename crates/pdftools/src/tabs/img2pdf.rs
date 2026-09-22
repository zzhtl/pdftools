//! 图片转 PDF。

use std::collections::HashMap;
use std::path::PathBuf;

use pdfcore::imaging::{probe, Fidelity, Tier};
use pdfcore::ops::images_to_pdf;
use pdfcore::Progress;
use pdfcore::{DatedFile, TimeSource, Timestamp};

use crate::app::{tier_selector, App};
use crate::job::{human_size, write_atomic, Done, Job};
use crate::thumbs::Thumb;

use super::{draggable_list, file_label, FileList};

pub struct State {
    pub files: FileList,
    pub tier: Tier,
    pub skipped_heif: Vec<String>,
    /// 每个文件的时间。添加时读一次就缓存 —— 每帧去解 EXIF 会把界面拖垮。
    pub times: HashMap<PathBuf, DatedFile>,
    /// 时间输入框的文本缓冲。用户可能正输到一半，此时还解析不出合法时间，
    /// 不能因此把缓存里的值冲掉。
    pub drafts: HashMap<PathBuf, String>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            files: FileList::default(),
            // 默认无损。这条路径的承诺是「绝不失真、绝不模糊」，
            // 那就不能在用户没要求的情况下擅自重新采样 ——
            // 300 DPI 上限肉眼确实看不出，但那依然是改动了像素。
            tier: Tier::Lossless,
            skipped_heif: Vec::new(),
            times: HashMap::new(),
            drafts: HashMap::new(),
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
        for p in &self.files.items {
            if !self.times.contains_key(p) {
                if let Some(t) = pdfcore::imaging::read_time(p) {
                    self.times.insert(p.clone(), t);
                }
            }
        }
    }

    /// 按时间排序。取证材料通常需要按拍摄先后排列。
    pub fn sort_by_time(&mut self) {
        let times = &self.times;
        self.files
            .items
            .sort_by_key(|p| times.get(p).map(|t| t.when));
    }

    /// 用户手动指定的那些时间，交给核心层写进 PDF。
    pub fn manual_times(&self) -> HashMap<PathBuf, Timestamp> {
        self.times
            .iter()
            .filter(|(_, t)| t.source == TimeSource::Manual)
            .map(|(p, t)| (p.clone(), t.when))
            .collect()
    }

    /// 有几张图片拿不到真实的拍摄时间。
    pub fn missing_capture_time(&self) -> usize {
        self.files
            .items
            .iter()
            .filter(|p| {
                self.times
                    .get(*p)
                    .is_none_or(|t| !t.source.is_capture_time())
            })
            .count()
    }

    /// 把第一张的时间套用到所有图片。同一场拍摄的照片常常只需要一个时间。
    pub fn apply_first_time_to_all(&mut self) {
        let Some(when) = self
            .files
            .items
            .first()
            .and_then(|p| self.times.get(p))
            .map(|t| t.when)
        else {
            return;
        };
        for p in &self.files.items {
            self.times.insert(
                p.clone(),
                DatedFile {
                    when,
                    source: TimeSource::Manual,
                },
            );
            self.drafts.insert(p.clone(), when.display());
        }
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
            if ui.button("按拍摄时间排序").clicked() {
                app.images.sort_by_time();
            }
            if ui.button("清空").clicked() {
                app.images.files.items.clear();
                app.images.skipped_heif.clear();
                app.images.times.clear();
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

    ui.separator();
    tier_selector(ui, &mut app.images.tier, !busy);
    if app.images.tier.is_lossy() {
        ui.colored_label(
            egui::Color32::from_rgb(0xC0, 0x50, 0x20),
            "注意：该档位会对超过 DPI 上限的图片重新采样，不再是逐像素无损。\
             要绝对保真请选「无损」。",
        );
    } else {
        ui.colored_label(
            egui::Color32::from_rgb(0x2E, 0x7D, 0x32),
            "当前为无损：JPEG 原始字节直接搬入 PDF（含横拍照片，旋转由 PDF 变换矩阵完成，\
             不重新编码），其余格式逐像素无损存储。",
        );
    }

    if app.images.files.items.is_empty() {
        ui.weak("还没有添加图片。");
        return;
    }

    // 缺拍摄时间是常态而不是异常（微信、网盘转发都会剥掉），
    // 所以要主动说清楚，并给出补救办法，而不是让用户自己去发现。
    let missing = app.images.missing_capture_time();
    if missing > 0 {
        egui::Frame::new()
            .fill(egui::Color32::from_rgb(0xFF, 0xF4, 0xE5))
            .inner_margin(8.0)
            .corner_radius(4.0)
            .show(ui, |ui| {
                ui.colored_label(
                    egui::Color32::from_rgb(0x8A, 0x4B, 0x00),
                    format!(
                        "有 {missing} 张图片读不到拍摄时间（EXIF / XMP / IPTC 里都没有）。经微信、网盘或「清除元数据」处理过的照片通常都是这样。"
                    ),
                );
                ui.colored_label(
                    egui::Color32::from_rgb(0x8A, 0x4B, 0x00),
                    "文件时间不能代替拍摄时间（复制一次就被刷新），所以不会写进 PDF。可以在下面每一行里直接填写真实拍摄时间，或改用手机相册里的原图。",
                );
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!busy, egui::Button::new("把第一张的时间套用到全部"))
                        .on_hover_text("同一场拍摄的照片通常共用一个时间")
                        .clicked()
                    {
                        app.images.apply_first_time_to_all();
                    }
                });
            });
        ui.add_space(4.0);
    }

    let thumbs = &mut app.thumbs;
    let times = &mut app.images.times;
    let drafts = &mut app.images.drafts;
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

        // 时间放在每一行里，而且可以直接改。
        //
        // 只读地显示一个文件时间是不够的：那不是拍摄时间，复制一次就变，
        // 而用户往往知道真实的拍摄时刻。给一个输入框，是这些已被剥掉
        // 时间信息的照片唯一能拿到正确时间的办法。
        let current = times.get(path).copied();
        let draft = drafts
            .entry(path.clone())
            .or_insert_with(|| current.map(|t| t.when.display()).unwrap_or_default());
        let parsed = Timestamp::parse_user_input(draft);
        let bad = !draft.trim().is_empty() && parsed.is_none();

        let edit = egui::TextEdit::singleline(draft)
            .desired_width(150.0)
            .text_color_opt(bad.then_some(egui::Color32::from_rgb(0xC6, 0x28, 0x28)));
        let resp = ui.add_enabled(!busy, edit);
        if resp.changed() {
            if let Some(when) = parsed {
                if current.map(|c| c.when) != Some(when) {
                    times.insert(
                        path.clone(),
                        DatedFile {
                            when,
                            source: TimeSource::Manual,
                        },
                    );
                }
            }
        }

        match current {
            Some(t) if t.source.is_capture_time() => {
                ui.colored_label(egui::Color32::from_rgb(0x2E, 0x7D, 0x32), t.source.label());
            }
            Some(t) => {
                ui.colored_label(egui::Color32::from_rgb(0xC0, 0x50, 0x20), t.source.label());
            }
            None => {
                ui.colored_label(egui::Color32::from_rgb(0xC6, 0x28, 0x28), "无时间");
            }
        }
    });
    let keep = app.images.files.items.clone();
    app.thumbs.retain(&keep);

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
    let manual = app.images.manual_times();

    app.job = Some(Job::spawn(ctx, move |sink| {
        let report = images_to_pdf::run(&paths, tier, &manual, sink).map_err(|e| e.to_string())?;
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

        let time_note = match (report.value.creation, report.value.creation_is_capture_time) {
            (Some(t), true) => format!("；PDF 创建时间 = 最早拍摄时间 {}", t.display()),
            (Some(t), false) => format!("；无拍摄时间，PDF 创建时间记为导出时刻 {}", t.display()),
            (None, _) => String::new(),
        };
        Ok(Done {
            summary: format!(
                "已生成 {}：{} 页，{}；{}{}",
                out.file_name().unwrap_or_default().to_string_lossy(),
                report.value.fidelity.len(),
                human_size(report.value.pdf.len() as u64),
                parts.join("，"),
                time_note
            ),
            output: Some(out),
        })
    }));
}

fn count(list: &[(PathBuf, Fidelity)], want: Fidelity) -> usize {
    list.iter().filter(|(_, f)| *f == want).count()
}
