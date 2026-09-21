//! 界面字体。
//!
//! egui 自带的字体只覆盖拉丁与西里尔字母，中文会全是豆腐块。
//! 这里优先加载**系统字体**：macOS 上 PingFang 的渲染质量明显好于其他选择，
//! 而且不用把十几 MB 的字体塞进二进制。

use pdfcore::fonts::system::{SystemFonts, UI_CJK_PREFERENCE};

/// 返回实际用上的字体名，供「关于」里显示；没找到中文字体时返回 None。
pub fn install(ctx: &egui::Context) -> Option<String> {
    let system = SystemFonts::load();
    let found = system.find(UI_CJK_PREFERENCE, false, false)?;

    let mut fonts = egui::FontDefinitions::default();
    let data = egui::FontData {
        font: found.face.data().to_vec().into(),
        // .ttc 是字体集合，索引选错会拿到日文或韩文那一份。
        index: found.face.index(),
        tweak: Default::default(),
    };
    fonts
        .font_data
        .insert("cjk".to_owned(), std::sync::Arc::new(data));

    // 插到最前面：中文优先用这个字体，缺的字形再回落到 egui 自带字体。
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, "cjk".to_owned());
    }
    ctx.set_fonts(fonts);
    Some(found.family)
}
