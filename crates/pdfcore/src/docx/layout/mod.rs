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
mod text;

pub use calib::{
    AutoSpace, Breaks, Calib, Cascade, CharClass, EmptyPara, FixedBaseline, Flow, GridLayout,
    HangingIndent, HangingPunct, Justify, Overflow, PageBottom, PageBreakBefore, ParaSpacing,
    RunFormat, Tabs, Theme, TrailingSpaces,
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
    /// 虚线、点线样式的下划线：一条水平线。`dash` 是 PDF 的虚线样式（线段、间隔交替）。
    Line {
        x1: f32,
        x2: f32,
        y: f32,
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
    /// 下划线、删除线、底色、占位框的边都用矩形画。
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: [u8; 3],
    },
}

impl PaintOp {
    /// 纵向平移。测量时 y 相对于行的基线，放进页面时才加上基线的绝对位置。
    fn shifted(&self, dy: f32) -> PaintOp {
        let mut op = self.clone();
        match &mut op {
            PaintOp::Text { y, .. } | PaintOp::Rect { y, .. } | PaintOp::Line { y, .. } => *y += dy,
            PaintOp::Link { y1, y2, .. } => {
                *y1 += dy;
                *y2 += dy;
            }
        }
        op
    }
}

#[derive(Debug, Clone, Default)]
pub struct Page {
    pub ops: Vec<PaintOp>,
}

pub struct LaidOut {
    pub pages: Vec<Page>,
    pub warnings: Vec<Warning>,
}

const PLACEHOLDER_COLOR: [u8; 3] = [0x88, 0x88, 0x88];

pub fn layout(doc: &ir::Document, book: &mut FontBook, calib: &Calib) -> LaidOut {
    let env = para::Env {
        grid: doc.grid,
        left: doc.page.margin_left,
        width: doc.page.content_width(),
        default_tab_stop: doc.default_tab_stop,
        calib,
    };
    let collapse = doc.html_paragraph_spacing && calib.para_spacing == ParaSpacing::HtmlCollapse;
    let mut pages = paginate::Paginator::new(&doc.page, grid_area(doc, calib), collapse, calib);
    let mut warnings = Vec::new();

    // 先把所有块量好，放的时候才能往后看（与下段同页要知道下一段有多高）。
    let mut measured: Vec<Measured> = doc
        .blocks
        .iter()
        .map(|block| match block {
            ir::Block::Para(p) => Measured::Para(para::measure(p, &env, book)),
            ir::Block::Placeholder(ph) => Measured::Placeholder(
                placeholder_paras(ph)
                    .iter()
                    .map(|p| para::measure(p, &env, book))
                    .collect(),
            ),
        })
        .collect();
    if calib.flow == Flow::Word {
        contextual_spacing(&doc.blocks, &mut measured);
    }

    let numbered = doc
        .blocks
        .iter()
        .filter(|b| matches!(b, ir::Block::Para(p) if p.numbering_dropped))
        .count();
    for (i, (block, m)) in doc.blocks.iter().zip(&measured).enumerate() {
        match (block, m) {
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
                    let next = match measured.get(i + chain.len()) {
                        Some(Measured::Para(p)) => Some(p),
                        _ => None,
                    };
                    pages.keep_together(&chain, next);
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
            (ir::Block::Para(_), Measured::Placeholder(_)) => {
                unreachable!("量出来的与原块一一对应")
            }
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
    if doc.has_header_footer {
        warnings.push(Warning::new(
            WarningKind::UnsupportedElement,
            "文档设置了页眉或页脚，本版本不渲染".to_string(),
        ));
    }

    warnings.extend(book.take_warnings());
    LaidOut {
        pages: pages.finish(),
        warnings,
    }
}

/// 量好的块，与 `ir::Document::blocks` 一一对应。
enum Measured {
    Para(para::ParaBox),
    Placeholder(Vec<para::ParaBox>),
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

/// 正文能用的竖向区间：(离版心顶端的偏移, 高度)。见 [`GridLayout::Centered`]。
fn grid_area(doc: &ir::Document, calib: &Calib) -> (f32, f32) {
    let height = doc.page.content_height();
    match (doc.grid, calib.grid) {
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
        style_id: None,
        numbering_dropped: false,
        text,
        spans,
        mark: style,
    }
}
