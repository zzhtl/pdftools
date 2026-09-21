//! 把整形好的字形序列写进内容流。

use pdf_writer::{Content, Finish, Name, Str};

use crate::fonts::{FontFace, GidMap, ShapedGlyph};

/// 一段待绘制的文本：同字体、同字号、同颜色，从某个基线点开始。
pub struct TextItem<'a> {
    pub glyphs: &'a [ShapedGlyph],
    /// 内容流里 `/Name Tf` 用的资源名。
    pub font_res: &'a str,
    pub size_pt: f32,
    /// 基线起点，PDF 用户空间（原点在左下角）。
    pub x_pt: f32,
    pub y_pt: f32,
    pub color: [u8; 3],
    /// 字间额外间距，用于纯 CJK 行的两端对齐。
    pub char_spacing: f32,
    /// 词间额外间距（只作用于字节 0x20），用于含空格行的两端对齐。
    pub word_spacing: f32,
}

/// 绘制一段文本。
///
/// 关键点：`/W` 数组里写的是 hmtx 的步进，所以 GPOS 带来的调整必须在这里
/// 用 TJ 的数字偏移补回去，否则字距会错。
pub fn show_text(content: &mut Content, item: &TextItem, face: &FontFace, map: &GidMap) {
    if item.glyphs.is_empty() {
        return;
    }
    let metrics = face.metrics();

    content.begin_text();
    content.set_fill_rgb(
        item.color[0] as f32 / 255.0,
        item.color[1] as f32 / 255.0,
        item.color[2] as f32 / 255.0,
    );
    content.set_font(Name(item.font_res.as_bytes()), item.size_pt);
    if item.char_spacing != 0.0 {
        content.set_char_spacing(item.char_spacing);
    }
    if item.word_spacing != 0.0 {
        content.set_word_spacing(item.word_spacing);
    }
    // 直接设文本矩阵而不是用 Td 链式累加：每段自带绝对位置，
    // 混排时不必跨 Tf 切换去追踪累计步进。
    content.set_text_matrix([1.0, 0.0, 0.0, 1.0, item.x_pt, item.y_pt]);

    let mut positioned = content.show_positioned();
    let mut items = positioned.items();

    let mut pending: Vec<u8> = Vec::with_capacity(item.glyphs.len() * 2);
    for g in item.glyphs {
        let new_gid = map.new_gid(g.gid);
        pending.extend_from_slice(&new_gid.to_be_bytes());

        // hmtx 步进与整形步进的差额，就是 GPOS 的贡献。
        let hmtx = face.advance(g.gid) as f32;
        let delta = g.x_advance as f32 - hmtx;
        if delta.abs() > 0.5 {
            items.show(Str(&pending));
            pending.clear();
            // TJ 的数字是「从当前位置减去」，单位是 1/1000 文本空间，
            // 所以要往右多移就得给负数。
            items.adjust(-metrics.to_pdf_units(delta));
        }
    }
    if !pending.is_empty() {
        items.show(Str(&pending));
    }

    items.finish();
    positioned.finish();
    content.end_text();
}
