//! 中间表示：样式已展开、单位已统一成**点**、字体已解析成请求的家族名。
//!
//! 从这里往后不再出现 twips、半磅、1/100 字符这些 OOXML 单位，
//! 排版代码只跟点打交道。

use super::model::{Align, LineRule, PPr, RPr, RawBlock, RawDocument, UnsupportedKind};
use super::style::Resolver;

/// twips → 点。1 点 = 20 twips。
fn tw(v: i32) -> f32 {
    v as f32 / 20.0
}

/// 半磅 → 点。
fn half_pt(v: u32) -> f32 {
    v as f32 / 2.0
}

/// 没有任何字号信息时的兜底。Word 的默认正文是五号（10.5 磅），
/// 但 OOXML 的 docDefaults 缺省是 10 磅。
const FALLBACK_SIZE_PT: f32 = 10.5;

/// 行网格。`pitch_pt` 是网格行距（点）。
#[derive(Debug, Clone, Copy)]
pub struct Grid {
    pub pitch_pt: f32,
}

impl Grid {
    /// 把单倍行高向上吸附到网格整数倍。
    ///
    /// 这是中文排版行密度的决定性一步：12pt 宋体自然行高 17.4pt，
    /// 在 15.6pt 的网格上要占满 2 格 = 31.2pt。不做这步，整篇会挤掉近一半。
    pub fn snap(self, natural_pt: f32) -> f32 {
        if self.pitch_pt <= 0.0 {
            return natural_pt;
        }
        (natural_pt / self.pitch_pt).ceil().max(1.0) * self.pitch_pt
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PageGeom {
    pub w_pt: f32,
    pub h_pt: f32,
    pub margin_top: f32,
    pub margin_bottom: f32,
    pub margin_left: f32,
    pub margin_right: f32,
}

impl PageGeom {
    pub fn content_width(&self) -> f32 {
        (self.w_pt - self.margin_left - self.margin_right).max(1.0)
    }
    pub fn content_height(&self) -> f32 {
        (self.h_pt - self.margin_top - self.margin_bottom).max(1.0)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum LineSpacing {
    /// 行距倍数。`lineRule="auto"` 时 `w:line` 是 240 分之一行，312 → 1.3 倍。
    Multiple(f32),
    Exact(f32),
    AtLeast(f32),
}

#[derive(Debug, Clone)]
pub struct Run {
    pub text: String,
    pub size_pt: f32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub color: [u8; 3],
    /// 西文字体家族名（来自 `w:rFonts/@w:ascii`）。
    pub font_latin: Option<String>,
    /// 中日韩字体家族名（来自 `w:rFonts/@w:eastAsia`）。
    pub font_east_asia: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Paragraph {
    pub align: Align,
    pub indent_left: f32,
    pub indent_right: f32,
    /// 首行缩进。负值表示悬挂缩进。
    pub first_line: f32,
    pub space_before: f32,
    pub space_after: f32,
    pub line: LineSpacing,
    pub page_break_before: bool,
    /// 本段是否参与行网格吸附（`w:snapToGrid`，缺省 true）。
    pub snap_to_grid: bool,
    /// 中日韩文字与西文/数字之间是否自动加间距（`w:autoSpaceDE`/`DN`，缺省 true）。
    pub auto_space: bool,
    /// 本段挂了自动编号，但编号文字没有生成。
    pub numbering_dropped: bool,
    pub runs: Vec<Run>,
}

#[derive(Debug, Clone)]
pub struct UnsupportedBlock {
    pub kind: UnsupportedKind,
    /// 能抽出来的文字。表格的单元格内容会走这里，以纯文本形式保留。
    pub text: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum Block {
    Para(Paragraph),
    Unsupported(UnsupportedBlock),
}

#[derive(Debug, Clone)]
pub struct Document {
    pub page: PageGeom,
    /// 文档引用了页眉或页脚，但本版本不渲染。
    pub has_header_footer: bool,
    /// 文档的行网格。None 表示没有网格或网格类型不吸附。
    pub grid: Option<Grid>,
    pub blocks: Vec<Block>,
}

pub fn build(raw: &RawDocument) -> Document {
    let resolver = Resolver::new(&raw.styles);
    let s = raw.section;

    let page = PageGeom {
        w_pt: tw(s.page_w),
        h_pt: tw(s.page_h),
        margin_top: tw(s.margin_top),
        margin_bottom: tw(s.margin_bottom),
        margin_left: tw(s.margin_left),
        margin_right: tw(s.margin_right),
    };

    let mut blocks = Vec::with_capacity(raw.blocks.len());
    for raw_block in &raw.blocks {
        match raw_block {
            RawBlock::Para(p) => {
                let ppr = resolver.paragraph(&p.ppr);
                let runs: Vec<Run> = p
                    .runs
                    .iter()
                    .map(|r| build_run(&resolver.run(&ppr, &r.rpr), &r.text))
                    .collect();

                // 首行缩进按「字符」算时，用的是段落标记的东亚字号。
                let mark = resolver.run(&ppr, &RPr::default());
                let char_size = mark
                    .size_half_pt
                    .map(half_pt)
                    .or_else(|| runs.first().map(|r| r.size_pt))
                    .unwrap_or(FALLBACK_SIZE_PT);

                blocks.push(Block::Para(build_paragraph(&ppr, runs, char_size)));

                for kind in &p.unsupported {
                    blocks.push(Block::Unsupported(UnsupportedBlock {
                        kind: kind.clone(),
                        text: Vec::new(),
                    }));
                }
            }
            RawBlock::Unsupported { kind, text } => {
                blocks.push(Block::Unsupported(UnsupportedBlock {
                    kind: kind.clone(),
                    text: text.clone(),
                }));
            }
        }
    }

    let grid = s
        .doc_grid
        .filter(|g| g.snaps && g.line_pitch > 0)
        .map(|g| Grid {
            pitch_pt: tw(g.line_pitch),
        });

    Document {
        page,
        has_header_footer: s.has_header_footer,
        grid,
        blocks,
    }
}

/// Word 的默认制表位是 0.74cm。本版本**不实现真正的制表位**，
/// 而是把每个 `w:tab` 当作一个全角空格（1 em）的固定推进。
/// 这是个近似，对含大量制表符对齐的文档会偏，README 里已说明。
fn expand_tabs(text: &str) -> String {
    if !text.contains('\t') {
        return text.to_string();
    }
    text.replace('\t', "\u{3000}")
}

fn build_run(rpr: &RPr, text: &str) -> Run {
    Run {
        text: expand_tabs(text),
        size_pt: rpr.size_half_pt.map(half_pt).unwrap_or(FALLBACK_SIZE_PT),
        bold: rpr.bold.unwrap_or(false),
        italic: rpr.italic.unwrap_or(false),
        underline: rpr.underline.unwrap_or(false),
        strike: rpr.strike.unwrap_or(false),
        color: rpr.color.unwrap_or([0, 0, 0]),
        font_latin: rpr.font_ascii.clone(),
        font_east_asia: rpr.font_east_asia.clone(),
    }
}

fn build_paragraph(ppr: &PPr, runs: Vec<Run>, char_size_pt: f32) -> Paragraph {
    let ind = &ppr.indent;

    // `*Chars` 版本优先于 twips 版本 —— Word 就是这么做的，而中文文档里
    // 「首行缩进 2 字符」几乎无处不在（写成 firstLineChars="200"）。
    let left = ind
        .left_chars
        .map(|c| c as f32 / 100.0 * char_size_pt)
        .or_else(|| ind.left_twips.map(tw))
        .unwrap_or(0.0);

    let hanging = ind
        .hanging_chars
        .map(|c| c as f32 / 100.0 * char_size_pt)
        .or_else(|| ind.hanging_twips.map(tw));

    let first_line = match hanging {
        // 悬挂缩进与首行缩进互斥，悬挂优先且方向相反。
        Some(h) => -h,
        None => ind
            .first_line_chars
            .map(|c| c as f32 / 100.0 * char_size_pt)
            .or_else(|| ind.first_line_twips.map(tw))
            .unwrap_or(0.0),
    };

    let line = match (ppr.line, ppr.line_rule) {
        (Some(v), Some(LineRule::Exact)) => LineSpacing::Exact(tw(v)),
        (Some(v), Some(LineRule::AtLeast)) => LineSpacing::AtLeast(tw(v)),
        // auto：w:line 的单位是 240 分之一行。312/240 = 1.3 倍行距。
        // 当成 twips 处理的话就变成 15.6 磅的固定行高，每份文档的页数都会错。
        (Some(v), _) => LineSpacing::Multiple((v as f32 / 240.0).max(0.1)),
        _ => LineSpacing::Multiple(1.0),
    };

    Paragraph {
        align: ppr.align.unwrap_or(Align::Left),
        indent_left: left,
        indent_right: ind.right_twips.map(tw).unwrap_or(0.0),
        first_line,
        space_before: ppr.space_before_twips.map(tw).unwrap_or(0.0),
        space_after: ppr.space_after_twips.map(tw).unwrap_or(0.0),
        line,
        page_break_before: ppr.page_break_before.unwrap_or(false),
        snap_to_grid: ppr.snap_to_grid.unwrap_or(true),
        // 两个开关只要有一个开着就加间距：它们分别管西文和数字，
        // 而我们不在字符级区分这两类，统一按「非中日韩」处理。
        auto_space: ppr.auto_space_latin.unwrap_or(true) || ppr.auto_space_digits.unwrap_or(true),
        numbering_dropped: ppr.numbering,
        runs,
    }
}
