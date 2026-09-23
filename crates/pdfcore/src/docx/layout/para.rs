//! 段落测量：把一个段落排成与页面位置无关的行框。

use std::ops::Range;

use super::calib::{Calib, EmptyPara, HangingPunct, TrailingSpaces};
use super::metrics::line_box;
use super::text::{self, Hang, ShapedPara};
use super::PaintOp;
use crate::docx::ir::{self, Align, Grid, LineSpacing};
use crate::fonts::FontBook;

/// 测量环境：一栏的横向位置与宽度，以及排版规则。
pub(super) struct Env<'a> {
    pub grid: Option<Grid>,
    /// 栏左边缘的绝对 x（PDF 坐标）。
    pub left: f32,
    pub width: f32,
    pub calib: &'a Calib,
}

/// 排好的一行。`ops` 的 x 是绝对坐标，y 相对于本行基线。
pub(super) struct Line {
    pub height: f32,
    /// 基线到行框顶部的距离。
    pub baseline: f32,
    /// 页底要容得下的高度（不超过 `height`）。
    pub fit_height: f32,
    pub ops: Vec<PaintOp>,
}

pub(super) enum ParaBody {
    /// 没有任何文字的段落。按旧规则它不参与放不放得下的判断。
    Empty {
        height: f32,
    },
    Lines(Vec<Line>),
}

pub(super) struct ParaBox {
    pub space_before: f32,
    pub space_after: f32,
    pub page_break_before: bool,
    pub body: ParaBody,
}

pub(super) fn measure(para: &ir::Paragraph, env: &Env, book: &mut FontBook) -> ParaBox {
    let shaped = text::shape(para, book);
    let body = if !shaped.pieces.is_empty() {
        ParaBody::Lines(break_lines(para, &shaped, env, book))
    } else {
        match env.calib.empty_para {
            EmptyPara::MarkLine => match mark_line(para, env, book) {
                Some(line) => ParaBody::Lines(vec![line]),
                // 系统里一个字体都没有，量不出行高。
                None => ParaBody::Empty {
                    height: legacy_empty_height(para),
                },
            },
            EmptyPara::Legacy => ParaBody::Empty {
                height: legacy_empty_height(para),
            },
        }
    };
    ParaBox {
        space_before: para.space_before,
        space_after: para.space_after,
        page_break_before: para.page_break_before,
        body,
    }
}

fn legacy_empty_height(para: &ir::Paragraph) -> f32 {
    let base = para.spans.first().map(|s| s.style.size_pt).unwrap_or(10.5) * 1.2;
    match para.line {
        LineSpacing::Multiple(m) => base * m,
        LineSpacing::Exact(pt) => pt,
        LineSpacing::AtLeast(pt) => base.max(pt),
    }
}

/// 空段落的那一行：只有段落标记，行高按标记的西文字体、字号算，见 [`EmptyPara::MarkLine`]。
fn mark_line(para: &ir::Paragraph, env: &Env, book: &mut FontBook) -> Option<Line> {
    let mark = &para.mark;
    let font = book.resolve(mark.font_latin.as_deref(), false, mark.bold, mark.italic)?;
    let m = book.face(font.id).metrics();
    let upem = m.upem as f32;
    // 与正文片段的自然行高、上伸同一算法（`Piece::natural_line_pt` / `ascent_pt`）。
    let unsnapped = (m.default_line_height() * mark.size_pt / upem).max(1.0);
    let ascent = m.ascender as f32 * mark.size_pt / upem;
    let b = line_box(
        unsnapped,
        ascent,
        env.grid,
        para.snap_to_grid,
        para.line,
        true,
        env.calib,
    );
    Some(Line {
        height: b.height,
        baseline: b.baseline,
        fit_height: b.fit_height,
        ops: Vec::new(),
    })
}

/// 行尾哪些东西可以悬挂在右边距外。
fn hang(para: &ir::Paragraph, calib: &Calib) -> Hang {
    Hang {
        spaces: calib.trailing_spaces == TrailingSpaces::Hang,
        punct: para.overflow_punct && calib.hanging_punct == HangingPunct::Punctuation,
    }
}

fn break_lines(para: &ir::Paragraph, sp: &ShapedPara, env: &Env, book: &FontBook) -> Vec<Line> {
    let avail_first = env.width - para.indent_left - para.indent_right + para.first_line.min(0.0);
    let avail_rest = env.width - para.indent_left - para.indent_right;
    let first_indent = para.first_line.max(0.0);

    let mut lines = Vec::new();
    let mut start = 0usize;
    let mut is_first = true;
    while start < sp.text.len() {
        let avail = if is_first {
            avail_first - first_indent
        } else {
            avail_rest
        };
        let (end, mandatory) = sp.next_break(start, avail, hang(para, env.calib));
        let is_last = end >= sp.text.len();
        lines.push(line(
            para,
            sp,
            start..end,
            env,
            book,
            is_first,
            is_last || mandatory,
            is_last,
        ));
        start = end;
        is_first = false;
    }
    lines
}

