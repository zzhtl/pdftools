//! 把排好版的页面画成 PDF。

use std::collections::{BTreeMap, BTreeSet};

use super::ir::PageGeom;
use super::layout::{LaidOut, PaintOp};
use crate::error::Result;
use crate::fonts::{FontBook, FontId};
use crate::pdf::writer::font::{embed_font, EmbeddedFont};
use crate::pdf::writer::{Canvas, DocBuilder, GlyphRun};

pub fn paint(
    laid: &LaidOut,
    page: &PageGeom,
    book: &FontBook,
    info: crate::pdf::writer::DocInfo,
) -> Result<Vec<u8>> {
    // 第一趟：把每个字体实际用到的字形收齐。子集化必须一次性知道全部用量，
    // 所以字体只能等内容全部排完才能写。
    let mut used: BTreeMap<FontId, BTreeMap<u16, String>> = BTreeMap::new();
    // `.notdef`（GID 0）可能对应好几个不同的缺字。
    let mut notdef: BTreeMap<FontId, BTreeSet<String>> = BTreeMap::new();
    for p in &laid.pages {
        for op in &p.ops {
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

    for laid_page in &laid.pages {
        let mut canvas = Canvas::new(page.w_pt, page.h_pt);
        for op in &laid_page.ops {
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
                PaintOp::Line {
                    x1,
                    x2,
                    y,
                    width,
                    color,
                    dash,
                } => canvas.stroke_line((*x1, *y), (*x2, *y), *width, *color, Some(dash)),
                PaintOp::Link {
                    x1,
                    y1,
                    x2,
                    y2,
                    uri,
                } => canvas.link([*x1, *y1, *x2, *y2], uri),
            }
        }
        doc.add_page(canvas.finish());
    }

    // 一页都没有的文档（空 docx）也要出一页空白，否则 PDF 非法。
    if doc.page_count() == 0 {
        doc.add_page(Canvas::new(page.w_pt, page.h_pt).finish());
    }

    doc.finish()
}
