//! 把排好版的页面画成 PDF。

use std::collections::{BTreeMap, BTreeSet, HashMap};

use pdf_writer::Ref;

use super::layout::{LaidOut, PaintOp};
use crate::error::{Result, Warning, WarningKind};
use crate::fonts::{FontBook, FontId};
use crate::imaging::{self, ColorData};
use crate::pdf::writer::font::{embed_font, EmbeddedFont};
use crate::pdf::writer::image::{ImageData, ImageEncoding};
use crate::pdf::writer::{Canvas, DocBuilder, GlyphRun};

/// 画出来的 PDF，以及画的时候才发现的问题（图片解不开之类）。
/// `media`：包里的图片部件，路径 → 字节。
pub fn paint(
    laid: &LaidOut,
    book: &FontBook,
    info: crate::pdf::writer::DocInfo,
    media: &HashMap<String, Vec<u8>>,
) -> Result<(Vec<u8>, Vec<Warning>)> {
    // 第一趟：把每个字体实际用到的字形收齐。子集化必须一次性知道全部用量，
    // 所以字体只能等内容全部排完才能写。
    let mut used: BTreeMap<FontId, BTreeMap<u16, String>> = BTreeMap::new();
    // `.notdef`（GID 0）可能对应好几个不同的缺字。
    let mut notdef: BTreeMap<FontId, BTreeSet<String>> = BTreeMap::new();
    for p in &laid.pages {
        for op in p.under.iter().chain(&p.ops).chain(&p.over) {
            let PaintOp::Text {
                font,
                glyphs,
                unicode,
                ..
            } = op
            else {
                continue;
            };
            let entry = used.entry(*font).or_default();
            for (i, g) in glyphs.iter().enumerate() {
                let text = unicode.get(i).cloned().unwrap_or_default();
                if g.gid == 0 && !text.is_empty() {
                    notdef.entry(*font).or_default().insert(text.clone());
                }
                entry
                    .entry(g.gid)
                    .and_modify(|existing| {
                        // 同一字形可能在不同位置对应不同原文（罕见）。
                        // 保留第一个非空的，空的不覆盖已有内容。
                        if existing.is_empty() && !text.is_empty() {
                            *existing = text.clone();
                        }
                    })
                    .or_insert(text);
            }
        }
    }
    // 几个不同的缺字共用 GID 0 时，ToUnicode 只能给它一个原文。映射成 U+FFFD，
    // 宁可抽出「�」，也不要把所有缺字都抽成第一个缺字 —— 曾经把「②③」抽成了「℃」。
    for (font, texts) in &notdef {
        if texts.len() > 1 {
            if let Some(entry) = used.get_mut(font) {
                entry.insert(0, "\u{FFFD}".to_string());
            }
        }
    }

    let mut doc = DocBuilder::new();
    doc.set_info(info);
    let mut embedded: BTreeMap<FontId, EmbeddedFont> = BTreeMap::new();
    for (font_id, glyphs) in &used {
        let face = book.face(*font_id);
        let (pdf, alloc) = doc.parts();
        embedded.insert(*font_id, embed_font(pdf, alloc, face, glyphs)?);
    }

    // 图片：同一个部件只写一次。解不开的（EMF、WMF 这类矢量图，或者坏了的）画成灰框。
    let mut images: HashMap<&str, Option<Ref>> = HashMap::new();
    let mut broken: BTreeSet<&str> = BTreeSet::new();
    for laid_page in &laid.pages {
        let (w, h) = laid_page.size;
        let mut canvas = Canvas::new(w, h);
        // 衬于文字下方的图、正文、浮于文字上方的图，依次画。
        for op in laid_page
            .under
            .iter()
            .chain(&laid_page.ops)
            .chain(&laid_page.over)
        {
            match op {
                PaintOp::Text {
                    font,
                    size_pt,
                    x,
                    y,
                    glyphs,
                    color,
                    extra_after,
                    synthetic_bold,
                    synthetic_italic,
                    ..
                } => {
                    let Some(e) = embedded.get(font) else {
                        continue;
                    };
                    canvas.glyphs(&GlyphRun {
                        font: e,
                        face: book.face(*font),
                        glyphs,
                        extra_after,
                        size_pt: *size_pt,
                        x_pt: *x,
                        y_pt: *y,
                        rise_pt: 0.0,
                        color: *color,
                        synthetic_bold: *synthetic_bold,
                        synthetic_italic: *synthetic_italic,
                    });
                }
                PaintOp::Rect { x, y, w, h, color } => canvas.fill_rect(*x, *y, *w, *h, *color),
                PaintOp::Image {
                    part,
                    x,
                    y,
                    w,
                    h,
                    crop,
                } => {
                    let image = *images
                        .entry(part.as_str())
                        .or_insert_with(|| embed_image(&mut doc, media.get(part)?));
                    match image {
                        Some(image) => place_image(&mut canvas, image, [*x, *y, *w, *h], *crop),
                        None => {
                            broken.insert(part.as_str());
                            missing_box(&mut canvas, [*x, *y, *w, *h]);
                        }
                    }
                }
                PaintOp::Line {
                    from,
                    to,
                    width,
                    color,
                    dash,
                } => {
                    let dash = (!dash.is_empty()).then_some(dash.as_slice());
                    canvas.stroke_line(*from, *to, *width, *color, dash)
                }
                PaintOp::Link {
                    x1,
                    y1,
                    x2,
                    y2,
                    uri,
                } => canvas.link([*x1, *y1, *x2, *y2], uri),
                PaintOp::Clip { x, y, w, h } => {
                    canvas.save();
                    canvas.clip_rect(*x, *y, *w, *h);
                }
                PaintOp::EndClip => canvas.restore(),
            }
        }
        doc.add_page(canvas.finish());
    }

    // 一页都没有的 PDF 不合法。排版总会给出至少一页，这里只是兜底：A4。
    if doc.page_count() == 0 {
        doc.add_page(Canvas::new(595.3, 841.9).finish());
    }

    let mut warnings = Vec::new();
    if !broken.is_empty() {
        warnings.push(Warning::new(
            WarningKind::UnsupportedElement,
            format!(
                "{} 张图片画不出来（EMF、WMF 之类的矢量图，或者图片已损坏），已按原大小画成灰框",
                broken.len()
            ),
        ));
    }
    Ok((doc.finish()?, warnings))
}

