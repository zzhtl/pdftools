//! 排版：IR → 绝对定位的页面。
//!
//! 断行是这里的核心。`unicode-linebreak` 负责回答「**哪里允许断**」——
//! 它实现了 UAX #14，汉字之间天然可断、拉丁词按空格断，而且带避头尾规则
//! （`。、」！？` 不会跑到行首）。自己写「CJK 随处可断」就会出现以句号开头的行，
//! 那是中文排版一眼就能看出的外行错误。
//!
//! `rustybuzz` 负责回答「**排到哪里必须断**」—— 它给出每个字形的精确步进。

use std::ops::Range;

use unicode_linebreak::{linebreaks, BreakOpportunity};

use super::ir::{self, LineSpacing};
use super::model::Align;
use crate::error::Warning;
use crate::error::WarningKind;
use crate::fonts::{
    attaches_to_previous, pua, shape_run, split_by_script, FontBook, FontId, Resolved, ScriptClass,
    ShapedGlyph, ShapedRun,
};

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

#[derive(Debug, Clone, Default)]
pub struct Page {
    pub ops: Vec<PaintOp>,
}

pub struct LaidOut {
    pub pages: Vec<Page>,
    pub warnings: Vec<Warning>,
}

// ---------------------------------------------------------------- 分片

/// 一个「同字体、同字号、同 script」的可整形单元。
struct Piece {
    range: Range<usize>,
    /// 本片的文种。相邻两片文种不同才需要插入中西文间距。
    class: ScriptClass,
    /// 用来整形、绘制的字体。缺字时是回退字体。
    font: FontId,
    /// 决定行高、基线的字体：始终是 run 请求的那个。回退字体只补字形，不抬高行高 ——
    /// 一个「☑」借了符号字体，不该让整行变高。
    metrics_font: FontId,
    synthetic_bold: bool,
    synthetic_italic: bool,
    size_pt: f32,
    color: [u8; 3],
    underline: bool,
    strike: bool,
    shaped: ShapedRun,
    /// 与 `shaped.glyphs` 等长。
    texts: Vec<String>,
    upem: f32,
    /// 本片开头要额外插入的间距（点）。中日韩与西文相邻时加，见 `CJK_LATIN_GAP_EM`。
    gap_before: f32,
}

/// 中日韩文字与西文/数字相邻时插入的间距，单位 em。
///
/// 没有它，「第9条」会挤成一团 —— Word 与 LibreOffice 默认都会加这个间距
/// （Word 的开关是 `w:autoSpaceDE` / `w:autoSpaceDN`，默认开启）。
/// 0.2em 是对着 LibreOffice 实测出来的（9pt 与 12pt 两个字号交叉验证）。
const CJK_LATIN_GAP_EM: f32 = 0.2;

impl Piece {
    /// 本片（含其前置间距）在区间内贡献的宽度。
    fn width_with_gap(&self, from: usize, to: usize) -> f32 {
        // 只有当本片的起点真的落在区间内部时，前置间距才算数 ——
        // 区间从本片中途开始时，那个间距在上一行的行尾，不该重复计入。
        let gap = if self.range.start > from && self.range.start < to {
            self.gap_before
        } else {
            0.0
        };
        gap + self.width(from, to)
    }

    /// 区间 `[from, to)`（段落全局字节偏移）在本片内的宽度，单位点。
    fn width(&self, from: usize, to: usize) -> f32 {
        let a = from.clamp(self.range.start, self.range.end) - self.range.start;
        let b = to.clamp(self.range.start, self.range.end) - self.range.start;
        if b <= a {
            return 0.0;
        }
        let gi = self.shaped.glyph_index_at_byte(a as u32);
        let gj = self.shaped.glyph_index_at_byte(b as u32);
        self.shaped.width_between(gi, gj) as f32 * self.size_pt / self.upem
    }

    /// 字节区间对应的字形下标区间。
    fn glyph_range(&self, from: usize, to: usize) -> Range<usize> {
        let a = from.clamp(self.range.start, self.range.end) - self.range.start;
        let b = to.clamp(self.range.start, self.range.end) - self.range.start;
        let gi = self.shaped.glyph_index_at_byte(a as u32);
        let gj = self.shaped.glyph_index_at_byte(b as u32);
        gi..gj.max(gi)
    }

