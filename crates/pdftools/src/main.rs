// 发布构建不弹控制台窗口；debug 构建保留，方便看日志。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod fonts_ui;
mod job;
mod tabs;
mod thumbs;

fn main() -> eframe::Result {
    env_logger::init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([960.0, 680.0])
            .with_min_inner_size([680.0, 480.0])
            .with_title("pdftools")
            .with_drag_and_drop(true),
        ..Default::default()
    };

    eframe::run_native(
        "pdftools",
        options,
        Box::new(|cc| {
            let font = fonts_ui::install(&cc.egui_ctx);
            Ok(Box::new(app::App::new(cc, font)))
        }),
    )
}
