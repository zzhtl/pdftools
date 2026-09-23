//! 排版：IR → 绝对定位的页面。
//!
//! 分两层：
//! - **测量**（[`para`]）：把一个块排成与页面位置无关的行框 —— 每行多高、基线在哪、
//!   画什么。横向坐标在这一步就定了（栏宽不随页变），纵向坐标相对于本行基线。
//! - **分页**（[`paginate`]）：按顺序把行框放进页面，决定在哪里换页。
//!
//! 分开以后，「一段放不下时拆在哪」「keepNext 要不要整段挪走」这类判断只读测量结果，
//! 不需要回滚已经画到页面上的东西；表格单元格、页眉页脚也用同一套测量。

mod calib;
mod metrics;
mod paginate;
mod para;
mod script;
mod table;
mod text;

pub use calib::{
    AutoSpace, Breaks, Calib, Cascade, CharClass, CharGrid, Decor, EmptyPara, FixedBaseline, Flow,
    GridLayout, HangingIndent, HangingPunct, HeaderFooter, Images, Justify, Kerning, LineGap,
    ListNumbers, Overflow, PageBottom, PageBreakBefore, ParaSpacing, RunFormat, Sections, Tables,
    Tabs, Theme, TrailingSpaces,
};

use super::ir;
use crate::error::{Warning, WarningKind};
use crate::fonts::{FontBook, FontId, ShapedGlyph};

#[derive(Debug, Clone)]
pub enum PaintOp {
    Text {
        font: FontId,
        size_pt: f32,
        x: f32,
        /// 基线的 y，PDF 坐标（原点在页面左下角）。
        y: f32,
        glyphs: Vec<ShapedGlyph>,
        /// 与 `glyphs` 一一对应的原文片段，用于构造 `/ToUnicode`。
        /// 必须来自整形的 cluster 回查，反查 cmap 在连字和多对一映射上是错的。
        unicode: Vec<String>,
        color: [u8; 3],
        /// 每个字形之后额外的推进（点）：两端对齐分到这个字形的份额。
        extra_after: Vec<f32>,
        synthetic_bold: bool,
        synthetic_italic: bool,
    },
    /// 描边的线：段落边框、表格框线，以及虚线、点线的下划线。`dash` 是 PDF 的虚线
    /// 样式（线段、间隔交替），空的是实线。
    Line {
        from: (f32, f32),
        to: (f32, f32),
        width: f32,
        color: [u8; 3],
        dash: Vec<f32>,
    },
    /// 可点击的区域：超链接。y 与其他操作一样，测量时相对基线。
    Link {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        uri: String,
    },
    /// 图片：`part` 是包里的图片部件，(`x`, `y`) 是显示框的左下角（测量时 y 与其他
    /// 操作一样相对基线），`crop` 是左、上、右、下各裁掉的比例。
    Image {
        part: String,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        crop: [f32; 4],
    },
    /// 下划线、删除线、底色、占位框的边都用矩形画。
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: [u8; 3],
    },
    /// 之后的操作只在这个矩形里可见，直到配对的 [`PaintOp::EndClip`]。两者在同一串
    /// 操作里成对出现（文本框里放不下的字）。
    Clip {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    },
    EndClip,
}

impl PaintOp {
    /// 纵向平移。测量时 y 相对于行的基线，放进页面时才加上基线的绝对位置。
    fn shifted(&self, dy: f32) -> PaintOp {
        self.translated(0.0, dy)
    }

    /// 平移：对象先按左下角在原点画好，放进行里、页上时再挪过去。
    fn translated(&self, dx: f32, dy: f32) -> PaintOp {
        let mut op = self.clone();
        match &mut op {
            PaintOp::Text { x, y, .. }
            | PaintOp::Rect { x, y, .. }
            | PaintOp::Image { x, y, .. }
            | PaintOp::Clip { x, y, .. } => {
                *x += dx;
                *y += dy;
            }
            PaintOp::Line { from, to, .. } => {
                *from = (from.0 + dx, from.1 + dy);
                *to = (to.0 + dx, to.1 + dy);
            }
            PaintOp::Link { x1, y1, x2, y2, .. } => {
                *x1 += dx;
                *x2 += dx;
                *y1 += dy;
                *y2 += dy;
            }
            PaintOp::EndClip => {}
        }
        op
    }
}

