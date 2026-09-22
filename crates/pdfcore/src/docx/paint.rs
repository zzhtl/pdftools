//! 把排好版的页面画成 PDF。

use std::collections::BTreeMap;

use pdf_writer::Name;

use super::ir::PageGeom;
use super::layout::{FontBook, LaidOut, PaintOp};
use crate::error::Result;
use crate::pdf::writer::font::{embed_font, EmbeddedFont};
use crate::pdf::writer::text::{show_text, TextItem};
use crate::pdf::writer::{DocBuilder, PageSpec};

pub fn paint(
    laid: &LaidOut,
    page: &PageGeom,
    book: &FontBook,
    info: crate::pdf::writer::DocInfo,
) -> Result<Vec<u8>> {
    // 第一趟：把每个字体实际用到的字形收齐。子集化必须一次性知道全部用量，
    // 所以字体只能等内容全部排完才能写。
    let mut used: BTreeMap<usize, BTreeMap<u16, String>> = BTreeMap::new();
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

    let mut doc = DocBuilder::new();
    doc.set_info(info);
    let mut embedded: BTreeMap<usize, EmbeddedFont> = BTreeMap::new();
    for (font_id, glyphs) in &used {
        let face = book.face(*font_id);
        let (pdf, alloc) = doc.parts();
        embedded.insert(*font_id, embed_font(pdf, alloc, face, glyphs)?);
    }

    for laid_page in &laid.pages {
        let mut spec = PageSpec::new(page.w_pt, page.h_pt);

        // 本页用到哪些字体，就挂哪些资源。
        let mut used_here: Vec<usize> = laid_page
            .ops
            .iter()
            .filter_map(|op| match op {
                PaintOp::Text { font, .. } => Some(*font),
                _ => None,
            })
            .collect();
        used_here.sort_unstable();
        used_here.dedup();
        for font_id in &used_here {
            if let Some(e) = embedded.get(font_id) {
                spec.fonts.push((font_res_name(*font_id), e.font_ref));
            }
        }

        for op in &laid_page.ops {
            match op {
                PaintOp::Text {
                    font,
                    size_pt,
                    x,
                    y,
                    glyphs,
                    color,
                    char_spacing,
                    word_spacing,
                    ..
                } => {
                    let Some(e) = embedded.get(font) else {
                        continue;
                    };
                    let name = font_res_name(*font);
                    show_text(
                        &mut spec.content,
                        &TextItem {
                            glyphs,
                            font_res: &name,
                            size_pt: *size_pt,
                            x_pt: *x,
                            y_pt: *y,
                            color: *color,
                            char_spacing: *char_spacing,
                            word_spacing: *word_spacing,
                        },
                        book.face(*font),
                        &e.map,
                    );
                }
                PaintOp::Rect { x, y, w, h, color } => {
                    spec.content.save_state();
                    spec.content.set_fill_rgb(
                        color[0] as f32 / 255.0,
                        color[1] as f32 / 255.0,
                        color[2] as f32 / 255.0,
                    );
                    spec.content.rect(*x, *y, *w, *h);
                    spec.content.fill_nonzero();
                    spec.content.restore_state();
                }
            }
        }

        doc.add_page(spec);
    }

    // 一页都没有的文档（空 docx）也要出一页空白，否则 PDF 非法。
    if doc.page_count() == 0 {
        doc.add_page(PageSpec::new(page.w_pt, page.h_pt));
    }

    let _ = Name(b"");
    doc.finish()
}

fn font_res_name(id: usize) -> String {
    format!("F{id}")
}