#[allow(clippy::too_many_arguments)]
fn line(
    para: &ir::Paragraph,
    sp: &ShapedPara,
    range: Range<usize>,
    env: &Env,
    book: &FontBook,
    is_first: bool,
    suppress_justify: bool,
    // 本行是不是所属段落的最后一行 —— 决定倍数行距怎么算，见 `line_box`。
    is_last: bool,
) -> Line {
    // 行内实际出现的片段决定行高：取最大的那个字体。
    let active = sp.pieces_in(range.start, range.end);
    let unsnapped = active
        .iter()
        .map(|p| p.natural_line_pt(book))
        .fold(0.0f32, f32::max)
        .max(1.0);
    let ascent = active
        .iter()
        .map(|p| p.ascent_pt(book))
        .fold(0.0f32, f32::max);
    let metrics = line_box(
        unsnapped,
        ascent,
        env.grid,
        para.snap_to_grid,
        para.line,
        is_last,
        env.calib,
    );

    // 悬挂在右边距外的行尾空格、标点不参与对齐。
    let measured = range.start..sp.measured_end(range.start, range.end, hang(para, env.calib));
    let line_width = sp.width(measured.start, measured.end);
    let content_left = env.left + para.indent_left;
    let avail = env.width - para.indent_left - para.indent_right;
    let indent = if is_first {
        para.first_line.max(0.0)
    } else {
        0.0
    };

    let mut x = match para.align {
        Align::Left | Align::Justify => content_left + indent,
        Align::Center => content_left + indent + (avail - indent - line_width) / 2.0,
        Align::Right => content_left + avail - line_width,
    };

    // 两端对齐：把剩余空间摊进字间。段落最后一行不参与。
    //
    // 含半角空格的行暂不拉开：旧版把空间全交给词距（Tw），而 Tw 对双字节编码
    // 从来不起作用，这些行实际一直是左对齐的。直接让词距生效会把整行的空间压到
    // 一两个空格上（中西文混排里常见），比左对齐更难看。正确的分配规则（摊到每个
    // 中文字之间，空格也分一份）要对着 LibreOffice 实测来定。
    let mut char_spacing = 0.0f32;
    if para.align == Align::Justify && !suppress_justify {
        let slack = avail - indent - line_width;
        let has_space = sp.text[measured.clone()].contains(' ');
        if slack > 0.0 && !has_space {
            let glyphs: usize = active
                .iter()
                .map(|p| p.glyphs_between(measured.start, measured.end).len())
                .sum();
            if glyphs > 1 {
                char_spacing = slack / (glyphs - 1) as f32;
            }
        }
    }
    // 行尾有悬挂的内容时，它紧挨着最后一个可见的字伸出边距：
    // 从最后一个可见的字起不再加字距，否则悬挂的标点会离开前一个字。
    let hanging_from = (measured.end < range.end)
        .then(|| sp.text[..measured.end].char_indices().next_back())
        .flatten()
        .map(|(i, _)| i);

    let mut ops = Vec::new();
    for piece in active {
        let glyphs = piece.glyphs_between(range.start, range.end);
        if glyphs.is_empty() {
            continue;
        }
        // 中西文间距：只有本片确实从行中间开始时才推进，
        // 行首的那个间距应当被吃掉，否则整行会往右偏。
        if piece.range.start > range.start && piece.range.start < range.end {
            x += piece.gap_before;
        }
        let w = piece.width(range.start, range.end);
        let gr = piece.glyph_range(range.start, range.end);
        let extra_after: Vec<f32> = match hanging_from {
            None => vec![char_spacing; glyphs.len()],
            Some(from) => glyphs
                .iter()
                .map(|g| {
                    if piece.range.start + g.cluster as usize >= from {
                        0.0
                    } else {
                        char_spacing
                    }
                })
                .collect(),
        };
        let extra: f32 = extra_after.iter().sum();

        ops.push(PaintOp::Text {
            font: piece.font,
            size_pt: piece.size_pt,
            x,
            y: 0.0,
            glyphs: glyphs.to_vec(),
            unicode: piece.texts[gr].to_vec(),
            color: piece.color,
            extra_after,
            synthetic_bold: piece.synthetic_bold,
            synthetic_italic: piece.synthetic_italic,
        });

        let thickness = (piece.size_pt * 0.05).max(0.5);
        if piece.underline {
            ops.push(PaintOp::Rect {
                x,
                y: -(piece.size_pt * 0.12),
                w: w + extra,
                h: thickness,
                color: piece.color,
            });
        }
        if piece.strike {
            ops.push(PaintOp::Rect {
                x,
                y: piece.size_pt * 0.26,
                w: w + extra,
                h: thickness,
                color: piece.color,
            });
        }
        x += w + extra;
    }

    Line {
        height: metrics.height,
        baseline: metrics.baseline,
        fit_height: metrics.fit_height,
        ops,
    }
}
