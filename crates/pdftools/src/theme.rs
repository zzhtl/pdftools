//! 按深浅色主题取的语义色。写死的颜色在深色主题下要么看不清，要么刺眼。

use egui::Color32;

pub struct Palette {
    /// 成功、无损、真实的拍摄时间。
    pub ok: Color32,
    /// 失败、出错、解析不了的输入。
    pub error: Color32,
    /// 需要留意：有损档位、文件时间不是拍摄时间、被跳过的文件。
    pub caution: Color32,
    /// 提示框的底色与字色（转换报告、缺拍摄时间的说明）。
    pub note_bg: Color32,
    pub note_fg: Color32,
}

pub fn palette(ui: &egui::Ui) -> Palette {
    let rgb = Color32::from_rgb;
    if ui.visuals().dark_mode {
        Palette {
            ok: rgb(0x81, 0xC7, 0x84),
            error: rgb(0xEF, 0x9A, 0x9A),
            caution: rgb(0xFF, 0xB7, 0x4D),
            note_bg: rgb(0x3A, 0x32, 0x1A),
            note_fg: rgb(0xFF, 0xE0, 0x82),
        }
    } else {
        Palette {
            ok: rgb(0x2E, 0x7D, 0x32),
            error: rgb(0xC6, 0x28, 0x28),
            caution: rgb(0xB3, 0x4A, 0x10),
            note_bg: rgb(0xFF, 0xF8, 0xE1),
            note_fg: rgb(0x60, 0x4A, 0x00),
        }
    }
}