#[derive(Debug, Clone)]
pub struct Page {
    /// 画在正文下面的（衬于文字下方的浮动图）。
    pub under: Vec<PaintOp>,
    pub ops: Vec<PaintOp>,
    /// 画在正文上面的（浮于文字上方的浮动图）。
    pub over: Vec<PaintOp>,
    /// 纸张大小（点）：(宽, 高)。各节可以不同。
    pub size: (f32, f32),
    /// 页码：显示出来的那个数，不一定等于第几页。
    pub number: i32,
    /// 属于第几节。
    pub section: usize,
    /// 用哪一类页眉页脚。
    pub kind: PageKind,
}

/// 一页用哪一类页眉页脚。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageKind {
    Default,
    /// 本节首页（`w:titlePg`）。
    First,
    /// 偶数页（`w:evenAndOddHeaders`）。
    Even,
}

impl Page {
    fn new(page: &ir::PageGeom, number: i32, section: usize, kind: PageKind) -> Self {
        Self {
            under: Vec::new(),
            ops: Vec::new(),
            over: Vec::new(),
            size: (page.w_pt, page.h_pt),
            number,
            section,
            kind,
        }
    }
}

pub struct LaidOut {
    pub pages: Vec<Page>,
    pub warnings: Vec<Warning>,
}

const PLACEHOLDER_COLOR: [u8; 3] = [0x88, 0x88, 0x88];

