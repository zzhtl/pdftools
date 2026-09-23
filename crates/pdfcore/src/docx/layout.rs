//! 排版：IR → 绝对定位的页面。
//!
//! 断行是这里的核心。`unicode-linebreak` 负责回答「**哪里允许断**」——
//! 它实现了 UAX #14，汉字之间天然可断、拉丁词按空格断，而且带避头尾规则
//! （`。、」！？` 不会跑到行首）。自己写「CJK 随处可断」就会出现以句号开头的行，
//! 那是中文排版一眼就能看出的外行错误。
//!
//! `rustybuzz` 负责回答「**排到哪里必须断**」—— 它给出每个字形的精确步进。

use std::collections::HashMap;
use std::ops::Range;

use unicode_linebreak::{linebreaks, BreakOpportunity};

use super::ir::{self, LineSpacing};
use super::model::Align;
use crate::error::{Warning, WarningKind};
use crate::fonts::{
    shape_run, split_by_script, system::SystemFonts, FontFace, ScriptClass, ShapedGlyph,
};

pub type FontId = usize;

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
        char_spacing: f32,
        word_spacing: f32,
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

// ---------------------------------------------------------------- 字体解析

/// 把 docx 里写的字体名解析成本机真实存在的字体，解析不到就按类别回退并留痕。
pub struct FontBook {
    system: SystemFonts,
    faces: Vec<FontFace>,
    cache: HashMap<(String, bool, bool), Option<FontId>>,
    substituted: Vec<String>,
    /// 所选字体里没有字形的字符。
    missing: Vec<char>,
    /// 系统里连一个可用字体都找不到。
    no_font_at_all: bool,
}

/// 字体名看起来是不是衬线体。用于挑回退链。
fn looks_serif(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    is_serif_cjk(name)
        || [
            "times", "serif", "georgia", "garamond", "book", "roman", "song", "ming", "kai",
        ]
        .iter()
        .any(|k| lower.contains(k))
}

/// 常见中文字体名 → 我们的回退链类别。
fn is_serif_cjk(name: &str) -> bool {
    matches!(
        name,
        "宋体"
            | "SimSun"
            | "NSimSun"
            | "新宋体"
            | "仿宋"
            | "FangSong"
            | "仿宋_GB2312"
            | "楷体"
            | "KaiTi"
            | "楷体_GB2312"
            | "STSong"
            | "Songti SC"
            | "STFangsong"
            | "Source Han Serif SC"
            | "Noto Serif CJK SC"
    )
}

impl FontBook {
    pub fn new() -> Self {
        Self {
            system: SystemFonts::load(),
            faces: Vec::new(),
            cache: HashMap::new(),
            substituted: Vec::new(),
            missing: Vec::new(),
            no_font_at_all: false,
        }
    }

    pub fn face(&self, id: FontId) -> &FontFace {
        &self.faces[id]
    }

    pub fn faces(&self) -> &[FontFace] {
        &self.faces
    }

