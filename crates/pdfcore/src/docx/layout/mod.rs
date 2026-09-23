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
mod text;

pub use calib::{
    Calib, EmptyPara, FixedBaseline, GridLayout, PageBottom, ParaSpacing, TrailingSpaces,
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
    /// 下划线、删除线、占位框的边都用矩形画。
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
            PaintOp::Text { y, .. } | PaintOp::Rect { y, .. } => *y += dy,
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
        calib,
    };
    let collapse = doc.html_paragraph_spacing && calib.para_spacing == ParaSpacing::HtmlCollapse;
    let mut pages = paginate::Paginator::new(&doc.page, grid_area(doc, calib), collapse);
    let mut warnings = Vec::new();

    let mut numbered = 0usize;
    for block in &doc.blocks {
        match block {
            ir::Block::Para(p) => {
                if p.numbering_dropped {
                    numbered += 1;
                }
                pages.place_para(&para::measure(p, &env, book));
            }
            // 不支持的内容：一句说明 + 能抽出来的文字，并记下在第几页。
            //
            // 不静默丢弃，是因为用户会以为转全了；不整份拒绝，是因为其余内容通常完全可用。
            // 对法律文书来说，悄悄丢一张表格是**危险**的。
            ir::Block::Placeholder(ph) => {
                warnings.push(
                    Warning::new(
                        WarningKind::UnsupportedElement,
                        format!("{} 未能渲染", ph.kind.label()),
                    )
                    .at_page(pages.page_index() + 1),
                );
                for p in placeholder_paras(ph) {
                    pages.place_para(&para::measure(&p, &env, book));
                }
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
        underline: false,
        strike: false,
        color: PLACEHOLDER_COLOR,
        font_latin: None,
        font_east_asia: None,
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
        numbering_dropped: false,
        text,
        spans,
        mark: style,
    }
}
