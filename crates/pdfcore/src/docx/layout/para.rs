//! 段落测量：把一个段落排成与页面位置无关的行框。

use std::ops::Range;

use super::calib::{
    Calib, Decor, EmptyPara, Flow, HangingIndent, HangingPunct, Justify, LineGap, Overflow,
    TrailingSpaces,
};
use super::metrics::{line_box, LineContent};
use super::paginate::StoryPage;
use super::text::{self, Hang, Piece, ShapedPara, TabRules};
use super::PaintOp;
use crate::docx::ir::{self, Align, Grid, LineSpacing};
use crate::docx::model::VAlign;
use crate::fonts::FontBook;

/// 画不出来的对象：浅灰的底、深一点的边，按原大小画，版面不乱。
fn missing_box(x: f32, y: f32, w: f32, h: f32) -> Vec<PaintOp> {
    let corners = [(x, y), (x + w, y), (x + w, y + h), (x, y + h), (x, y)];
    std::iter::once(PaintOp::Rect {
        x,
        y,
        w,
        h,
        color: [0xEE; 3],
    })
    .chain(corners.windows(2).map(|pair| PaintOp::Line {
        from: pair[0],
        to: pair[1],
        width: 0.5,
        color: [0x99; 3],
        dash: Vec::new(),
    }))
    .collect()
}

/// 量一串块（形状里的字）并按给定的各页高度排下去，见 [`flow`](super::paginate::flow)。
pub(super) type MeasureStory<'a> =
    dyn FnMut(&[ir::Block], &Env, &mut FontBook, &[f32]) -> Vec<StoryPage> + 'a;

/// 一个对象画出来的样子：左下角在原点，宽 `w`、高 `h`。
fn object_ops(
    content: &ir::ObjectContent,
    (w, h): (f32, f32),
    env: &Env,
    book: &mut FontBook,
    story: &mut MeasureStory,
) -> Vec<PaintOp> {
    match content {
        ir::ObjectContent::Image { part, crop } => vec![PaintOp::Image {
            part: part.clone(),
            x: 0.0,
            y: 0.0,
            w,
            h,
            crop: *crop,
        }],
        ir::ObjectContent::Shape(shape) => shape_ops(shape, (w, h), env, book, story),
        ir::ObjectContent::Missing { .. } => missing_box(0.0, 0.0, w, h),
    }
}

/// 形状：先填充，再描轮廓（骑在外框上），最后是框里的字 —— 按框宽减去左右边距
/// 排成一栏，在上下边距之间按竖直对齐放。
fn shape_ops(
    shape: &ir::ShapeObject,
    (w, h): (f32, f32),
    env: &Env,
    book: &mut FontBook,
    story: &mut MeasureStory,
) -> Vec<PaintOp> {
    let mut ops = Vec::new();
    if let Some(color) = shape.fill {
        ops.push(PaintOp::Rect {
            x: 0.0,
            y: 0.0,
            w,
            h,
            color,
        });
    }
    if let Some((width, color)) = shape.line {
        let segment = |from, to| PaintOp::Line {
            from,
            to,
            width,
            color,
            dash: Vec::new(),
        };
        match shape.kind {
            // 上下两条边各往外多伸半个线宽，把四个角补齐。
            ir::ShapeKind::Rect => {
                let half = width / 2.0;
                ops.push(segment((-half, 0.0), (w + half, 0.0)));
                ops.push(segment((w, 0.0), (w, h)));
                ops.push(segment((w + half, h), (-half, h)));
                ops.push(segment((0.0, h), (0.0, 0.0)));
            }
            ir::ShapeKind::Line { rising: false } => ops.push(segment((0.0, h), (w, 0.0))),
            ir::ShapeKind::Line { rising: true } => ops.push(segment((0.0, 0.0), (w, h))),
            ir::ShapeKind::TextOnly => {}
        }
    }
    if !shape.text.is_empty() {
        let [top, left, bottom, right] = shape.insets;
        // 框里的字不吸附行网格，行尾标点照样悬挂（与单元格不同），LibreOffice 实测。
        let inner = Env {
            grid: None,
            left: 0.0,
            width: (w - left - right).max(0.0),
            default_tab_stop: env.default_tab_stop,
            char_pitch: None,
            punct_hangs: true,
            calib: env.calib,
        };
        // 放不下时：放得下的行照画；接着的一行顶端还在文字区里就也画，裁到文字区
        // 底边为止；再往后的不画（LibreOffice 实测）。第二页高度为 0：每页只放一行。
        let room = (h - top - bottom).max(0.0);
        let mut pages = story(&shape.text, &inner, book, &[room, 0.0]).into_iter();
        let (mut text, mut height) = pages
            .next()
            .map_or((Vec::new(), 0.0), |p| (p.ops, p.height));
        let overflow = pages.len() > 0;
        if let Some(next) = pages.next().filter(|_| height < room) {
            text.extend(next.ops.iter().map(|op| op.shifted(-height)));
            height += next.height;
        }
        let down = match shape.text_align {
            VAlign::Top => 0.0,
            VAlign::Center => (room - height) / 2.0,
            VAlign::Bottom => room - height,
        }
        .max(0.0);
        let placed = text.iter().map(|op| op.translated(left, h - top - down));
        if overflow || height > room {
            ops.push(PaintOp::Clip {
                x: 0.0,
                y: bottom,
                w,
                h: h - bottom,
            });
            ops.extend(placed);
            ops.push(PaintOp::EndClip);
        } else {
            ops.extend(placed);
        }
    }
    ops
}

