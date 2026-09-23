//! PDF 转图片。

use std::path::PathBuf;

use pdfcore::fsio::OutputNamer;
use pdfcore::ops::pdf_to_images::{self, Format, Options};
use pdfcore::pdf::ranges;
use pdfcore::{Progress, ProgressSink};

use crate::app::App;
use crate::job::{run_batch, Done, Job};

use super::common::{self, FileList, PDF_EXTENSIONS};
use super::{file_label, FOOTER};

/// 可选的分辨率。150 足够屏幕上看、发给别人；打印用 300。
pub const DPI_CHOICES: [u32; 5] = [96, 150, 200, 300, 600];

pub struct State {
    pub files: FileList,
    pub dpi: u32,
    pub format: Format,
    /// 页码范围，空表示全部。
    pub pages: String,
    /// 上次添加 PDF、保存图片的文件夹。
    pub open_dir: Option<PathBuf>,
    pub out_dir: Option<PathBuf>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            files: FileList::default(),
            dpi: 150,
            format: Format::Png,
            pages: String::new(),
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
    let pal = crate::theme::palette(ui);
    let state = &mut app.pdf2img;

    ui.horizontal(|ui| {
        if ui.button("添加 PDF…").on_hover_text("Ctrl+O").clicked() || common::open_shortcut(ui)
        {
            if let Some(picked) = common::pick_files(&mut state.open_dir, "PDF", PDF_EXTENSIONS) {
                state.add_paths(picked);
            }
        }
        if ui.button("清空").clicked() {
            state.files.clear();
        }
    });
    ui.weak("把 PDF 或装着它们的文件夹拖进窗口也可以添加。每一页转成一张图片。");

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("分辨率：");
        for dpi in DPI_CHOICES {
            ui.selectable_value(&mut state.dpi, dpi, format!("{dpi} DPI"));
        }
    });
    ui.weak("150 DPI 适合在屏幕上看、发给别人；打印用 300 DPI。");
    ui.horizontal(|ui| {
        ui.label("格式：");
        ui.selectable_value(&mut state.format, Format::Png, "PNG")
            .on_hover_text("无损。文字为主的页面选它，字的边缘干净");
        ui.selectable_value(&mut state.format, Format::Jpeg, "JPEG")
            .on_hover_text("体积小。照片、扫描件选它");
    });
    let range_error = ranges::check(&state.pages).err();
    ui.horizontal(|ui| {
        ui.label("页码：");
        ui.add(
            egui::TextEdit::singleline(&mut state.pages)
                .hint_text("全部")
                .desired_width(160.0)
                .text_color_opt(range_error.is_some().then_some(pal.error)),
        );
        match &range_error {
            Some(e) => ui.colored_label(pal.error, e),
            None => ui.weak("例如 1-3, 5, 8-（8- 表示第 8 页到最后）"),
        };
    });
    ui.separator();

    if state.files.items.is_empty() {
        ui.weak("还没有添加 PDF。");
        return;
    }

    let height = ui.available_height() - FOOTER;
    common::file_list(
        ui,
        "pdf2img",
        &mut state.files,
        height,
        24.0,
        |ui, _i, path| {
            ui.label(file_label(path));
        },
    );

    ui.separator();
    if common::run_button(ui, "转成图片到文件夹…", !busy && range_error.is_none()) {
        start(app, &ctx);
    }
}

fn start(app: &mut App, ctx: &egui::Context) {
    let state = &mut app.pdf2img;
    let Some(dir) = common::pick_folder(&mut state.out_dir) else {
        return;
    };
    let files = state.files.items.clone();
    let opts = Options {
        dpi: state.dpi as f32,
        format: state.format,
        pages: state.pages.clone(),
    };

    app.job = Some(Job::spawn(ctx, files.len(), move |worker| {
        // 输出目录选成源目录也不怕：什么都不覆盖，同名的自动改名。
        let mut namer = OutputNamer::new(&files);
        let mut images = 0usize;
        let batch = run_batch(worker, &files, |path, sink| {
            let report = pdf_to_images::run(path, &dir, &opts, &mut namer, sink)?;
            for w in report.warnings {
                sink.emit(Progress::Warn(w));
            }
            let out = &report.value.files;
            images += out.len();
            let detail = format!(
                "{} 页中的 {} 页 → {}",
                report.value.page_count,
                out.len(),
                opts.format.extension().to_uppercase()
            );
            Ok((out.first().cloned().unwrap_or_else(|| dir.clone()), detail))
        })?;

        let mut summary = format!("已转换 {} 个 PDF，共 {images} 张图片", batch.succeeded);
        if batch.failed > 0 {
            summary += &format!("；{} 个没有转成", batch.failed);
        }
        Ok(Done {
            summary,
            output: batch.last_output,
        })
    }));
}