pub fn layout(doc: &ir::Document, book: &mut FontBook, calib: &Calib) -> LaidOut {
    let collapse = doc.html_paragraph_spacing && calib.para_spacing == ParaSpacing::HtmlCollapse;
    let hf_on = calib.header_footer == HeaderFooter::Drawn;
    // 页眉页脚先用域的缓存值量出高度，定下各类页面的正文区；真实的页码要等全文
    // 排完才知道，那时再代入重排页眉页脚，正文不再跟着动。
    let heights: Vec<[Hf; 3]> = doc
        .sections
        .iter()
        .map(|s| {
            [PageKind::Default, PageKind::First, PageKind::Even].map(|kind| {
                let mut height = |set: &ir::HeaderSet| {
                    let blocks = pick_story(set, kind).filter(|_| hf_on)?;
                    let boxes = story_boxes(blocks, s, doc, book, calib, collapse, &|_| None);
                    Some(stack(&boxes, collapse).1)
                };
                (height(&s.headers), height(&s.footers))
            })
        })
        .collect();
    let frames: Vec<paginate::Frames> = doc
        .sections
        .iter()
        .zip(&heights)
        .map(|(s, [default, first, even])| paginate::Frames {
            default: frame(s, calib, *default),
            first: frame(s, calib, *first),
            even: frame(s, calib, *even),
            title_page: hf_on && s.title_page,
            even_odd: hf_on && doc.even_and_odd_headers,
        })
        .collect();
    let first = &doc.sections[0];
    let mut pages = paginate::Paginator::new(frames[0], first.page_number_start, collapse, calib);
    let mut warnings = Vec::new();

    // 先把所有块量好，放的时候才能往后看（与下段同页要知道下一段有多高）。
    // 各节按自己的栏宽量。
    let mut measured: Vec<Measured> = Vec::with_capacity(doc.blocks.len());
    for section in &doc.sections {
        let env = para::Env {
            grid: section.grid,
            left: section.page.margin_left,
            width: section.page.content_width(),
            default_tab_stop: doc.default_tab_stop,
            char_pitch: section.char_pitch,
            punct_hangs: true,
            calib,
        };
        measured.extend(
            doc.blocks[section.blocks.clone()]
                .iter()
                .map(|block| measure_block(block, &env, book, collapse, &|_| None)),
        );
    }
    if calib.flow == Flow::Word {
        contextual_spacing(&doc.blocks, &mut measured);
    }
    join_boxes(&mut measured);

    let numbered = doc
        .blocks
        .iter()
        .filter(|b| matches!(b, ir::Block::Para(p) if p.numbering_dropped))
        .count();
    for (si, section) in doc.sections.iter().enumerate() {
        if si > 0 {
            pages.start_section(frames[si], section.start, section.page_number_start, si);
        }
        // 与下段同页不跨节：下一节总是另起一页（或者与本节无关）。
        let in_section = &measured[..section.blocks.end];
        for i in section.blocks.clone() {
            place_block(i, &doc.blocks[i], in_section, &mut pages, &mut warnings);
        }
    }

    // 编号与页眉页脚都按文档级汇总，不逐段报 —— 一个 50 项的列表
    // 报 50 条警告，等于没报。
    if numbered > 0 {
        warnings.push(Warning::new(
            WarningKind::UnsupportedElement,
            format!(
"{numbered} 个段落使用了 Word 的自动编号，本版本不生成编号文字（正文已保留）。需要编号请在 Word 里改成手动输入的序号。"
            ),
        ));
    }
    if !doc.num_format_fallbacks.is_empty() {
        warnings.push(Warning::new(
            WarningKind::UnsupportedElement,
            format!(
                "编号格式 {} 暂不支持，已按阿拉伯数字输出",
                doc.num_format_fallbacks.join("、")
            ),
        ));
    }
    if doc.missing_objects > 0 {
        warnings.push(Warning::new(
            WarningKind::UnsupportedElement,
            format!(
                "{} 个组合、图表或找不到的图片本版本画不出来，已按原大小画成灰框",
                doc.missing_objects
            ),
        ));
    }
    if doc.approximated_shapes > 0 {
        warnings.push(Warning::new(
            WarningKind::UnsupportedElement,
            format!(
                "{} 个形状本版本画不准：圆角矩形画成直角，旋转的按不旋转画，矩形与直线以外的形状（椭圆、箭头……）只画了框里的字",
                doc.approximated_shapes
            ),
        ));
    }
    if doc.notes > 0 {
        warnings.push(Warning::new(
            WarningKind::UnsupportedElement,
            format!(
                "{} 处脚注、尾注本版本不排：正文里的注释编号与注释的内容都没有画",
                doc.notes
            ),
        ));
    }
    if doc.multi_column_sections > 0 {
        warnings.push(Warning::new(
            WarningKind::UnsupportedElement,
            format!("{} 节设置了分栏，本版本按单栏排", doc.multi_column_sections),
        ));
    }
    if doc.approximated_wraps > 0 {
        warnings.push(Warning::new(
            WarningKind::UnsupportedElement,
            format!(
                "{} 张四周型、紧密型或穿越型环绕的图本版本按上下型排：图所在的那一段横条上不排字，文字不绕着图走",
                doc.approximated_wraps
            ),
        ));
    }
    if doc.has_header_footer && !hf_on {
        warnings.push(Warning::new(
            WarningKind::UnsupportedElement,
            "文档设置了页眉或页脚，本版本不渲染".to_string(),
        ));
    }

    let mut pages = pages.finish();
    if hf_on {
        draw_headers_footers(
            doc,
            &mut pages,
            &heights,
            book,
            calib,
            collapse,
            &mut warnings,
        );
    }

    warnings.extend(book.take_warnings());
    LaidOut { pages, warnings }
}

/// 一类页面上页眉、页脚的高度，没有就是 None。
type Hf = (Option<f32>, Option<f32>);

/// 一类页面用的页眉（或页脚）。首页、偶数页没有单独定义时就是没有，不退回默认的。
fn pick_story(set: &ir::HeaderSet, kind: PageKind) -> Option<&[ir::Block]> {
    match kind {
        PageKind::Default => set.default.as_deref(),
        PageKind::First => set.first.as_deref(),
        PageKind::Even => set.even.as_deref(),
    }
}

