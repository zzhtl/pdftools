//! 中间表示：样式已展开、单位已统一成**点**、字体仍是文档里写的名字。
//!
//! 从这里往后不再出现 twips、半磅、1/100 字符这些 OOXML 单位，排版代码只跟点打交道；
//! 字体要等排版时才解析成本机真实的字体，所以这一层不依赖字体也能测。
//!
//! 一个段落的文字是**一个字符串加若干 span**：断行（unicode-linebreak）与整形
//! （rustybuzz）都作用在同一个字符串上，字节偏移是唯一的坐标系。
//! 不是文字的东西也编进这个字符串：
//!
//! | 来源 | 字符 |
//! | --- | --- |
//! | `w:tab` | `\t` |
//! | `w:br`、`w:cr` | U+2028 |
//! | `w:br w:type="page"` | U+000C |
//! | `w:br w:type="column"` | U+000B |
//! | `w:noBreakHyphen` | U+2011 |

use std::ops::Range;

use super::layout::{Calib, RunFormat};
use super::model::{self, BreakKind, LineRule, PPr, RPr, RunItem};
pub use super::model::{TabAlign, TabLeader, UnderlineStyle};
use super::resolve::Resolver;

pub const LINE_BREAK: char = '\u{2028}';
pub const PAGE_BREAK: char = '\u{000C}';
pub const COLUMN_BREAK: char = '\u{000B}';

/// twips → 点。1 点 = 20 twips。
fn tw(v: i32) -> f32 {
    v as f32 / 20.0
}

/// 半磅 → 点。
fn half_pt(v: u32) -> f32 {
    v as f32 / 2.0
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Debug, Clone, Copy)]
pub enum LineSpacing {
    /// 行距倍数。`lineRule="auto"` 时 `w:line` 是 240 分之一行，312 → 1.3 倍。
    Multiple(f32),
    Exact(f32),
    AtLeast(f32),
}

/// 一段文字的格式，层叠已算完。
#[derive(Debug, Clone, PartialEq)]
pub struct RunStyle {
    pub size_pt: f32,
    pub bold: bool,
    pub italic: bool,
    pub underline: Option<Underline>,
    pub strike: bool,
    pub double_strike: bool,
    /// 文字背后的底色：突出显示，没有的话是底纹。
    pub background: Option<[u8; 3]>,
    pub color: [u8; 3],
    /// 西文字体家族名（来自 `w:rFonts/@w:ascii`）。
    pub font_latin: Option<String>,
    /// 中日韩字体家族名（来自 `w:rFonts/@w:eastAsia`）。
    pub font_east_asia: Option<String>,
    /// 每个字后面额外加的间距（点），负数是紧缩。
    pub char_spacing: f32,
}

/// 下划线，颜色已经落实（没写时就是文字颜色）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Underline {
    pub style: UnderlineStyle,
    pub color: [u8; 3],
}

/// 段落文字里的一段同格式区间。span 首尾相接、不重叠、都不为空。
#[derive(Debug, Clone)]
pub struct Span {
    pub range: Range<usize>,
    pub style: RunStyle,
}

/// 一个制表位。`pos` 是从正文区左缘量起的点数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TabStop {
    pub pos: f32,
    pub align: TabAlign,
    pub leader: TabLeader,
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
    /// 行尾标点可以伸出右边距（`w:overflowPunct`，缺省 true）。
    pub overflow_punct: bool,
    /// 自定义制表位，按位置升序。竖线位不在其中。
    pub tabs: Vec<TabStop>,
    /// 本段挂了自动编号，但编号文字没有生成。
    pub numbering_dropped: bool,
    pub text: String,
    pub spans: Vec<Span>,
    /// 段落标记（¶）的格式。空段落的行高由它决定。
    pub mark: RunStyle,
}

/// 本版本画不出来的内容。它是 IR 的一等公民，而不是一个被丢掉的分支 ——
/// 这样「诚实失败」就不是靠自觉，而是类型系统逼着排版层去处理。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaceholderKind {
    Table { rows: usize, cols: usize },
    Drawing { alt: Option<String> },
}

