//! 主界面骨架：四个 Tab、任务状态条、警告面板。

use pdfcore::imaging::Tier;
use pdfcore::{Warning, WarningKind};

use crate::job::{Job, State};
use crate::tabs;
use crate::thumbs::ThumbCache;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    ImagesToPdf,
    DocxToPdf,
    PdfCompress,
    ImagesCompress,
}

impl Tab {
    pub const ALL: [Tab; 4] = [
        Tab::ImagesToPdf,
        Tab::DocxToPdf,
        Tab::PdfCompress,
        Tab::ImagesCompress,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Tab::ImagesToPdf => "图片转 PDF",
            Tab::DocxToPdf => "Word 转 PDF",
            Tab::PdfCompress => "PDF 压缩",
            Tab::ImagesCompress => "图片压缩",
        }
    }
}

pub struct App {
    pub tab: Tab,
    pub images: tabs::img2pdf::State,
    pub docx: tabs::docx2pdf::State,
    pub compress: tabs::pdf_compress::State,
    pub img_compress: tabs::img_compress::State,
    pub job: Option<Job>,
    pub thumbs: ThumbCache,
    /// 启动时探测到的界面字体名；None 表示本机没有中文字体。
    pub ui_font: Option<String>,
    /// 首帧耗时只记一次。
    first_frame_logged: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, ui_font: Option<String>) -> Self {
        Self {
            tab: Tab::ImagesToPdf,
            images: Default::default(),
            docx: Default::default(),
            compress: Default::default(),
            img_compress: Default::default(),
            job: None,
            thumbs: ThumbCache::new(&cc.egui_ctx),
            ui_font,
            first_frame_logged: false,
        }
    }

    /// 按扩展名把文件分派到对应的 Tab，并切换过去。
    /// 拖放和命令行参数共用这条路径。
    pub fn open_paths(&mut self, paths: Vec<std::path::PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let ext = |p: &std::path::Path| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase())
                .unwrap_or_default()
        };
        let docx: Vec<_> = paths
            .iter()
            .filter(|p| matches!(ext(p).as_str(), "docx" | "doc"))
            .cloned()
            .collect();
        let pdfs: Vec<_> = paths.iter().filter(|p| ext(p) == "pdf").cloned().collect();
        let images: Vec<_> = paths
            .iter()
            .filter(|p| {
                pdfcore::imaging::probe::looks_like_image(p) || pdfcore::imaging::probe::is_heif(p)
            })
            .cloned()
            .collect();

        if !images.is_empty() {
            self.tab = Tab::ImagesToPdf;
            self.images.add_paths(images);
        }
        if !docx.is_empty() {
            self.tab = Tab::DocxToPdf;
            self.docx.add_paths(docx);
        }
        if !pdfs.is_empty() {
            self.tab = Tab::PdfCompress;
            self.compress.add_paths(pdfs);
        }
    }

    pub fn busy(&self) -> bool {
        self.job.as_ref().is_some_and(|j| j.is_active())
    }
}

impl eframe::App for App {
    /// 通道排空放在这里而不是 `ui`：窗口最小化时 eframe 不跑 egui pass，
    /// 因而不调 `ui`，但仍会调 `logic`。放错地方的话，用户一最小化，
    /// 长任务就会看起来卡死。
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(job) = &mut self.job {
            job.pump();
        }
        self.thumbs.pump(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if !self.first_frame_logged {
            self.first_frame_logged = true;
            log::debug!("启动：开始绘制首帧 {:?}", crate::since_start());
        }
        let ctx = ui.ctx().clone();
        self.handle_dropped_files(&ctx);

        egui::Panel::top("tabs").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                for tab in Tab::ALL {
                    if ui.selectable_label(self.tab == tab, tab.title()).clicked() {
                        self.tab = tab;
                    }
                }
            });
            ui.add_space(4.0);
        });

        egui::Panel::bottom("status").show(ui, |ui| {
            self.status_bar(ui);
        });

        egui::CentralPanel::default().show(ui, |ui| {
            if self.ui_font.is_none() {
                ui.colored_label(
                    egui::Color32::from_rgb(0xC0, 0x50, 0x20),
                    "未找到中文字体，界面与导出的 PDF 都可能显示为方块。\
                     请安装 Noto Sans CJK 或思源黑体。",
                );
                ui.separator();
            }
            egui::ScrollArea::vertical().show(ui, |ui| match self.tab {
                Tab::ImagesToPdf => tabs::img2pdf::ui(self, ui),
                Tab::DocxToPdf => tabs::docx2pdf::ui(self, ui),
                Tab::PdfCompress => tabs::pdf_compress::ui(self, ui),
                Tab::ImagesCompress => tabs::img_compress::ui(self, ui),
            });
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // 别让后台线程在进程退出后还往磁盘上写东西。
        if let Some(job) = &mut self.job {
            job.request_cancel();
        }
    }
}

