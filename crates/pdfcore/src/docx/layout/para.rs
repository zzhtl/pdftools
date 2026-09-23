//! 段落测量：把一个段落排成与页面位置无关的行框。

use std::ops::Range;

use super::calib::{
    Breaks, Calib, EmptyPara, HangingIndent, HangingPunct, Justify, Overflow, Tabs, TrailingSpaces,
};
use super::metrics::line_box;
use super::text::{self, Hang, Piece, ShapedPara, TabRules};
use super::PaintOp;
use crate::docx::ir::{self, Align, Grid, LineSpacing};
use crate::fonts::FontBook;

/// 测量环境：一栏的横向位置与宽度，以及排版规则。
pub(super) struct Env<'a> {
    pub grid: Option<Grid>,
    /// 栏左边缘的绝对 x（PDF 坐标）。
    pub left: f32,
    pub width: f32,
    /// 默认制表位的间距（点）。
    pub default_tab_stop: f32,
    pub calib: &'a Calib,
}

/// 排好的一行。`ops` 的 x 是绝对坐标，y 相对于本行基线。
pub(super) struct Line {
    pub height: f32,
    /// 基线到行框顶部的距离。
    pub baseline: f32,
    /// 页底要容得下的高度（不超过 `height`）。
    pub fit_height: f32,
    /// 本行以分页符结尾：放下之后换页。
    pub page_break_after: bool,
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
    let shaped = text::shape(
        para,
        book,
        env.calib.breaks == Breaks::Typed,
        env.calib.tabs == Tabs::Stops,
    );
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
        page_break_after: false,
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

fn tab_rules<'a>(para: &'a ir::Paragraph, env: &Env) -> TabRules<'a> {
    TabRules {
        stops: &para.tabs,
        default: env.default_tab_stop,
        implicit: (para.first_line < 0.0).then_some(para.indent_left),
    }
}

/// 一行的起点相对左缩进的偏移：首行缩进，或者悬挂缩进往左伸出的量。
fn first_line_offset(para: &ir::Paragraph, is_first: bool, calib: &Calib) -> f32 {
    match (is_first, calib.hanging_indent) {
        (false, _) => 0.0,
        (true, HangingIndent::Legacy) => para.first_line.max(0.0),
        (true, HangingIndent::Outdent) => para.first_line,
    }
}

/// 一行的起点，从正文区左缘量起。
fn line_start(para: &ir::Paragraph, is_first: bool, calib: &Calib) -> f32 {
    para.indent_left + first_line_offset(para, is_first, calib)
}