/// 量页眉页脚。它们不吸附行网格（LibreOffice 实测）。`value` 给出域的值，
/// None 时用缓存的结果。
fn story_boxes(
    blocks: &[ir::Block],
    section: &ir::Section,
    doc: &ir::Document,
    book: &mut FontBook,
    calib: &Calib,
    collapse: bool,
    value: &dyn Fn(&ir::Field) -> Option<String>,
) -> Vec<Measured> {
    let env = para::Env {
        grid: None,
        left: section.page.margin_left,
        width: section.page.content_width(),
        default_tab_stop: doc.default_tab_stop,
        char_pitch: None,
        punct_hangs: true,
        calib,
    };
    measure_blocks(blocks, &env, book, collapse, value)
}

/// 量一个块。表格的各格递归地量好、叠起来。`value` 给出域的值，None 时用缓存的结果。
fn measure_block(
    block: &ir::Block,
    env: &para::Env,
    book: &mut FontBook,
    collapse: bool,
    value: &dyn Fn(&ir::Field) -> Option<String>,
) -> Measured {
    // 形状里的字与单元格里的一样量。
    let mut story = |blocks: &[ir::Block], env: &para::Env, book: &mut FontBook, caps: &[f32]| {
        let measured = measure_blocks(blocks, env, book, collapse, value);
        paginate::flow(&measured, caps, false, collapse)
    };
    match block {
        ir::Block::Para(p) => {
            Measured::Para(para::measure(&with_fields(p, value), env, book, &mut story))
        }
        ir::Block::Placeholder(ph) => Measured::Placeholder(
            placeholder_paras(ph)
                .iter()
                .map(|p| para::measure(p, env, book, &mut story))
                .collect(),
        ),
        ir::Block::Table(t) => {
            Measured::Table(table::measure(t, env, collapse, &mut |blocks, env| {
                measure_blocks(blocks, env, book, collapse, value)
            }))
        }
    }
}

/// 量一串块（单元格、页眉页脚），同正文一样处理段距与合框。它们里面不分页，
/// 见 [`para::ParaBox::flatten`]。
fn measure_blocks(
    blocks: &[ir::Block],
    env: &para::Env,
    book: &mut FontBook,
    collapse: bool,
    value: &dyn Fn(&ir::Field) -> Option<String>,
) -> Vec<Measured> {
    let mut measured: Vec<Measured> = blocks
        .iter()
        .map(|block| measure_block(block, env, book, collapse, value))
        .collect();
    if env.calib.flow == Flow::Word {
        contextual_spacing(blocks, &mut measured);
    }
    join_boxes(&mut measured);
    for m in &mut measured {
        match m {
            Measured::Para(p) => p.flatten(),
            Measured::Placeholder(paras) => paras.iter_mut().for_each(para::ParaBox::flatten),
            Measured::Table(_) => {}
        }
    }
    measured
}

/// 把量好的块从上往下叠起来，不分页：返回绘制操作（y 以顶端为 0）与总高度。
fn stack(measured: &[Measured], collapse: bool) -> (Vec<PaintOp>, f32) {
    let page = paginate::flow(measured, &[paginate::ENDLESS], false, collapse).swap_remove(0);
    (page.ops, page.height)
}

/// 代入域的值：同一个域的结果只留一份，写成 `value` 给的文字。
fn with_fields<'a>(
    p: &'a ir::Paragraph,
    value: &dyn Fn(&ir::Field) -> Option<String>,
) -> std::borrow::Cow<'a, ir::Paragraph> {
    use std::borrow::Cow;
    if !p.spans.iter().any(|s| s.style.field.is_some()) {
        return Cow::Borrowed(p);
    }
    let mut text = String::new();
    let mut spans = Vec::with_capacity(p.spans.len());
    let mut done = std::collections::HashSet::new();
    for span in &p.spans {
        let piece = match span.style.field.map(|f| (f, value(&f))) {
            Some((f, Some(v))) if done.insert(f.id) => v,
            Some((_, Some(_))) => continue,
            _ => p.text[span.range.clone()].to_string(),
        };
        if piece.is_empty() {
            continue;
        }
        let start = text.len();
        text.push_str(&piece);
        spans.push(ir::Span {
            range: start..text.len(),
            style: span.style.clone(),
        });
    }
    Cow::Owned(ir::Paragraph {
        text,
        spans,
        ..p.clone()
    })
}