impl App {
    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<std::path::PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if dropped.is_empty() || self.busy() {
            return;
        }
        // 在「图片压缩」页拖图片时应当留在本页，其余情况按文件类型自动分派。
        if self.tab == Tab::ImagesCompress {
            self.img_compress.add_paths(dropped);
        } else {
            self.open_paths(dropped);
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        // 先把要执行的动作算出来，再动 self —— 否则闭包会和 `&mut self.job` 抢借用。
        let mut dismiss = false;
        let mut cancel = false;
        let mut reveal_path: Option<std::path::PathBuf> = None;

        match self.job.as_ref() {
            None => {
                ui.horizontal(|ui| {
                    ui.label("就绪");
                    if let Some(font) = &self.ui_font {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.weak(format!("界面字体：{font}"));
                        });
                    }
                });
                return;
            }
            Some(job) => {
                ui.add_space(4.0);
                match &job.state {
                    State::Running | State::Cancelling => {
                        let cancelling = job.state == State::Cancelling;
                        ui.horizontal(|ui| {
                            let bar = match job.fraction() {
                                Some(f) => egui::ProgressBar::new(f).show_percentage(),
                                None => egui::ProgressBar::new(0.0).animate(true),
                            };
                            ui.add_sized([(ui.available_width() - 90.0).max(60.0), 18.0], bar);
                            if ui
                                .add_enabled(!cancelling, egui::Button::new("取消"))
                                .clicked()
                            {
                                cancel = true;
                            }
                        });
                        let text = if cancelling {
                            "正在取消…".to_string()
                        } else if job.total > 0 {
                            format!("{}/{}  {}", job.done, job.total, job.label)
                        } else {
                            job.label.clone()
                        };
                        ui.weak(text);
                    }
                    State::Finished(msg) => {
                        let msg = msg.clone();
                        let out = job.output.clone();
                        ui.horizontal(|ui| {
                            ui.colored_label(egui::Color32::from_rgb(0x2E, 0x7D, 0x32), "✔");
                            ui.label(msg);
                            if let Some(out) = out {
                                if ui.button("打开所在文件夹").clicked() {
                                    reveal_path = Some(out);
                                }
                            }
                            if ui.button("知道了").clicked() {
                                dismiss = true;
                            }
                        });
                    }
                    State::Failed(msg) => {
                        let msg = msg.clone();
                        ui.horizontal(|ui| {
                            ui.colored_label(egui::Color32::from_rgb(0xC6, 0x28, 0x28), "✖");
                            ui.label(msg);
                            if ui.button("知道了").clicked() {
                                dismiss = true;
                            }
                        });
                    }
                }

                // 任务产生的所有提示都摊开给用户看，不折叠、不省略。
                if !job.warnings.is_empty() {
                    let warnings = job.warnings.clone();
                    egui::ScrollArea::vertical()
                        .max_height(140.0)
                        .id_salt("job-warnings")
                        .show(ui, |ui| warnings_panel(ui, &warnings));
                }
                ui.add_space(4.0);
            }
        }

        if cancel {
            if let Some(job) = &mut self.job {
                job.request_cancel();
            }
        }
        if dismiss {
            self.job = None;
        }
        if let Some(p) = reveal_path {
            reveal(&p);
        }
    }
}

/// 转换报告。**必须在保存之前展示**，这样用户发现丢了东西还来得及放弃。
pub fn warnings_panel(ui: &mut egui::Ui, warnings: &[Warning]) {
    if warnings.is_empty() {
        return;
    }
    let unsupported = warnings
        .iter()
        .filter(|w| w.kind == WarningKind::UnsupportedElement)
        .count();
    let title = if unsupported > 0 {
        format!(
            "本次转换有 {unsupported} 处内容未能完整还原（共 {} 条提示）",
            warnings.len()
        )
    } else {
        format!("{} 条提示", warnings.len())
    };

    egui::Frame::new()
        .fill(egui::Color32::from_rgb(0xFF, 0xF8, 0xE1))
        .inner_margin(8.0)
        .corner_radius(4.0)
        .show(ui, |ui| {
            ui.colored_label(egui::Color32::from_rgb(0x8D, 0x6E, 0x00), title);
            for w in warnings {
                let line = match w.page {
                    Some(p) => format!("· 第 {p} 页：{}", w.detail),
                    None => format!("· {}", w.detail),
                };
                ui.colored_label(egui::Color32::from_rgb(0x60, 0x4A, 0x00), line);
            }
        });
}

/// 档位选择器。四个功能共用。
pub fn tier_selector(ui: &mut egui::Ui, tier: &mut Tier, enabled: bool) {
    ui.horizontal(|ui| {
        ui.label("质量档位：");
        ui.add_enabled_ui(enabled, |ui| {
            for t in Tier::ALL {
                if ui
                    .selectable_label(*tier == t, t.label())
                    .on_hover_text(t.description())
                    .clicked()
                {
                    *tier = t;
                }
            }
        });
    });
    ui.weak(tier.description());
}

fn reveal(path: &std::path::Path) {
    let dir = path.parent().unwrap_or(path);
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer").arg(dir).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(dir).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
}
