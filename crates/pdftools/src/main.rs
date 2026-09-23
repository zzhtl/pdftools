// 发布构建不弹控制台窗口；debug 构建保留，方便看日志。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod fonts_ui;
mod job;
mod tabs;
mod thumbs;

use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// 进程启动时刻。启动各阶段的耗时日志（`RUST_LOG=pdftools=debug`）都以它为零点。
static STARTED: OnceLock<Instant> = OnceLock::new();

pub fn since_start() -> Duration {
    STARTED.get().map(Instant::elapsed).unwrap_or_default()
}

fn main() -> eframe::Result {
    STARTED.get_or_init(Instant::now);
    env_logger::init();

    // 命令行上给的文件直接进列表。这让「用 pdftools 打开」「把文件拖到图标上」
    // 这类系统级用法能正常工作。
    let initial: Vec<std::path::PathBuf> = std::env::args_os()
        .skip(1)
        .map(std::path::PathBuf::from)
        .filter(|p| p.exists())
        .collect();

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([960.0, 680.0])
        .with_min_inner_size([680.0, 480.0])
        .with_title("pdftools")
        .with_drag_and_drop(true);
    // 图标解析失败不该挡住程序启动，顶多是任务栏上没图标。
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/icon.png"))
    {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "pdftools",
        options,
        Box::new(|cc| {
            log::debug!("启动：窗口与 GL 上下文就绪 {:?}", since_start());
            let font = fonts_ui::install(&cc.egui_ctx);
            log::debug!("启动：界面字体装好 {:?}", since_start());
            let mut app = app::App::new(cc, font);
            app.open_paths(initial);
            log::debug!("启动：命令行文件入列 {:?}", since_start());
            Ok(Box::new(app))
        }),
    )
}