/// 测量环境：一栏的横向位置与宽度，以及排版规则。
pub(super) struct Env<'a> {
    pub grid: Option<Grid>,
    /// 栏左边缘的绝对 x（PDF 坐标）。
    pub left: f32,
    pub width: f32,
    /// 默认制表位的间距（点）。
    pub default_tab_stop: f32,
    /// 字符网格的格宽（点），见 [`CharGrid::Cells`](super::calib::CharGrid)。
    pub char_pitch: Option<f32>,
    /// 行尾标点能伸出栏外。单元格里不能，见 [`Tables::Drawn`](super::calib::Tables)。
    pub punct_hangs: bool,
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
    pub keep_next: bool,
    pub keep_lines: bool,
    pub widow_control: bool,
    /// 段落边框与底纹。
    pub decor: Option<ParaDecor>,
    /// 与下一段合成同一个框（边框、底纹、缩进都相同）。排完所有段落后才知道。
    pub joins_next: bool,
    pub body: ParaBody,
    /// 锚在这一段上的浮动对象，放第一行时按页面定位。
    pub floats: Vec<Float>,
    /// 所在的栏：左边缘的 x 与宽度。浮动对象相对栏定位时用。
    pub column: (f32, f32),
}

/// 锚在段落上的浮动对象，连同它画出来的样子（左下角在原点）。
pub(super) struct Float {
    pub object: ir::FloatObject,
    pub ops: Vec<PaintOp>,
}

/// 段落里的行内对象：各自画出来的样子（左下角在原点），以及只有对象的行按什么算
/// 行距 —— 段落标记的自然行高与上伸，见 [`mark_metrics`]。
struct Objects {
    drawn: Vec<Vec<PaintOp>>,
    marks: Option<(f32, f32)>,
}

/// 段落边框与底纹围成的框。横向位置在测量时就定了；纵向由分页决定 ——
/// 框在哪一页开、哪一页收，跨页时两边各自收口。见 [`Decor::Boxes`]。
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ParaDecor {
    /// 框的左、右外沿（绝对 x）。
    pub left: f32,
    pub right: f32,
    pub borders: ir::Borders,
    pub fill: Option<[u8; 3]>,
}

impl ParaDecor {
    /// 框顶到第一行：上边框的厚度加上它与文字的距离。
    pub fn top(&self) -> f32 {
        self.borders.top.map_or(0.0, |b| b.thickness() + b.space)
    }

    /// 最后一行到框底。
    pub fn bottom(&self) -> f32 {
        self.borders.bottom.map_or(0.0, |b| b.thickness() + b.space)
    }

    /// 同一个框里两段之间的分隔线，连同它上下的距离。
    pub fn between(&self) -> f32 {
        self.borders
            .between
            .map_or(0.0, |b| b.space + b.thickness() + b.space)
    }
}

fn decor(para: &ir::Paragraph, env: &Env) -> Option<ParaDecor> {
    let b = para.borders;
    if env.calib.decor == Decor::Ignored || (b == ir::Borders::default() && para.shading.is_none())
    {
        return None;
    }
    // 左边框从缩进（悬挂缩进时是首行的位置）往外：先隔开距离，再是线。
    let indent = para.indent_left.min(para.indent_left + para.first_line);
    let outside = |b: Option<ir::Border>| b.map_or(0.0, |b| b.space + b.thickness());
    Some(ParaDecor {
        left: env.left + indent - outside(b.left),
        right: env.left + env.width - para.indent_right + outside(b.right),
        borders: b,
        fill: para.shading,
    })
}

impl ParaBox {
    /// 各行的高度之和（不含段距）。
    pub fn body_height(&self) -> f32 {
        match &self.body {
            ParaBody::Empty { height } => *height,
            ParaBody::Lines(lines) => lines.iter().map(|l| l.height).sum(),
        }
    }