    fn glyphs_between(&self, from: usize, to: usize) -> &[ShapedGlyph] {
        let r = self.glyph_range(from, to);
        &self.shaped.glyphs[r]
    }

    fn ascent_pt(&self, book: &FontBook) -> f32 {
        let m = book.face(self.metrics_font).metrics();
        m.ascender as f32 * self.size_pt / m.upem as f32
    }

    fn natural_line_pt(&self, book: &FontBook) -> f32 {
        let m = book.face(self.metrics_font).metrics();
        m.default_line_height() * self.size_pt / m.upem as f32
    }
}

/// 收集整形后落到 `.notdef` 的字符。
///
/// 这类字符在 PDF 里会显示成空白或方框，而且多个缺字会共用 GID 0，
/// 连 ToUnicode 都会串。必须能被发现，不能靠用户自己看出来。
fn collect_missing(text: &str, shaped: &ShapedRun, book: &mut FontBook) {
    for g in &shaped.glyphs {
        if g.gid != 0 {
            continue;
        }
        if let Some(c) = text[g.cluster as usize..].chars().next() {
            book.note_missing(c);
        }
    }
}

/// 把一段同文种的文字按「主字体有没有这个字」切开：主字体缺的字交给回退字体。
///
/// 「②」「☑」这类字符常常不在所选字体里；不回退的话，它们都落到同一个 `.notdef` 上，
/// 显示成方框，ToUnicode 还会把它们全抽成第一个缺字。组合符号、变体选择符
/// 跟着前一个字走，否则同一个字会被拆到两个字体里。
fn split_by_coverage(
    text: &str,
    range: Range<usize>,
    primary: Resolved,
    east_asian: bool,
    run: &ir::Run,
    book: &mut FontBook,
) -> Vec<(Range<usize>, Resolved)> {
    let mut out: Vec<(Range<usize>, Resolved)> = Vec::new();
    let mut sibling: Option<Option<Resolved>> = None;
    for (i, c) in text[range.clone()].char_indices() {
        let start = range.start + i;
        let end = start + c.len_utf8();
        // 换行符（`w:br` 在这里是 `\n`）这类控制字符单独成片，不整形、不绘制：
        // 字体里本来就没有它，整形只会得到 .notdef —— 一条误报的缺字，还白占一个字宽，
        // 回退时更会平白多嵌一个字体。片本身要留着，空行的行高靠它撑起来。
        if c.is_control() {
            out.push((start..end, primary));
            continue;
        }
        let font = if attaches_to_previous(c) {
            out.last().map(|(_, f)| *f).unwrap_or(primary)
        } else if book.face(primary.id).has_glyph(c) {
            primary
        } else {
            sibling
                .get_or_insert_with(|| sibling_font(run, east_asian, book))
                .filter(|s| book.face(s.id).has_glyph(c))
                .or_else(|| book.fallback(c, east_asian, run.bold, run.italic))
                .unwrap_or(primary)
        };
        match out.last_mut() {
            Some((r, f)) if *f == font && r.end == start && !is_unpainted(&text[r.clone()]) => {
                r.end = end
            }
            _ => out.push((start..end, font)),
        }
    }
    out
}

/// [`split_by_coverage`] 切出来的控制字符片。
fn is_unpainted(part: &str) -> bool {
    part.starts_with(char::is_control)
}

/// 同一个 run 的另一个字体：西文字体缺 ℃、② 时试中文字体，反过来也一样。
/// 那同样是作者给这段文字选的字体，比系统回退链里的任何字体都更贴近原文 ——
/// 宋体文档里的 ② 不该变成无衬线体。run 只写了一个字体名时，主字体已经是它了。
fn sibling_font(run: &ir::Run, east_asian: bool, book: &mut FontBook) -> Option<Resolved> {
    let (own, other) = if east_asian {
        (&run.font_east_asia, &run.font_latin)
    } else {
        (&run.font_latin, &run.font_east_asia)
    };
    own.as_ref()?;
    book.resolve(other.as_deref(), !east_asian, run.bold, run.italic)
}