    /// 解析一个字体请求。`east_asian` 决定回退链，因为同一个 run 的汉字和
    /// 西文要走不同的字体（`w:rFonts` 本来就给了两个名字）。
    pub fn resolve(
        &mut self,
        family: Option<&str>,
        east_asian: bool,
        bold: bool,
        italic: bool,
    ) -> Option<FontId> {
        let key = (
            family.unwrap_or("").to_string() + if east_asian { "|ea" } else { "|latin" },
            bold,
            italic,
        );
        if let Some(hit) = self.cache.get(&key) {
            return *hit;
        }

        // 先按原名精确找。找到就用，这是最忠实于文档的结果。
        let mut found = family.and_then(|f| self.system.query(f, bold, italic));

        if found.is_none() {
            // 找不到就按类别回退，并记下这次替换 —— 用户有权知道字体被换了。
            let chain = if east_asian {
                if family.map(is_serif_cjk).unwrap_or(false) {
                    crate::fonts::system::PDF_SERIF_PREFERENCE
                } else {
                    crate::fonts::system::PDF_SANS_PREFERENCE
                }
            } else if family.map(looks_serif).unwrap_or(false) {
                crate::fonts::system::LATIN_SERIF_PREFERENCE
            } else {
                crate::fonts::system::LATIN_SANS_PREFERENCE
            };
            found = self.system.find(chain, bold, italic);
            if found.is_none() {
                // 首选链全军覆没时，把其余所有链都试一遍，最后退到系统里任意一个字体。
                //
                // 这一步守的是一条底线：**绝不因为找不到字体就把文字丢掉**。
                // 哪怕最终字体缺少对应字形（显示为空白），文字也仍在 PDF 里、仍可搜索，
                // 而且 `.notdef` 检测会把这件事报出来。悄悄少一整段字要严重得多。
                for alt in [
                    crate::fonts::system::PDF_SANS_PREFERENCE,
                    crate::fonts::system::PDF_SERIF_PREFERENCE,
                    crate::fonts::system::LATIN_SANS_PREFERENCE,
                    crate::fonts::system::LATIN_SERIF_PREFERENCE,
                ] {
                    found = self.system.find(alt, bold, italic);
                    if found.is_some() {
                        break;
                    }
                }
            }
            if found.is_none() {
                found = self.system.any_face(bold, italic);
            }
            if let (Some(want), Some(got)) = (family, found.as_ref()) {
                let note = format!("字体「{want}」不可用，已替换为「{}」", got.family);
                if !self.substituted.contains(&note) {
                    self.substituted.push(note);
                }
            }
        }

        let id = found.map(|f| {
            self.faces.push(f.face);
            self.faces.len() - 1
        });
        self.cache.insert(key, id);
        id
    }

    fn take_warnings(&mut self) -> Vec<Warning> {
        let mut out: Vec<Warning> = self
            .substituted
            .drain(..)
            .map(|d| Warning::new(WarningKind::FontSubstituted, d))
            .collect();
        if self.no_font_at_all {
            out.push(Warning::new(
                WarningKind::FontSubstituted,
                "系统中找不到任何可用字体，部分内容无法排版。请安装 Noto Sans CJK 或思源黑体。",
            ));
        }
        if !self.missing.is_empty() {
            let chars: String = self.missing.drain(..).take(40).collect();
            out.push(Warning::new(
                WarningKind::FontSubstituted,
                format!("以下字符在可用字体中没有字形，PDF 里会显示为空白：{chars}"),
            ));
        }
        out
    }
}

impl Default for FontBook {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------- 分片

/// 一个「同字体、同字号、同 script」的可整形单元。
struct Piece {
    range: Range<usize>,
    /// 本片的文种。相邻两片文种不同才需要插入中西文间距。
    class: ScriptClass,
    font: FontId,
    size_pt: f32,
    color: [u8; 3],
    underline: bool,
    strike: bool,
    shaped: crate::fonts::ShapedRun,
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
        let m = book.face(self.font).metrics();
        m.ascender as f32 * self.size_pt / self.upem
    }

    fn natural_line_pt(&self, book: &FontBook) -> f32 {
        let m = book.face(self.font).metrics();
        m.default_line_height() * self.size_pt / self.upem
    }
}

/// 收集整形后落到 `.notdef` 的字符。
///
/// 这类字符在 PDF 里会显示成空白或方框，而且多个缺字会共用 GID 0，
/// 连 ToUnicode 都会串。必须能被发现，不能靠用户自己看出来。
fn collect_missing(text: &str, shaped: &crate::fonts::ShapedRun, out: &mut Vec<char>) {
    for g in &shaped.glyphs {
        if g.gid != 0 {
            continue;
        }
        if let Some(c) = text[g.cluster as usize..].chars().next() {
            if !out.contains(&c) {
                out.push(c);
            }
        }
    }
}