/// 页码类域的值。PAGE 按本节的页码格式写，另外两个按阿拉伯数字写；域代码里的
/// `\*` 开关优先。
fn field_text(f: &ir::Field, number: i32, total: i32, section_pages: i32, format: &str) -> String {
    let (n, default) = match f.kind {
        ir::FieldKind::Page => (number, format),
        ir::FieldKind::NumPages => (total, "decimal"),
        ir::FieldKind::SectionPages => (section_pages, "decimal"),
    };
    match f.format.unwrap_or(default) {
        "arabicDash" => format!("- {n} -"),
        fmt => crate::docx::numfmt::format(n, fmt).unwrap_or_else(|| n.to_string()),
    }
}

/// 逐页画页眉页脚：代入这一页的页码、总页数。页眉顶端在离纸张上边 `w:header` 处，
/// 页脚底端在离纸张下边 `w:footer` 处。
fn draw_headers_footers(
    doc: &ir::Document,
    pages: &mut [Page],
    heights: &[[Hf; 3]],
    book: &mut FontBook,
    calib: &Calib,
    collapse: bool,
    warnings: &mut Vec<Warning>,
) {
    let total = pages.len() as i32;
    let mut per_section = vec![0i32; doc.sections.len()];
    for p in pages.iter() {
        per_section[p.section] += 1;
    }
    let mut grew = false;
    for page in pages.iter_mut() {
        let (number, si, kind) = (page.number, page.section, page.kind);
        let s = &doc.sections[si];
        let value = |f: &ir::Field| {
            Some(field_text(
                f,
                number,
                total,
                per_section[si],
                &s.page_number_format,
            ))
        };
        let frozen = heights[si][match kind {
            PageKind::Default => 0,
            PageKind::First => 1,
            PageKind::Even => 2,
        }];
        for (set, is_header, frozen) in
            [(&s.headers, true, frozen.0), (&s.footers, false, frozen.1)]
        {
            let Some(blocks) = pick_story(set, kind) else {
                continue;
            };
            let boxes = story_boxes(blocks, s, doc, book, calib, collapse, &value);
            let (ops, height) = stack(&boxes, collapse);
            grew |= height > frozen.unwrap_or(0.0) + 1.0;
            let dy = if is_header {
                s.page.h_pt - s.page.header_dist
            } else {
                s.page.footer_dist + height
            };
            page.ops.extend(ops.iter().map(|op| op.shifted(dy)));
        }
    }
    if grew {
        warnings.push(Warning::new(
            WarningKind::UnsupportedElement,
            "页眉页脚代入真实页码后变高了，正文没有跟着下移，可能与页眉页脚重叠".to_string(),
        ));
    }
}