/// run 用了 Symbol / Wingdings 这类符号字体、而本机又没有时，把私用区码位换成
/// 意思相同的 Unicode 字符，交给回退字体去画。装了原字体就原样保留。
fn symbol_font_of<'a>(run: &'a ir::Run, book: &FontBook) -> Option<&'a str> {
    [run.font_latin.as_deref(), run.font_east_asia.as_deref()]
        .into_iter()
        .flatten()
        .find(|f| pua::is_symbol_font(f) && !book.has_family(f))
}

fn build_pieces(para: &ir::Paragraph, book: &mut FontBook) -> (String, Vec<Piece>) {
    let mut text = String::new();
    let mut spans: Vec<(Range<usize>, &ir::Run)> = Vec::new();
    for run in &para.runs {
        let start = text.len();
        match symbol_font_of(run, book) {
            // 映射目标与私用区码位同为 3 字节 UTF-8，字节偏移不受影响。
            Some(family) => text.extend(
                run.text
                    .chars()
                    .map(|c| pua::symbol_to_unicode(family, c).unwrap_or(c)),
            ),
            None => text.push_str(&run.text),
        }
        if text.len() > start {
            spans.push((start..text.len(), run));
        }
    }

    let mut pieces: Vec<Piece> = Vec::new();
    for (span, run) in spans {
        let segment = &text[span.clone()];
        // 一个 run 内部还要按 script 再切：中文用 eastAsia 字体，西文用 ascii 字体。
        for (sub, class) in split_by_script(segment) {
            let abs = span.start + sub.start..span.start + sub.end;
            let east = class == ScriptClass::EastAsian;
            let family = if east {
                run.font_east_asia.as_deref().or(run.font_latin.as_deref())
            } else {
                run.font_latin.as_deref().or(run.font_east_asia.as_deref())
            };
            let Some(primary) = book.resolve(family, east, run.bold, run.italic) else {
                // 系统里一个字体都没有。无法排版，但要留痕而不是装作没事。
                book.note_no_font();
                continue;
            };
            for (part, font) in split_by_coverage(&text, abs.clone(), primary, east, run, book) {
                let face = book.face(font.id);
                let upem = face.metrics().upem as f32;
                let shaped = if is_unpainted(&text[part.clone()]) {
                    ShapedRun::empty()
                } else {
                    shape_run(face, &text[part.clone()], class.to_rustybuzz())
                };
                collect_missing(&text[part.clone()], &shaped, book);
                let texts = crate::fonts::cluster_texts(&text[part.clone()], &shaped.glyphs)
                    .into_iter()
                    .map(|(_, t)| t)
                    .collect();
                // 与紧邻的上一片文种不同时，插入中西文间距。
                // 间距按两侧较大的字号算，跟 Word 的观感一致。回退字体切出来的片段
                // 与主字体同文种，不会在它们之间加间距。
                //
                // 但边界上已经有空白时**不加** —— 空格本身已经把两边分开了，再叠一层
                // 会让行变宽并提前折行。实测参照：「正文第1段。」每个边界加 2.4pt，
                // 而「正文第 1 段。」只有空格宽度、没有额外间距。
                let boundary_spaced = pieces.last().is_some_and(|prev| {
                    text[prev.range.clone()].ends_with(char::is_whitespace)
                        || text[part.clone()].starts_with(char::is_whitespace)
                });
                let gap_before = match pieces.last() {
                    Some(prev)
                        if para.auto_space
                            && prev.class != class
                            && prev.range.end == part.start
                            && !boundary_spaced =>
                    {
                        CJK_LATIN_GAP_EM * prev.size_pt.max(run.size_pt)
                    }
                    _ => 0.0,
                };
                pieces.push(Piece {
                    range: part,
                    class,
                    font: font.id,
                    metrics_font: primary.id,
                    synthetic_bold: font.synthetic_bold,
                    synthetic_italic: font.synthetic_italic,
                    size_pt: run.size_pt,
                    color: run.color,
                    underline: run.underline,
                    strike: run.strike,
                    shaped,
                    texts,
                    upem,
                    gap_before,
                });
            }
        }
    }
    (text, pieces)
}