    /// 单元格、页眉页脚里的段落：分页符不起作用，段落之间的版流控制（与下段同页、
    /// 段中不分页、孤行控制）也不做 —— LibreOffice 拆开单元格时写了 `w:widowControl`
    /// 的两行段落照样一页一行。
    pub fn flatten(&mut self) {
        self.page_break_before = false;
        self.keep_next = false;
        self.keep_lines = false;
        self.widow_control = false;
        if let ParaBody::Lines(lines) = &mut self.body {
            lines.iter_mut().for_each(|l| l.page_break_after = false);
        }
    }

    /// 边框占的高度：上、下边框各自的厚度与距离。
    pub fn decor_height(&self) -> f32 {
        self.decor.as_ref().map_or(0.0, |d| d.top() + d.bottom())
    }

    /// 第一行在页底要容得下的高度。
    pub fn first_line_height(&self) -> f32 {
        match &self.body {
            ParaBody::Empty { height } => *height,
            ParaBody::Lines(lines) => lines.first().map_or(0.0, |l| l.fit_height),
        }
    }
}

/// 量一段。`story` 量形状里的字。
pub(super) fn measure(
    para: &ir::Paragraph,
    env: &Env,
    book: &mut FontBook,
    story: &mut MeasureStory,
) -> ParaBox {
    let shaped = text::shape(para, book, env.calib, env.char_pitch);
    let body = if !shaped.pieces.is_empty() {
        let objects = Objects {
            drawn: para
                .objects
                .iter()
                .map(|o| object_ops(&o.content, (o.width, o.height), env, book, story))
                .collect(),
            // 只有图的行，行距倍数多出来的部分按段落标记的字体算。
            marks: (!para.objects.is_empty())
                .then(|| mark_metrics(para, env, book))
                .flatten(),
        };
        ParaBody::Lines(break_lines(para, &shaped, env, book, &objects))
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
    let flow = env.calib.flow == Flow::Word;
    ParaBox {
        space_before: para.space_before,
        space_after: para.space_after,
        page_break_before: para.page_break_before,
        keep_next: flow && para.keep_next,
        keep_lines: flow && para.keep_lines,
        widow_control: flow && para.widow_control,
        decor: decor(para, env),
        joins_next: false,
        body,
        floats: para
            .floats
            .iter()
            .map(|f| Float {
                object: f.clone(),
                ops: object_ops(&f.content, (f.width, f.height), env, book, story),
            })
            .collect(),
        column: (env.left, env.width),
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

/// 段落标记的自然行高与上伸：按标记的西文字体、字号算，与正文片段同一算法
/// （`Piece::natural_line_pt` / `ascent_pt`）。
fn mark_metrics(para: &ir::Paragraph, env: &Env, book: &mut FontBook) -> Option<(f32, f32)> {
    let mark = &para.mark;
    let font = book.resolve(mark.font_latin.as_deref(), false, mark.bold, mark.italic)?;
    let m = book.face(font.id).metrics();
    let upem = m.upem as f32;
    let unsnapped = (m.default_line_height() * mark.size_pt / upem).max(1.0);
    let gap = if env.calib.line_gap == LineGap::Above {
        m.line_gap as f32
    } else {
        0.0
    };
    Some((unsnapped, (m.ascender as f32 + gap) * mark.size_pt / upem))
}

/// 空段落的那一行：只有段落标记，行高按标记的西文字体、字号算，见 [`EmptyPara::MarkLine`]。
fn mark_line(para: &ir::Paragraph, env: &Env, book: &mut FontBook) -> Option<Line> {
    let (unsnapped, ascent) = mark_metrics(para, env, book)?;
    let b = line_box(
        LineContent::text(unsnapped, ascent),
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
fn hang(para: &ir::Paragraph, env: &Env) -> Hang {
    Hang {
        spaces: env.calib.trailing_spaces == TrailingSpaces::Hang,
        punct: para.overflow_punct
            && env.punct_hangs
            && env.calib.hanging_punct == HangingPunct::Punctuation,
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
fn first_line_offset(para: &ir::Paragraph, sp: &ShapedPara, is_first: bool, calib: &Calib) -> f32 {
    match (is_first, calib.hanging_indent) {
        (false, _) => 0.0,
        (true, HangingIndent::Legacy) => para.first_line.max(0.0),
        (true, HangingIndent::Outdent) => para.first_line + number_shift(para, sp),
    }
}

/// 编号以首行起点为锚点对齐（`w:lvlJc`）：右对齐时整个编号在起点左边，居中时
/// 骑在起点上 —— 首行的起点相应左移。LibreOffice 实测如此，后面的制表符照旧跳到
/// 左缩进处。
fn number_shift(para: &ir::Paragraph, sp: &ShapedPara) -> f32 {
    let Some(n) = para.number else {
        return 0.0;
    };
    let w = sp.width(0, n.len);
    match n.align {
        Align::Right => -w,
        Align::Center => -w / 2.0,
        _ => 0.0,
    }
}

/// 一行的起点，从正文区左缘量起。
fn line_start(para: &ir::Paragraph, sp: &ShapedPara, is_first: bool, calib: &Calib) -> f32 {
    para.indent_left + first_line_offset(para, sp, is_first, calib)
}

fn break_lines(
    para: &ir::Paragraph,
    sp: &ShapedPara,
    env: &Env,
    book: &FontBook,
    objects: &Objects,
) -> Vec<Line> {
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
            (true, HangingIndent::Outdent) => {
                avail_rest - first_line_offset(para, sp, true, env.calib)
            }
        };
        let (end, mandatory) = sp.next_break(
            start,
            avail,
            hang(para, env),
            line_start(para, sp, is_first, env.calib),
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
            objects,
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
    objects: &Objects,
) -> Line {
    // 行内实际出现的片段决定行高：取最大的那个字体。行内对象另算，见 `LineContent`。
    let active = sp.pieces_in(range.start, range.end);
    let text = || active.iter().filter(|p| p.object.is_none());
    let object = active
        .iter()
        .filter_map(|p| p.object.map(|o| o.height))
        .fold(0.0f32, f32::max);
    let has_text = text().next().is_some();
    let content = if has_text || object == 0.0 {
        let unsnapped = text()
            .map(|p| p.natural_line_pt(book))
            .fold(0.0f32, f32::max)
            .max(1.0);
        let ascent = text()
            .map(|p| p.ascent_pt(book, env.calib.line_gap == LineGap::Above))
            .fold(0.0f32, f32::max);
        LineContent {
            unsnapped,
            ascent,
            object,
            has_text: true,
        }
    } else {
        // 只有对象的行：行距倍数多出来的部分按段落标记的字体算。
        let (unsnapped, ascent) = objects.marks.unwrap_or((object, object));
        LineContent {
            unsnapped,
            ascent,
            object,
            has_text: false,
        }
    };
    let metrics = line_box(
        content,
        env.grid,
        para.snap_to_grid,
        para.line,
        is_last,
        env.calib,
    );

    // 悬挂在右边距外的行尾空格、标点不参与对齐。
    let measured = range.start..sp.measured_end(range.start, range.end, hang(para, env));
    let rules = tab_rules(para, env);
    let has_tabs = sp.tabs.iter().any(|t| range.contains(t));
    let line_width = if has_tabs {
        let x0 = line_start(para, sp, is_first, env.calib);
        sp.advance(measured.start, measured.end, x0, &rules) - x0
    } else {
        sp.width(measured.start, measured.end)
    };
    let content_left = env.left + para.indent_left;
    let avail = env.width - para.indent_left - para.indent_right;
    let indent = first_line_offset(para, sp, is_first, env.calib);

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
        if let Some(obj) = piece.object {
            let extra: f32 = extra_after.iter().sum();
            let drawn = &objects.drawn[obj.index];
            ops.extend(drawn.iter().map(|op| op.translated(x, piece.rise)));
            x += obj.width + extra;
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
        // `w` 已含字符间距；`extra` 只是两端对齐分到的。画字时两者都要进 TJ。
        let w = piece.width(range.start, range.end);
        let gr = piece.glyph_range(range.start, range.end);
        let extra: f32 = extra_after.iter().sum();
        let extra_after: Vec<f32> = extra_after
            .iter()
            .enumerate()
            .map(|(k, e)| e + piece.letter_spacing_after(gr.start + k))
            .collect();

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

        if let Some(uri) = &piece.link {
            let m = book.face(piece.font).metrics();
            let scale = piece.size_pt / m.upem as f32;
            let (bottom, top) = (m.descender as f32 * scale, m.ascender as f32 * scale);
            // 同一行上紧挨着的同一个链接合成一块：一个链接常被格式切成好几段。
            match ops
                .iter_mut()
                .rev()
                .find(|o| matches!(o, PaintOp::Link { .. }))
            {
                Some(PaintOp::Link {
                    x2,
                    uri: last,
                    y1,
                    y2,
                    ..
                }) if last == uri && (*x2 - x).abs() < 0.5 => {
                    *x2 = x + w + extra;
                    *y1 = y1.min(bottom);
                    *y2 = y2.max(top);
                }
                _ => ops.push(PaintOp::Link {
                    x1: x,
                    y1: bottom,
                    x2: x + w + extra,
                    y2: top,
                    uri: uri.clone(),
                }),
            }
        }
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
        from: (x, y + width / 2.0),
        to: (x + w, y + width / 2.0),
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