fn build_pieces(para: &ir::Paragraph, book: &mut FontBook) -> (String, Vec<Piece>) {
    let mut text = String::new();
    let mut spans: Vec<(Range<usize>, &ir::Run)> = Vec::new();
    for run in &para.runs {
        let start = text.len();
        text.push_str(&run.text);
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
            let Some(font) = book.resolve(family, east, run.bold, run.italic) else {
                // 系统里一个字体都没有。无法排版，但要留痕而不是装作没事。
                book.no_font_at_all = true;
                continue;
            };
            let upem = book.face(font).metrics().upem as f32;
            let shaped = shape_run(book.face(font), &text[abs.clone()], class.to_rustybuzz());
            collect_missing(&text[abs.clone()], &shaped, &mut book.missing);
            let texts = crate::fonts::cluster_texts(&text[abs.clone()], &shaped.glyphs)
                .into_iter()
                .map(|(_, t)| t)
                .collect();
            // 与紧邻的上一片文种不同时，插入中西文间距。
            // 间距按两侧较大的字号算，跟 Word 的观感一致。
            //
            // 但边界上已经有空白时**不加** —— 空格本身已经把两边分开了，再叠一层
            // 会让行变宽并提前折行。实测参照：「正文第1段。」每个边界加 2.4pt，
            // 而「正文第 1 段。」只有空格宽度、没有额外间距。
            let boundary_spaced = pieces.last().is_some_and(|prev| {
                text[prev.range.clone()].ends_with(char::is_whitespace)
                    || text[abs.clone()].starts_with(char::is_whitespace)
            });
            let gap_before = match pieces.last() {
                Some(prev)
                    if para.auto_space
                        && prev.class != class
                        && prev.range.end == abs.start
                        && !boundary_spaced =>
                {
                    CJK_LATIN_GAP_EM * prev.size_pt.max(run.size_pt)
                }
                _ => 0.0,
            };
            pieces.push(Piece {
                range: abs,
                class,
                font,
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

        // 两端对齐：把剩余空间摊进字间或词间。段落最后一行不参与。
        let (mut char_spacing, mut word_spacing) = (0.0f32, 0.0f32);
        if para.align == Align::Justify && !suppress_justify {
            let slack = avail - indent - line_width;
            if slack > 0.0 {
                let slice = &text[range.clone()];
                let spaces = slice.chars().filter(|c| *c == ' ').count();
                if spaces > 0 {
                    word_spacing = slack / spaces as f32;
                } else {
                    let glyphs: usize = active
                        .iter()
                        .map(|p| p.glyphs_between(range.start, range.end).len())
                        .sum();
                    if glyphs > 1 {
                        char_spacing = slack / (glyphs - 1) as f32;
                    }
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
            let extra = char_spacing * glyphs.len() as f32
                + word_spacing
                    * text[range.clone()].chars().filter(|c| *c == ' ').count() as f32
                    * 0.0; // 词间距由 PDF 的 Tw 施加，这里不重复计入宽度

            let gr = piece.glyph_range(range.start, range.end);
            page.ops.push(PaintOp::Text {
                font: piece.font,
                size_pt: piece.size_pt,
                x,
                y,
                glyphs: glyphs.to_vec(),
                unicode: piece.texts[gr].to_vec(),
                color: piece.color,
                char_spacing,
                word_spacing,
            });

            let metrics = book.face(piece.font).metrics();
            let scale = piece.size_pt / piece.upem;
            if piece.underline {
                let thickness = (metrics.upem as f32 * 0.05 * scale).max(0.5);
                page.ops.push(PaintOp::Rect {
                    x,
                    y: y - piece.size_pt * 0.12,
                    w: w + extra,
                    h: thickness,
                    color: piece.color,
                });
            }
            if piece.strike {
                let thickness = (metrics.upem as f32 * 0.05 * scale).max(0.5);
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