/// 把第 `i` 块放进页面。`measured` 只到本节末尾：与下段同页不跨节。
fn place_block(
    i: usize,
    block: &ir::Block,
    measured: &[Measured],
    pages: &mut paginate::Paginator,
    warnings: &mut Vec<Warning>,
) {
    match (block, &measured[i]) {
        (_, Measured::Para(b)) => {
            if b.keep_next {
                // 这一串与下段同页的段落，以及紧跟在后面的那一段。
                let chain: Vec<&para::ParaBox> = measured[i..]
                    .iter()
                    .map_while(|m| match m {
                        Measured::Para(p) if p.keep_next => Some(p),
                        _ => None,
                    })
                    .collect();
                pages.keep_together(&chain, measured.get(i + chain.len()));
            }
            pages.place_para(b);
        }
        // 不支持的内容：一句说明 + 能抽出来的文字，并记下在第几页。
        //
        // 不静默丢弃，是因为用户会以为转全了；不整份拒绝，是因为其余内容通常完全可用。
        // 对法律文书来说，悄悄丢一张表格是**危险**的。
        (ir::Block::Placeholder(ph), Measured::Placeholder(paras)) => {
            warnings.push(
                Warning::new(
                    WarningKind::UnsupportedElement,
                    format!("{} 未能渲染", ph.kind.label()),
                )
                .at_page(pages.page_index() + 1),
            );
            for b in paras {
                pages.place_para(b);
            }
        }
        (ir::Block::Table(t), Measured::Table(tb)) => {
            let mut inside = Vec::new();
            table_placeholders(t, &mut inside);
            for ph in inside {
                warnings.push(
                    Warning::new(
                        WarningKind::UnsupportedElement,
                        format!("{} 未能渲染", ph.kind.label()),
                    )
                    .at_page(pages.page_index() + 1),
                );
            }
            pages.place_table(tb);
        }
        _ => unreachable!("量出来的与原块一一对应"),
    }
}

/// 表格里画不出来的内容（嵌套的表格、图片），按出现的顺序。
fn table_placeholders<'a>(t: &'a ir::Table, out: &mut Vec<&'a ir::Placeholder>) {
    for block in t.rows.iter().flat_map(|r| &r.cells).flat_map(|c| &c.blocks) {
        match block {
            ir::Block::Placeholder(ph) => out.push(ph),
            ir::Block::Table(inner) => table_placeholders(inner, out),
            ir::Block::Para(_) => {}
        }
    }
}

/// 量好的块，与 `ir::Document::blocks` 一一对应。
enum Measured {
    Para(para::ParaBox),
    Placeholder(Vec<para::ParaBox>),
    Table(table::TableBox),
}

/// 同一样式的相邻段落之间不加段距：`contextualSpacing` 写在谁身上，就去掉谁靠近
/// 同样式邻居的那一侧（段前或段后）。占位块把相邻关系隔开。
fn contextual_spacing(blocks: &[ir::Block], measured: &mut [Measured]) {
    let style = |i: usize| match blocks.get(i) {
        Some(ir::Block::Para(p)) => Some(p),
        _ => None,
    };
    for (i, m) in measured.iter_mut().enumerate() {
        let (Some(p), Measured::Para(b)) = (style(i), m) else {
            continue;
        };
        if !p.contextual_spacing {
            continue;
        }
        let same = |j: Option<usize>| {
            j.and_then(style)
                .is_some_and(|q| q.style_id.is_some() && q.style_id == p.style_id)
        };
        if same(i.checked_sub(1)) {
            b.space_before = 0.0;
        }
        if same(Some(i + 1)) {
            b.space_after = 0.0;
        }
    }
}

/// 相邻段落的边框、底纹、缩进都相同时合成一个框。占位块把相邻关系隔开。
fn join_boxes(measured: &mut [Measured]) {
    for i in 1..measured.len() {
        let (head, tail) = measured.split_at_mut(i);
        if let (Measured::Para(a), Measured::Para(b)) = (&mut head[i - 1], &tail[0]) {
            a.joins_next = a.decor.is_some() && a.decor == b.decor;
        }
    }
}

/// 一类页面的版面。`hf` 是这类页面上页眉、页脚的高度（没有就是 None）：页眉的
/// 底边低过上边距时正文从页眉底下开始，页脚的顶边高过下边距时正文排到页脚顶为止。
fn frame(section: &ir::Section, calib: &Calib, (header, footer): Hf) -> paginate::Frame {
    let p = &section.page;
    let top = header.map_or(p.margin_top, |h| p.margin_top.max(p.header_dist + h));
    let bottom = footer.map_or(p.margin_bottom, |h| p.margin_bottom.max(p.footer_dist + h));
    let height = (p.h_pt - top - bottom).max(1.0);
    let (origin, capacity) = grid_area(height, section.grid, calib);
    paginate::Frame {
        page: *p,
        origin: top - p.margin_top + origin,
        capacity,
    }
}