fn width_between(pieces: &[Piece], from: usize, to: usize) -> f32 {
    pieces.iter().map(|p| p.width_with_gap(from, to)).sum()
}

// ---------------------------------------------------------------- 主流程

const PLACEHOLDER_COLOR: [u8; 3] = [0x88, 0x88, 0x88];

pub fn layout(doc: &ir::Document, book: &mut FontBook) -> LaidOut {
    let mut ctx = Ctx {
        page: &doc.page,
        grid: doc.grid,
        pages: vec![Page::default()],
        used: 0.0,
        warnings: Vec::new(),
    };

    let mut numbered = 0usize;
    for block in &doc.blocks {
        match block {
            ir::Block::Para(p) => {
                if p.numbering_dropped {
                    numbered += 1;
                }
                ctx.place_paragraph(p, book)
            }
            ir::Block::Unsupported(u) => ctx.place_placeholder(u, book),
        }
    }

    // 编号与页眉页脚都按文档级汇总，不逐段报 —— 一个 50 项的列表
    // 报 50 条警告，等于没报。
    if numbered > 0 {
        ctx.warnings.push(Warning::new(
            WarningKind::UnsupportedElement,
            format!(
"{numbered} 个段落使用了 Word 的自动编号，本版本不生成编号文字（正文已保留）。需要编号请在 Word 里改成手动输入的序号。"
            ),
        ));
    }
    if doc.has_header_footer {
        ctx.warnings.push(Warning::new(
            WarningKind::UnsupportedElement,
            "文档设置了页眉或页脚，本版本不渲染".to_string(),
        ));
    }

    ctx.warnings.extend(book.take_warnings());
    LaidOut {
        pages: ctx.pages,
        warnings: ctx.warnings,
    }
}

struct Ctx<'a> {
    page: &'a ir::PageGeom,
    grid: Option<ir::Grid>,
    pages: Vec<Page>,
    /// 当前页已用掉的垂直空间（从内容区顶部往下量）。
    used: f32,
    warnings: Vec<Warning>,
}