impl PlaceholderKind {
    pub fn label(&self) -> String {
        match self {
            Self::Table { rows, cols } => format!("表格（{rows} 行 × {cols} 列）"),
            Self::Drawing { .. } => "图片".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Placeholder {
    pub kind: PlaceholderKind,
    /// 能抽出来的文字。表格里的文字往往是文档里最重要的内容，
    /// 就算画不出表格也要把字留下。
    pub text: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum Block {
    Para(Paragraph),
    Placeholder(Placeholder),
}

#[derive(Debug, Clone)]
pub struct Document {
    pub page: PageGeom,
    /// 文档引用了页眉或页脚，但本版本不渲染。
    pub has_header_footer: bool,
    /// 文档的行网格。None 表示没有网格或网格类型不吸附。
    pub grid: Option<Grid>,
    /// 相邻两段的段后距与段前距取较大值而不是相加（HTML 的规矩）。
    /// 文档没有设置 `w:doNotUseHTMLParagraphAutoSpacing` 时为真，见 `Calib::para_spacing`。
    pub html_paragraph_spacing: bool,
    /// 默认制表位的间距（点）。`settings.xml` 没写时是 Word 的缺省 36pt。
    pub default_tab_stop: f32,
    pub blocks: Vec<Block>,
}

pub fn build(doc: &model::Document, calib: &Calib) -> Document {
    let resolver = Resolver::new(&doc.styles);
    let s = doc.section;

    let mut blocks = Vec::with_capacity(doc.body.len());
    for block in &doc.body {
        match block {
            model::Block::Para(p) => push_paragraph(&mut blocks, p, &resolver, calib),
            model::Block::Table(t) => blocks.push(Block::Placeholder(table_placeholder(t))),
        }
    }

    let grid = s
        .doc_grid
        .filter(|g| g.snaps && g.line_pitch > 0)
        .map(|g| Grid {
            pitch_pt: tw(g.line_pitch),
        });

    Document {
        page: PageGeom {
            w_pt: tw(s.page_w),
            h_pt: tw(s.page_h),
            margin_top: tw(s.margin_top),
            margin_bottom: tw(s.margin_bottom),
            margin_left: tw(s.margin_left),
            margin_right: tw(s.margin_right),
        },
        has_header_footer: s.has_header_footer,
        grid,
        html_paragraph_spacing: !doc.settings.no_html_paragraph_spacing,
        default_tab_stop: doc.settings.default_tab_stop.map(tw).unwrap_or(36.0),
        blocks,
    }
}

fn push_paragraph(out: &mut Vec<Block>, p: &model::Para, resolver: &Resolver, calib: &Calib) {
    let ppr = resolver.paragraph(&p.ppr);
    let mut text = String::new();
    let mut spans = Vec::with_capacity(p.runs.len());
    let mut drawings = Vec::new();
    for run in &p.runs {
        let rpr = resolver.run(&ppr, &run.rpr);
        let full = calib.run_format == RunFormat::Full;
        // 隐藏文字不显示，也不占位置。
        if full && rpr.vanish == Some(true) {
            continue;
        }
        let style = run_style(&rpr, calib);
        let mut start = text.len();
        // 没有文字的 run（只有格式、只有一张图）不成 span：它不占位置，
        // 也不该决定空段落的行高。
        let close = |text: &String, start: usize, style: &RunStyle, spans: &mut Vec<Span>| {
            if text.len() > start {
                spans.push(Span {
                    range: start..text.len(),
                    style: style.clone(),
                });
            }
        };
        for item in &run.items {
            match item {
                RunItem::Text(t) => text.push_str(t),
                RunItem::Tab => text.push('\t'),
                RunItem::Break(BreakKind::Line) => text.push(LINE_BREAK),
                RunItem::Break(BreakKind::Page) => text.push(PAGE_BREAK),
                RunItem::Break(BreakKind::Column) => text.push(COLUMN_BREAK),
                RunItem::NoBreakHyphen => text.push('\u{2011}'),
                RunItem::Drawing { alt } => drawings.push(alt.clone()),
                // 符号单独成一段，用它自己的字体。符号字体（Symbol、Wingdings）里的码位
                // 写成单字节时，实际在私用区 U+F0xx。
                RunItem::Sym { .. } if !full => {}
                RunItem::Sym { font, code } => {
                    let symbolic = font
                        .as_deref()
                        .is_some_and(super::super::fonts::pua::is_symbol_font);
                    let code = if *code <= 0xFF && symbolic {
                        0xF000 + code
                    } else {
                        *code
                    };
                    let Some(c) = char::from_u32(code) else {
                        continue;
                    };
                    close(&text, start, &style, &mut spans);
                    let at = text.len();
                    text.push(c);
                    spans.push(Span {
                        range: at..text.len(),
                        style: RunStyle {
                            font_latin: font.clone().or_else(|| style.font_latin.clone()),
                            font_east_asia: font.clone().or_else(|| style.font_east_asia.clone()),
                            ..style.clone()
                        },
                    });
                    start = text.len();
                }
            }
        }
        close(&text, start, &style, &mut spans);
    }

    // 首行缩进按「字符」算时，用的是段落标记的东亚字号。
    let mark = resolver.run(&ppr, &RPr::default());
    let char_size = mark
        .size_half_pt
        .map(half_pt)
        .or_else(|| spans.first().map(|s| s.style.size_pt))
        .unwrap_or(calib.default_size_pt);

    let mark = run_style(&mark, calib);
    out.push(Block::Para(paragraph(&ppr, text, spans, mark, char_size)));
    for alt in drawings {
        out.push(Block::Placeholder(Placeholder {
            kind: PlaceholderKind::Drawing { alt },
            text: Vec::new(),
        }));
    }
}

fn run_style(rpr: &RPr, calib: &Calib) -> RunStyle {
    let full = calib.run_format == RunFormat::Full;
    let color = rpr.color.unwrap_or([0, 0, 0]);
    RunStyle {
        size_pt: rpr
            .size_half_pt
            .map(half_pt)
            .unwrap_or(calib.default_size_pt),
        bold: rpr.bold.unwrap_or(false),
        italic: rpr.italic.unwrap_or(false),
        underline: rpr
            .underline
            .filter(|u| u.style != UnderlineStyle::None)
            .map(|u| match calib.run_format {
                RunFormat::Full => Underline {
                    style: u.style,
                    color: u.color.unwrap_or(color),
                },
                // 重写前只有单线，颜色跟文字走。
                RunFormat::Legacy => Underline {
                    style: UnderlineStyle::Single,
                    color,
                },
            }),
        strike: rpr.strike.unwrap_or(false),
        double_strike: full && rpr.double_strike.unwrap_or(false),
        background: if full {
            rpr.highlight.flatten().or(rpr.shading.flatten())
        } else {
            None
        },
        color,
        font_latin: rpr.font_ascii.clone(),
        font_east_asia: rpr.font_east_asia.clone(),
        char_spacing: match calib.run_format {
            RunFormat::Full => rpr.spacing.map(tw).unwrap_or(0.0),
            RunFormat::Legacy => 0.0,
        },
    }
}

fn paragraph(
    ppr: &PPr,
    text: String,
    spans: Vec<Span>,
    mark: RunStyle,
    char_size_pt: f32,
) -> Paragraph {
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
        align: match ppr.align {
            Some(model::Align::Center) => Align::Center,
            Some(model::Align::Right) => Align::Right,
            Some(model::Align::Both | model::Align::Distribute) => Align::Justify,
            Some(model::Align::Left) | None => Align::Left,
        },
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
        overflow_punct: ppr.overflow_punct.unwrap_or(true),
        tabs: ppr
            .tabs
            .iter()
            .filter(|t| !matches!(t.align, TabAlign::Bar | TabAlign::Clear))
            .map(|t| TabStop {
                pos: tw(t.pos),
                align: t.align,
                leader: t.leader,
            })
            .collect(),
        numbering_dropped: ppr.numbering,
        text,
        spans,
        mark,
    }
}

/// 表格还画不出来：留下行列数和每个单元格的文字。
fn table_placeholder(t: &model::Table) -> Placeholder {
    let mut text = Vec::new();
    collect_cell_texts(t, &mut text);
    Placeholder {
        kind: PlaceholderKind::Table {
            rows: t.rows.len(),
            cols: t.rows.iter().map(|r| r.cells.len()).max().unwrap_or(0),
        },
        text,
    }
}

/// 每个单元格一条；嵌套表格的单元格紧跟在外层单元格之后。
fn collect_cell_texts(t: &model::Table, out: &mut Vec<String>) {
    for cell in t.rows.iter().flat_map(|r| &r.cells) {
        let mut s = String::new();
        let mut nested = Vec::new();
        for block in &cell.content {
            match block {
                model::Block::Para(p) => {
                    for item in p.runs.iter().flat_map(|r| &r.items) {
                        if let RunItem::Text(t) = item {
                            s.push_str(t);
                        }
                    }
                }
                model::Block::Table(inner) => nested.push(inner),
            }
        }
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            out.push(trimmed.to_string());
        }
        for inner in nested {
            collect_cell_texts(inner, out);
        }
    }
}