/// 正文能用的竖向区间：(离正文区顶端的偏移, 高度)。见 [`GridLayout::Centered`]。
fn grid_area(height: f32, grid: Option<ir::Grid>, calib: &Calib) -> (f32, f32) {
    match (grid, calib.grid) {
        (Some(g), GridLayout::Centered) if g.pitch_pt > 0.0 && g.pitch_pt <= height => {
            let area = (height / g.pitch_pt).floor() * g.pitch_pt;
            ((height - area) / 2.0, area)
        }
        _ => (0.0, height),
    }
}

fn placeholder_paras(ph: &ir::Placeholder) -> Vec<ir::Paragraph> {
    let note = match &ph.kind {
        ir::PlaceholderKind::Drawing { alt: Some(a) } => {
            format!("［图片：{a}　本版本不渲染图片］")
        }
        k => format!("［{}　本版本不渲染，以下为其文字内容］", k.label()),
    };
    std::iter::once(placeholder_para(note, true))
        .chain(ph.text.iter().map(|t| placeholder_para(t.clone(), false)))
        .collect()
}

fn placeholder_para(text: String, is_note: bool) -> ir::Paragraph {
    let style = ir::RunStyle {
        size_pt: 9.0,
        bold: false,
        italic: false,
        underline: None,
        strike: false,
        double_strike: false,
        background: None,
        color: PLACEHOLDER_COLOR,
        font_latin: None,
        font_east_asia: None,
        hint_east_asia: false,
        char_spacing: 0.0,
        vert_align: ir::VertAlign::Baseline,
        position_pt: 0.0,
        link: None,
        field: None,
        kern: false,
    };
    let spans = vec![ir::Span {
        range: 0..text.len(),
        style: style.clone(),
    }];
    ir::Paragraph {
        align: ir::Align::Left,
        indent_left: if is_note { 0.0 } else { 21.0 },
        indent_right: 0.0,
        first_line: 0.0,
        space_before: if is_note { 6.0 } else { 0.0 },
        space_after: if is_note { 2.0 } else { 0.0 },
        line: ir::LineSpacing::Multiple(1.0),
        page_break_before: false,
        // 占位说明不参与网格吸附：它是我们插入的提示，不属于原文排版。
        snap_to_grid: false,
        auto_space: true,
        overflow_punct: true,
        tabs: Vec::new(),
        keep_next: false,
        keep_lines: false,
        widow_control: false,
        contextual_spacing: false,
        borders: ir::Borders::default(),
        shading: None,
        style_id: None,
        numbering_dropped: false,
        number: None,
        text,
        spans,
        objects: Vec::new(),
        floats: Vec::new(),
        mark: style,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 西文字体的行间距算在字的上面：基线离行顶「上伸 + 行间距」。
    /// 重写前的规则把它算在下面。
    #[test]
    fn line_gap_sits_above_the_text() {
        let mut book = FontBook::new();
        let p = placeholder_para("Abc".into(), false);
        for (calib, above) in [(Calib::current(), true), (Calib::legacy(), false)] {
            let env = para::Env {
                grid: None,
                left: 0.0,
                width: 400.0,
                default_tab_stop: 36.0,
                char_pitch: None,
                punct_hangs: true,
                calib: &calib,
            };
            let b = para::measure(&p, &env, &mut book, &mut |_, _, _, _| Vec::new());
            let para::ParaBody::Lines(lines) = &b.body else {
                panic!("应当有一行字");
            };
            let font = book.resolve(None, false, false, false).unwrap();
            let m = book.face(font.id).metrics();
            let gap = if above { m.line_gap as f32 } else { 0.0 };
            let want = (m.ascender as f32 + gap) * 9.0 / m.upem as f32;
            assert!(
                (lines[0].baseline - want).abs() < 1e-3,
                "{} vs {want}",
                lines[0].baseline
            );
        }
    }
}