/// 把一张图写进 PDF。解不开时返回 None。
fn embed_image(doc: &mut DocBuilder, bytes: &[u8]) -> Option<Ref> {
    let img = imaging::prepare_embedded(bytes).ok()?;
    let (encoding, gray) = match &img.color {
        ColorData::Jpeg { bytes, gray } => (ImageEncoding::Jpeg(bytes), *gray),
        ColorData::Raw { bytes, gray } => (ImageEncoding::Raw(bytes), *gray),
    };
    Some(doc.add_image(&ImageData {
        width: img.width,
        height: img.height,
        gray,
        encoding,
        alpha: img.alpha.as_deref(),
    }))
}

/// 把图放进显示框 `[x, y, w, h]`（左下角与大小）。裁剪时整张图按比例放大，
/// 再用显示框裁掉多出来的部分。
fn place_image(canvas: &mut Canvas, image: Ref, [x, y, w, h]: [f32; 4], crop: [f32; 4]) {
    let [l, t, r, b] = crop;
    let full_w = w / (1.0 - l - r).max(1e-3);
    let full_h = h / (1.0 - t - b).max(1e-3);
    let matrix = [full_w, 0.0, 0.0, full_h, x - l * full_w, y - b * full_h];
    if crop == [0.0; 4] {
        canvas.image(image, matrix);
        return;
    }
    canvas.save();
    canvas.clip_rect(x, y, w, h);
    canvas.image(image, matrix);
    canvas.restore();
}

/// 画不出来的图：浅灰底、深灰边的框。
fn missing_box(canvas: &mut Canvas, [x, y, w, h]: [f32; 4]) {
    canvas.fill_rect(x, y, w, h, [0xEE; 3]);
    let corners = [(x, y), (x + w, y), (x + w, y + h), (x, y + h), (x, y)];
    for pair in corners.windows(2) {
        canvas.stroke_line(pair[0], pair[1], 0.5, [0x99; 3], None);
    }
}