fn break_lines(para: &ir::Paragraph, sp: &ShapedPara, env: &Env, book: &FontBook) -> Vec<Line> {
    let avail_first = env.width - para.indent_left - para.indent_right + para.first_line.min(0.0);
    let avail_rest = env.width - para.indent_left - para.indent_right;
    let first_indent = para.first_line.max(0.0);
    let rules = tab_rules(para, env);

    let mut lines = Vec::new();
    let mut start = 0usize;
    let mut is_first = true;
    while start < sp.text.len() {
        let avail = match (is_first, env.calib.hanging_indent) {
            (false, _) => avail_rest,
            (true, HangingIndent::Legacy) => avail_first - first_indent,
            (true, HangingIndent::Outdent) => avail_rest - para.first_line,
        };
        let (end, mandatory) = sp.next_break(
            start,
            avail,
            hang(para, env.calib),
            line_start(para, is_first, env.calib),
            &rules,
            env.calib.overflow == Overflow::CharBoundary,
        );
        let is_last = end >= sp.text.len();
        let mut l = line(
            para,
            sp,
            start..end,
            env,
            book,
            is_first,
            is_last || mandatory,
            is_last,
        );
        l.page_break_after = mandatory
            && sp.text[start..end]
                .chars()
                .next_back()
                .is_some_and(text::is_page_break);
        lines.push(l);
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
    let rules = tab_rules(para, env);
    let has_tabs = sp.tabs.iter().any(|t| range.contains(t));
    let line_width = if has_tabs {
        let x0 = line_start(para, is_first, env.calib);
        sp.advance(measured.start, measured.end, x0, &rules) - x0
    } else {
        sp.width(measured.start, measured.end)
    };
    let content_left = env.left + para.indent_left;
    let avail = env.width - para.indent_left - para.indent_right;
    let indent = first_line_offset(para, is_first, env.calib);

    let mut x = match para.align {
        Align::Left | Align::Justify => content_left + indent,
        Align::Center => content_left + indent + (avail - indent - line_width) / 2.0,
        Align::Right => content_left + avail - line_width,
    };

    // 两端对齐：把剩余空间摊进字间。段落最后一行（以及换行符结束的行）不参与；
    // 有制表符的行也不参与 —— 制表位把文字钉在了固定位置上。
    let slack = (para.align == Align::Justify && !suppress_justify && !has_tabs)
        .then_some(avail - indent - line_width)
        .filter(|s| *s > 0.0);
    let mut extras = match env.calib.justify {
        Justify::Legacy => legacy_extras(sp, active, &range, &measured, slack),
        Justify::Gaps => gap_extras(active, &range, &measured, slack),
    }
    .into_iter();

    // 跳制表位要知道当前 x 离正文区左缘多远；居中、右对齐的偏移整体加在后面。
    let shift = x - (content_left + indent);
    let mut ops = Vec::new();
    // 突出显示、底纹要画在文字底下。
    let mut backgrounds = Vec::new();
    for piece in active {
        let extra_after = extras.next().unwrap_or_default();
        if has_tabs && &sp.text[piece.range.clone()] == "\t" {
            let seg_end = sp
                .tabs
                .iter()
                .copied()
                .find(|t| *t > piece.range.start)
                .unwrap_or(measured.end)
                .min(measured.end)
                .max(piece.range.end);
            let from = x - shift - env.left;
            let (to, stop) = sp.tab_target(from, piece.range.start, seg_end, &rules);
            if let Some(op) = leader(piece, stop.leader, from, to, env.left + shift, book) {
                ops.push(op);
            }
            x = env.left + shift + to;
            continue;
        }
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
        let extra: f32 = extra_after.iter().sum();

        ops.push(PaintOp::Text {
            font: piece.font,
            size_pt: piece.size_pt,
            x,
            y: piece.rise,
            glyphs: glyphs.to_vec(),
            unicode: piece.texts[gr].to_vec(),
            color: piece.color,
            extra_after,
            synthetic_bold: piece.synthetic_bold,
            synthetic_italic: piece.synthetic_italic,
        });

        if let Some(bg) = piece.background {
            let m = book.face(piece.font).metrics();
            let scale = piece.size_pt / m.upem as f32;
            backgrounds.push(PaintOp::Rect {
                x,
                y: m.descender as f32 * scale,
                w: w + extra,
                h: (m.ascender - m.descender) as f32 * scale,
                color: bg,
            });
        }
        let thickness = (piece.size_pt * 0.05).max(0.5);
        if let Some(u) = piece.underline {
            decorate(&mut ops, u, x, w + extra, piece.size_pt, thickness);
        }
        if piece.strike || piece.double_strike {
            let ys: &[f32] = if piece.double_strike {
                &[0.2, 0.32]
            } else {
                &[0.26]
            };
            for &k in ys {
                ops.push(PaintOp::Rect {
                    x,
                    y: piece.size_pt * k,
                    w: w + extra,
                    h: thickness,
                    color: piece.color,
                });
            }
        }
        x += w + extra;
    }

    backgrounds.append(&mut ops);
    Line {
        height: metrics.height,
        baseline: metrics.baseline,
        fit_height: metrics.fit_height,
        page_break_after: false,
        ops: backgrounds,
    }
}

/// 画一段下划线。`x`、`w` 是这段文字的起点与宽度，y 相对基线。
fn decorate(ops: &mut Vec<PaintOp>, u: ir::Underline, x: f32, w: f32, size: f32, t: f32) {
    use ir::UnderlineStyle as U;
    let y = -(size * 0.12);
    let rect = |y: f32, h: f32| PaintOp::Rect {
        x,
        y,
        w,
        h,
        color: u.color,
    };
    let dashed = |width: f32, dash: Vec<f32>| PaintOp::Line {
        x1: x,
        x2: x + w,
        y: y + width / 2.0,
        width,
        color: u.color,
        dash,
    };
    match u.style {
        U::None => {}
        // 只划字不划空格、波浪线：都近似成单线。
        U::Single | U::Words | U::Wave => ops.push(rect(y, t)),
        U::WavyHeavy | U::Thick => ops.push(rect(y - t / 2.0, t * 2.0)),
        U::Double | U::WavyDouble => {
            ops.push(rect(y, t));
            ops.push(rect(y - t * 2.0, t));
        }
        U::Dotted => ops.push(dashed(t, vec![t, t * 2.0])),
        U::DottedHeavy => ops.push(dashed(t * 2.0, vec![t * 2.0, t * 2.0])),
        U::Dash => ops.push(dashed(t, vec![t * 4.0, t * 2.0])),
        U::DashedHeavy => ops.push(dashed(t * 2.0, vec![t * 4.0, t * 2.0])),
        U::DashLong => ops.push(dashed(t, vec![t * 8.0, t * 3.0])),
        U::DashLongHeavy => ops.push(dashed(t * 2.0, vec![t * 8.0, t * 3.0])),
        U::DotDash => ops.push(dashed(t, vec![t * 4.0, t * 2.0, t, t * 2.0])),
        U::DashDotHeavy => ops.push(dashed(t * 2.0, vec![t * 4.0, t * 2.0, t, t * 2.0])),
        U::DotDotDash => ops.push(dashed(t, vec![t * 4.0, t * 2.0, t, t * 2.0, t, t * 2.0])),
        U::DashDotDotHeavy => ops.push(dashed(
            t * 2.0,
            vec![t * 4.0, t * 2.0, t, t * 2.0, t, t * 2.0],
        )),
    }
}

/// 重写前的两端对齐：含半角空格的行不拉开，其余的行平均分给每个字形之后。
/// 返回与 `active` 一一对应、每片每个字形之后的额外推进。
fn legacy_extras(
    sp: &ShapedPara,
    active: &[Piece],
    range: &Range<usize>,
    measured: &Range<usize>,
    slack: Option<f32>,
) -> Vec<Vec<f32>> {
    let mut char_spacing = 0.0f32;
    if let Some(slack) = slack {
        if !sp.text[measured.clone()].contains(' ') {
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
    active
        .iter()
        .map(|piece| {
            let glyphs = piece.glyphs_between(range.start, range.end);
            match hanging_from {
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
            }
        })
        .collect()
}

/// 见 [`Justify::Gaps`]。只在行内可见的部分里分；悬挂的空格、标点不分，
/// 最后一个可见字形之后也不分。
fn gap_extras(
    active: &[Piece],
    range: &Range<usize>,
    measured: &Range<usize>,
    slack: Option<f32>,
) -> Vec<Vec<f32>> {
    let mut extras: Vec<Vec<f32>> = active
        .iter()
        .map(|p| vec![0.0; p.glyphs_between(range.start, range.end).len()])
        .collect();
    let Some(slack) = slack else {
        return extras;
    };
    // 可见字形，按排列顺序：(第几片, 片内第几个, 是汉字, 是空格)。
    let mut visible = Vec::new();
    for (pi, piece) in active.iter().enumerate() {
        let gr = piece.glyph_range(range.start, range.end);
        for (k, g) in piece.shaped.glyphs[gr.clone()].iter().enumerate() {
            if piece.range.start + g.cluster as usize >= measured.end {
                continue;
            }
            let space = piece.texts[gr.start + k] == " ";
            let cjk = piece.class == crate::fonts::ScriptClass::EastAsian && !space;
            visible.push((pi, k, cjk, space));
        }
    }
    let targets: Vec<(usize, usize)> = if visible.iter().any(|g| g.3) {
        visible.iter().filter(|g| g.3).map(|g| (g.0, g.1)).collect()
    } else {
        visible
            .windows(2)
            .filter(|w| w[0].2 || w[1].2)
            .map(|w| (w[0].0, w[0].1))
            .collect()
    };
    if !targets.is_empty() {
        let share = slack / targets.len() as f32;
        for (pi, k) in targets {
            extras[pi][k] = share;
        }
    }
    extras
}

/// 制表符前导符：从 `from` 到 `to`（从正文区左缘量起）填满一串同样的字符，右端对齐到 `to`。
fn leader(
    piece: &Piece,
    kind: ir::TabLeader,
    from: f32,
    to: f32,
    origin: f32,
    book: &FontBook,
) -> Option<PaintOp> {
    let c = match kind {
        ir::TabLeader::None => return None,
        ir::TabLeader::Dot => ".",
        ir::TabLeader::Hyphen => "-",
        ir::TabLeader::Underscore | ir::TabLeader::Heavy => "_",
        ir::TabLeader::MiddleDot => "·",
    };
    let face = book.face(piece.font);
    let shaped = crate::fonts::shape_run(face, c, rustybuzz::script::LATIN);
    let g = *shaped.glyphs.first()?;
    let advance = g.x_advance as f32 * piece.size_pt / face.metrics().upem as f32;
    if advance <= 0.0 {
        return None;
    }
    let n = ((to - from) / advance).floor() as usize;
    if n == 0 {
        return None;
    }
    Some(PaintOp::Text {
        font: piece.font,
        size_pt: piece.size_pt,
        x: origin + to - n as f32 * advance,
        y: 0.0,
        glyphs: vec![g; n],
        unicode: vec![c.to_string(); n],
        color: piece.color,
        extra_after: vec![0.0; n],
        synthetic_bold: piece.synthetic_bold,
        synthetic_italic: piece.synthetic_italic,
    })
}