impl Ctx<'_> {
    fn page_index(&self) -> usize {
        self.pages.len() - 1
    }

    fn new_page(&mut self) {
        self.pages.push(Page::default());
        self.used = 0.0;
    }

    fn at_page_top(&self) -> bool {
        self.used <= f32::EPSILON
    }

    fn place_paragraph(&mut self, para: &ir::Paragraph, book: &mut FontBook) {
        if para.page_break_before && !(self.at_page_top() && self.pages.len() == 1) {
            self.new_page();
        }
        // 段前距在页首也照常生效。
        //
        // 原先这里会在页首吃掉段前距（照搬了 HTML 的习惯），实测 LibreOffice
        // 并不这么做：同一份文档，参照的首行基线距正文顶 40.3pt，而吃掉段前距
        // 只有 24.1pt，整页内容整体上移一截。
        self.used += para.space_before;

        let (text, pieces) = build_pieces(para, book);
        if pieces.is_empty() {
            // 空段落也要占一行的高度，否则段落间距会全乱。
            self.used += empty_line_height(para);
            self.used += para.space_after;
            return;
        }

        let avail_first = self.page.content_width() - para.indent_left - para.indent_right
            + para.first_line.min(0.0);
        let avail_rest = self.page.content_width() - para.indent_left - para.indent_right;
        let first_indent = para.first_line.max(0.0);

        let breaks: Vec<(usize, BreakOpportunity)> = linebreaks(&text).collect();
        let mut line_start = 0usize;
        let mut is_first_line = true;

        while line_start < text.len() {
            let avail = if is_first_line {
                avail_first - first_indent
            } else {
                avail_rest
            };

            let (line_end, mandatory) = find_break(&pieces, &breaks, line_start, avail);
            let is_last = line_end >= text.len();

            self.emit_line(
                &text,
                &pieces,
                line_start..line_end,
                para,
                book,
                is_first_line,
                is_last || mandatory,
                is_last,
            );

            line_start = line_end;
            is_first_line = false;
        }

        self.used += para.space_after;
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_line(
        &mut self,
        text: &str,
        pieces: &[Piece],
        range: Range<usize>,
        para: &ir::Paragraph,
        book: &FontBook,
        is_first_line: bool,
        suppress_justify: bool,
        // 本行是不是所属段落的最后一行 —— 决定倍数行距怎么算，见函数体内的说明。
        is_last_line: bool,
    ) {
        // 行内实际出现的片段决定行高：取最大的那个字体。
        let active: Vec<&Piece> = pieces
            .iter()
            .filter(|p| p.range.start < range.end && p.range.end > range.start)
            .collect();
        // 字体的自然行高（吸附前）。段落最后一行要用到它，见下。
        let unsnapped = active
            .iter()
            .map(|p| p.natural_line_pt(book))
            .fold(0.0f32, f32::max)
            .max(1.0);

        // 行网格：单倍行高先向上吸附到网格整数倍，倍数再乘在这之上。
        // 漏掉这一步，中文文档的行密度会比 Word 高出近一倍。
        let natural = match self.grid {
            Some(g) if para.snap_to_grid => g.snap(unsnapped),
            _ => unsnapped,
        };

        let ascent = active
            .iter()
            .map(|p| p.ascent_pt(book))
            .fold(0.0f32, f32::max);

        let line_height = match para.line {
            // 段落的**最后一行**，倍数带来的额外行距按**吸附前**的自然行高算，
            // 而不是吸附后的。这条实测自 LibreOffice，1.0 / 1.3 / 1.5 / 2.0
            // 四个倍数全部吻合；没有网格时 natural == unsnapped，公式自然退化
            // 成 natural × m。
            //
            // 不区分这一条，每个段落边界都会多出 (m-1) × 吸附增量，
            // 段落密集的文档累积下来会平白多出一整页。
            LineSpacing::Multiple(m) if is_last_line => natural + (m - 1.0).max(0.0) * unsnapped,
            LineSpacing::Multiple(m) => natural * m,
            LineSpacing::Exact(pt) => pt,
            LineSpacing::AtLeast(pt) => natural.max(pt),
        };

        if self.used + line_height > self.page.content_height() && !self.at_page_top() {
            self.new_page();
        }

        // 基线在行框里的位置。
        //
        // 只由**吸附**带来的那部分额外空间加在基线上方（文字坐在网格线上），
        // 而**倍数**带来的额外行距加在下方 —— 所以这里用 natural 而不是
        // line_height：实测参照的首基线位置在 1.0 倍和 1.3 倍行距下完全相同
        // （都是距正文顶 26.55pt），说明倍数不影响基线在行框内的位置。
        //
        // 原先按 line_height/natural 等比缩放 ascent，结果基线比参照高 8.6pt，
        // 整页文字随之上移，看起来上边距小了一截。
        let baseline_from_top = ascent + (natural - unsnapped).max(0.0);
        let y = self.page.h_pt - self.page.margin_top - self.used - baseline_from_top;

        let line_width = width_between(pieces, range.start, range.end);
        let content_left = self.page.margin_left + para.indent_left;
        let avail = self.page.content_width() - para.indent_left - para.indent_right;
        let indent = if is_first_line {
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
        // 中文字之间，空格也分一份）要对着 LibreOffice 实测来定，放在引擎重写里做。
        let mut char_spacing = 0.0f32;
        if para.align == Align::Justify && !suppress_justify {
            let slack = avail - indent - line_width;
            let has_space = text[range.clone()].contains(' ');
            if slack > 0.0 && !has_space {
                let glyphs: usize = active
                    .iter()
                    .map(|p| p.glyphs_between(range.start, range.end).len())
                    .sum();
                if glyphs > 1 {
                    char_spacing = slack / (glyphs - 1) as f32;
                }
            }
        }

        let page = self.pages.last_mut().expect("至少有一页");
        for piece in &active {
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
            let extra_after = vec![char_spacing; glyphs.len()];
            let extra: f32 = extra_after.iter().sum();

            page.ops.push(PaintOp::Text {
                font: piece.font,
                size_pt: piece.size_pt,
                x,
                y,
                glyphs: glyphs.to_vec(),
                unicode: piece.texts[gr].to_vec(),
                color: piece.color,
                extra_after,
                synthetic_bold: piece.synthetic_bold,
                synthetic_italic: piece.synthetic_italic,
            });

            let thickness = (piece.size_pt * 0.05).max(0.5);
            if piece.underline {
                page.ops.push(PaintOp::Rect {
                    x,
                    y: y - piece.size_pt * 0.12,
                    w: w + extra,
                    h: thickness,
                    color: piece.color,
                });
            }
            if piece.strike {
                page.ops.push(PaintOp::Rect {
                    x,
                    y: y + piece.size_pt * 0.26,
                    w: w + extra,
                    h: thickness,
                    color: piece.color,
                });
            }
            x += w + extra;
        }

        self.used += line_height;
    }

    /// 不支持的内容：画一个灰框 + 一句说明 + 能抽出来的文字。
    ///
    /// 不静默丢弃，是因为用户会以为转全了；不整份拒绝，是因为其余内容通常完全可用。
    /// 对法律文书来说，悄悄丢一张表格是**危险**的。
    fn place_placeholder(&mut self, block: &ir::UnsupportedBlock, book: &mut FontBook) {
        let page_no = self.page_index() + 1;
        self.warnings.push(
            Warning::new(
                WarningKind::UnsupportedElement,
                format!("{} 未能渲染", block.kind.label()),
            )
            .at_page(page_no),
        );

        let note = match &block.kind {
            super::model::UnsupportedKind::Drawing { alt: Some(a) } => {
                format!("［图片：{a}　本版本不渲染图片］")
            }
            k => format!("［{}　本版本不渲染，以下为其文字内容］", k.label()),
        };

        let mut paras = vec![placeholder_para(&note, true)];
        for t in &block.text {
            paras.push(placeholder_para(t, false));
        }
        for p in &paras {
            self.place_paragraph(p, book);
        }
    }
}

fn placeholder_para(text: &str, is_note: bool) -> ir::Paragraph {
    ir::Paragraph {
        align: Align::Left,
        indent_left: if is_note { 0.0 } else { 21.0 },
        indent_right: 0.0,
        first_line: 0.0,
        space_before: if is_note { 6.0 } else { 0.0 },
        space_after: if is_note { 2.0 } else { 0.0 },
        line: LineSpacing::Multiple(1.0),
        page_break_before: false,
        // 占位说明不参与网格吸附：它是我们插入的提示，不属于原文排版。
        snap_to_grid: false,
        auto_space: true,
        numbering_dropped: false,
        runs: vec![ir::Run {
            text: text.to_string(),
            size_pt: 9.0,
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            color: PLACEHOLDER_COLOR,
            font_latin: None,
            font_east_asia: None,
        }],
    }
}

fn empty_line_height(para: &ir::Paragraph) -> f32 {
    let base = para.runs.first().map(|r| r.size_pt).unwrap_or(10.5) * 1.2;
    match para.line {
        LineSpacing::Multiple(m) => base * m,
        LineSpacing::Exact(pt) => pt,
        LineSpacing::AtLeast(pt) => base.max(pt),
    }
}

/// 从 `start` 开始，找最后一个装得下的断行点。
///
/// 返回 (断点字节偏移, 是否是强制断行)。
fn find_break(
    pieces: &[Piece],
    breaks: &[(usize, BreakOpportunity)],
    start: usize,
    avail: f32,
) -> (usize, bool) {
    let mut best: Option<usize> = None;
    for (idx, kind) in breaks.iter().copied() {
        if idx <= start {
            continue;
        }
        if kind == BreakOpportunity::Mandatory {
            // 强制断行点之前的内容装不下也得先断在这里之前的某个可断点。
            if width_between(pieces, start, idx) <= avail {
                return (idx, true);
            }
            break;
        }
        if width_between(pieces, start, idx) <= avail {
            best = Some(idx);
        } else {
            break;
        }
    }

    match best {
        Some(b) => (b, false),
        None => {
            // 一个不可断的整体就是超宽（超长 URL、连续数字）。
            // 与其溢出页边，不如在最近的字符边界上硬断。
            let next = breaks
                .iter()
                .copied()
                .find(|(i, _)| *i > start)
                .map(|(i, _)| i);
            (next.unwrap_or(usize::MAX), false)
        }
    }
}
