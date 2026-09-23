//! 段落文字的分片、整形与断行。
//!
//! 断行是这里的核心。`unicode-linebreak` 负责回答「**哪里允许断**」——
//! 它实现了 UAX #14，汉字之间天然可断、拉丁词按空格断，而且带避头尾规则
//! （`。、」！？` 不会跑到行首）。自己写「CJK 随处可断」就会出现以句号开头的行，
//! 那是中文排版一眼就能看出的外行错误。
//!
//! `rustybuzz` 负责回答「**排到哪里必须断**」—— 它给出每个字形的精确步进。
//!
//! 整形结果（[`ShapedPara`]）与栏宽无关：同一段落换一个宽度重排（表格列宽试算、
//! 页眉里的域换了值）不必重新整形。

use std::ops::Range;

use unicode_linebreak::{linebreaks, BreakOpportunity};

use super::calib::{AutoSpace, Breaks, Calib, CharClass, Kerning, Tabs};
use super::script;
use crate::docx::ir;
use crate::fonts::{
    attaches_to_previous, cluster_texts, pua, shape_run_with, split_by_script, FontBook, FontId,
    Resolved, ScriptClass, ShapedGlyph, ShapedRun,
};

/// 中日韩文字与西文/数字相邻时插入的间距，单位 em。
///
/// 没有它，「第9条」会挤成一团 —— Word 与 LibreOffice 默认都会加这个间距
/// （Word 的开关是 `w:autoSpaceDE` / `w:autoSpaceDN`，默认开启）。
/// 0.2em 是对着 LibreOffice 实测出来的（9pt 与 12pt 两个字号交叉验证）。
const CJK_LATIN_GAP_EM: f32 = 0.2;

/// 可以伸出右边距的句读标点。见 `HangingPunct::Punctuation`。
const HANGING_PUNCT: &[char] = &[
    '，', '。', '、', '；', '：', '！', '？', '．', ',', '.', ';', ':', '!', '?',
];

/// 行尾哪些东西可以悬挂在右边距外。
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Hang {
    pub spaces: bool,
    pub punct: bool,
}

/// 上下标的字号比例。LibreOffice 实测 58%。
const SUPERSCRIPT_SCALE: f32 = 0.58;

/// 一个「同字体、同字号、同 script」的可整形单元。
pub(super) struct Piece {
    pub range: Range<usize>,
    /// 本片的文种。相邻两片文种不同才需要插入中西文间距。
    pub class: ScriptClass,
    /// 用来整形、绘制的字体。缺字时是回退字体。
    pub font: FontId,
    /// 决定行高、基线的字体：始终是 run 请求的那个。回退字体只补字形，不抬高行高 ——
    /// 一个「☑」借了符号字体，不该让整行变高。
    pub metrics_font: FontId,
    pub synthetic_bold: bool,
    pub synthetic_italic: bool,
    /// 画出来的字号。上下标比 run 的字号小。
    pub size_pt: f32,
    /// 字符间距（点）：每个字后面加这么多，负数是紧缩。
    pub letter_spacing: f32,
    /// `letter_ends[i]` = 前 i 个字形里有几个是字（cluster）的末尾。没有字符间距时为空。
    letter_ends: Vec<u32>,
    /// 决定行高、基线用的字号：始终是 run 的字号，上下标不让行变矮。
    pub metrics_size_pt: f32,
    /// 基线的升降（点），正数往上：上下标与 `w:position`。
    pub rise: f32,
    pub color: [u8; 3],
    pub underline: Option<ir::Underline>,
    pub strike: bool,
    pub double_strike: bool,
    pub background: Option<[u8; 3]>,
    pub link: Option<String>,
    pub shaped: ShapedRun,
    /// 与 `shaped.glyphs` 等长。
    pub texts: Vec<String>,
    upem: f32,
    /// 本片开头要额外插入的间距（点）。中日韩与西文相邻时加，见 `CJK_LATIN_GAP_EM`。
    pub gap_before: f32,
    /// 行内对象（图片）占的这一片：不画字，只占宽度、撑高行。
    pub object: Option<ObjectBox>,
}

/// 行内对象在一片里的样子，见 [`ir::InlineObject`]。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct ObjectBox {
    /// 第几个对象（[`ir::Paragraph::objects`] 的下标）。
    pub index: usize,
    pub width: f32,
    pub height: f32,
}

/// 对象片的「字形」按千分之一点量宽度：步进是整数字体单位，字号取 1、每 em 1000 单位。
const OBJECT_UPEM: f32 = 1000.0;

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

    /// 区间 `[from, to)`（段落全局字节偏移）在本片内的宽度，单位点。含字符间距。
    pub fn width(&self, from: usize, to: usize) -> f32 {
        let a = from.clamp(self.range.start, self.range.end) - self.range.start;
        let b = to.clamp(self.range.start, self.range.end) - self.range.start;
        if b <= a {
            return 0.0;
        }
        let gi = self.shaped.glyph_index_at_byte(a as u32);
        let gj = self.shaped.glyph_index_at_byte(b as u32);
        let glyphs = self.shaped.width_between(gi, gj) as f32 * self.size_pt / self.upem;
        if self.letter_ends.is_empty() {
            glyphs
        } else {
            glyphs + self.letter_spacing * (self.letter_ends[gj] - self.letter_ends[gi]) as f32
        }
    }

    /// 第 `k` 个字形之后的字符间距（点）：字的最后一个字形才有。
    pub fn letter_spacing_after(&self, k: usize) -> f32 {
        if self.letter_ends.is_empty() || self.letter_ends[k + 1] == self.letter_ends[k] {
            0.0
        } else {
            self.letter_spacing
        }
    }

    /// 字节区间对应的字形下标区间。
    pub fn glyph_range(&self, from: usize, to: usize) -> Range<usize> {
        let a = from.clamp(self.range.start, self.range.end) - self.range.start;
        let b = to.clamp(self.range.start, self.range.end) - self.range.start;
        let gi = self.shaped.glyph_index_at_byte(a as u32);
        let gj = self.shaped.glyph_index_at_byte(b as u32);
        gi..gj.max(gi)
    }

    pub fn glyphs_between(&self, from: usize, to: usize) -> &[ShapedGlyph] {
        let r = self.glyph_range(from, to);
        &self.shaped.glyphs[r]
    }

    /// 基线离行顶多远。`gap_above`：字体的行间距算在上面（[`LineGap::Above`](super::calib::LineGap)）。
    pub fn ascent_pt(&self, book: &FontBook, gap_above: bool) -> f32 {
        let m = book.face(self.metrics_font).metrics();
        let gap = if gap_above { m.line_gap as f32 } else { 0.0 };
        (m.ascender as f32 + gap) * self.metrics_size_pt / m.upem as f32
    }

    pub fn natural_line_pt(&self, book: &FontBook) -> f32 {
        let m = book.face(self.metrics_font).metrics();
        m.default_line_height() * self.metrics_size_pt / m.upem as f32
    }
}

/// 整形好的段落：与栏宽无关，换个宽度重新断行不必重新整形。
pub(super) struct ShapedPara {
    /// 实际整形的文字。与 IR 的文字不完全相同：符号字体的私用区码位换成了 Unicode，
    /// 制表符、分行符换成了排版时的替身。
    pub text: String,
    pub pieces: Vec<Piece>,
    /// UAX #14 的断行机会，按偏移升序。
    pub breaks: Vec<(usize, BreakOpportunity)>,
    /// 制表符的偏移，升序。
    pub tabs: Vec<usize>,
}

/// 制表符跳到哪里。位置都从正文区左缘量起（点）。
pub(super) struct TabRules<'a> {
    pub stops: &'a [ir::TabStop],
    pub default: f32,
    /// 悬挂缩进时，左缩进处有一个隐含的左对齐制表位（Word 的规矩：编号后的制表符
    /// 靠它对齐到正文）。
    pub implicit: Option<f32>,
}

/// 正好停在制表位上的文字，下一个制表符跳到再下一个位置。
const TAB_EPS: f32 = 1e-3;

impl TabRules<'_> {
    /// 位置 `x` 之后的下一个制表位。自定义位优先；越过最后一个自定义位之后才用默认位。
    pub fn next(&self, x: f32) -> ir::TabStop {
        let implicit = self.implicit.map(|pos| ir::TabStop {
            pos,
            align: ir::TabAlign::Left,
            leader: ir::TabLeader::None,
        });
        self.stops
            .iter()
            .copied()
            .chain(implicit)
            .filter(|t| t.pos > x + TAB_EPS)
            .min_by(|a, b| a.pos.total_cmp(&b.pos))
            .unwrap_or_else(|| {
                let d = self.default.max(1.0);
                ir::TabStop {
                    pos: ((x + TAB_EPS) / d).floor() * d + d,
                    align: ir::TabAlign::Left,
                    leader: ir::TabLeader::None,
                }
            })
    }
}

impl ShapedPara {
    /// 与区间 `[from, to)` 相交的片。片按偏移升序、首尾相接。
    pub fn pieces_in(&self, from: usize, to: usize) -> &[Piece] {
        let first = self.pieces.partition_point(|p| p.range.end <= from);
        let last = first + self.pieces[first..].partition_point(|p| p.range.start < to);
        &self.pieces[first..last]
    }

    /// 区间 `[from, to)` 排成一行有多宽（含中西文间距）。
    pub fn width(&self, from: usize, to: usize) -> f32 {
        // 与区间不相交的片贡献恰好是 0，只累加相交的片，结果逐位相同。
        self.pieces_in(from, to)
            .iter()
            .map(|p| p.width_with_gap(from, to))
            .sum()
    }

    /// 行 `[start, end)` 里算行宽的部分到哪里为止：悬挂在右边距外的不算。
    /// 先是行尾的半角空格（连同其后的换行符），再是紧挨着它们的一个句读标点。
    pub fn measured_end(&self, start: usize, end: usize, hang: Hang) -> usize {
        let end = end.min(self.text.len());
        let mut line = &self.text[start..end];
        if hang.spaces {
            line = line.trim_end_matches(|c: char| c == ' ' || c.is_control());
        }
        if hang.punct {
            if let Some(c) = line
                .chars()
                .next_back()
                .filter(|c| HANGING_PUNCT.contains(c))
            {
                line = &line[..line.len() - c.len_utf8()];
            }
        }
        start + line.len()
    }

    /// 制表符（在 `tab` 处）从 `x` 跳到哪里，以及用的是哪个制表位。
    /// `[tab + 1, seg_end)` 是它后面、到下一个制表符或行尾的文字：右对齐、居中、
    /// 小数点位要按它的宽度往回让，但不会退到 `x` 之前。
    pub fn tab_target(
        &self,
        x: f32,
        tab: usize,
        seg_end: usize,
        rules: &TabRules,
    ) -> (f32, ir::TabStop) {
        let stop = rules.next(x);
        let after = tab + 1;
        let target = match stop.align {
            ir::TabAlign::Right => stop.pos - self.width(after, seg_end),
            ir::TabAlign::Center => stop.pos - self.width(after, seg_end) / 2.0,
            ir::TabAlign::Decimal => {
                let dot = self.text[after..seg_end]
                    .find(['.', '．'])
                    .map_or(seg_end, |i| after + i);
                stop.pos - self.width(after, dot)
            }
            _ => stop.pos,
        };
        (target.max(x), stop)
    }

    /// 行 `[start, end)` 从 `x0` 开始排，右缘到哪里（都从正文区左缘量起）。
    pub fn advance(&self, start: usize, end: usize, x0: f32, rules: &TabRules) -> f32 {
        let first = self.tabs.partition_point(|t| *t < start);
        let last = self.tabs.partition_point(|t| *t < end);
        let tabs = &self.tabs[first..last];
        let mut x = x0;
        let mut seg = start;
        for (i, &t) in tabs.iter().enumerate() {
            x += self.width(seg, t);
            let seg_end = tabs.get(i + 1).copied().unwrap_or(end);
            x = self.tab_target(x, t, seg_end, rules).0;
            seg = t + 1;
        }
        x + self.width(seg, end)
    }

    /// 从 `start` 开始，找最后一个装得下的断行点。返回 (断点偏移, 是否是强制断行)。
    /// `x0` 是本行起点（从正文区左缘量起），有制表符时要靠它定位。
    /// `char_boundary`：一整串不可断的内容放不下时，在字符边界上断开（否则整串越过右边距）。
    pub fn next_break(
        &self,
        start: usize,
        avail: f32,
        hang: Hang,
        x0: f32,
        rules: &TabRules,
        char_boundary: bool,
    ) -> (usize, bool) {
        let first = self.breaks.partition_point(|(i, _)| *i <= start);
        let fits = |idx| {
            let end = self.measured_end(start, idx, hang);
            if self.tabs.is_empty() {
                self.width(start, end) <= avail
            } else {
                self.advance(start, end, x0, rules) - x0 <= avail
            }
        };
        let mut best: Option<usize> = None;
        for &(idx, kind) in &self.breaks[first..] {
            if kind == BreakOpportunity::Mandatory {
                // 强制断行点之前的内容装不下也得先断在这里之前的某个可断点。
                if fits(idx) {
                    return (idx, true);
                }
                break;
            }
            if fits(idx) {
                best = Some(idx);
            } else {
                break;
            }
        }
        let next = self
            .breaks
            .get(first)
            .map(|(i, _)| *i)
            .unwrap_or(self.text.len());
        match best {
            Some(b) => (b, false),
            // 一个不可断的整体比行还宽（超长 URL、连续数字）。
            None if !char_boundary => (next, false),
            None => {
                // 放得下的最后一个字符边界；组合符号不能和前一个字拆开。
                // 一个字都放不下时也放一个，否则永远排不出去。
                let mut cut = None;
                for (i, c) in self.text[start..next].char_indices().skip(1) {
                    if attaches_to_previous(c) {
                        continue;
                    }
                    if !fits(start + i) {
                        break;
                    }
                    cut = Some(start + i);
                }
                let first_char = self.text[start..next]
                    .char_indices()
                    .skip(1)
                    .find(|(_, c)| !attaches_to_previous(*c))
                    .map_or(next, |(i, _)| start + i);
                (cut.unwrap_or(first_char), false)
            }
        }
    }
}

/// 分页符、分栏符：以它们结尾的行之后要换页。
pub(super) fn is_page_break(c: char) -> bool {
    c == ir::PAGE_BREAK || c == ir::COLUMN_BREAK
}

/// `char_pitch`：字符网格的格宽，汉字与全角标点按整格排。
pub(super) fn shape(
    para: &ir::Paragraph,
    book: &mut FontBook,
    calib: &Calib,
    char_pitch: Option<f32>,
) -> ShapedPara {
    let typed_breaks = calib.breaks == Breaks::Typed;
    let tab_stops = calib.tabs == Tabs::Stops;
    let mut text = String::with_capacity(para.text.len());
    let mut spans: Vec<(Range<usize>, &ir::RunStyle)> = Vec::with_capacity(para.spans.len());
    for span in &para.spans {
        let start = text.len();
        let symbol = symbol_font_of(&span.style, book);
        for c in para.text[span.range.clone()].chars() {
            text.push(match c {
                // 旧规则：制表符当一个全角空格。否则它是不绘制的控制字符，
                // 排版时跳到制表位。
                '\t' if !tab_stops => '\u{3000}',
                ir::LINE_BREAK => '\n',
                // 分页符、分栏符本身就是强制断行点（UAX #14 的 BK），也不绘制；
                // 旧规则把它们都当换行。
                ir::PAGE_BREAK | ir::COLUMN_BREAK if !typed_breaks => '\n',
                c => symbol
                    .and_then(|family| pua::symbol_to_unicode(family, c))
                    .unwrap_or(c),
            });
        }
        spans.push((start..text.len(), &span.style));
    }

    // 按字再切：中文用 eastAsia 字体，西文用 ascii 字体。
    let classes = match calib.char_class {
        CharClass::Blocks => {
            let hints: Vec<_> = spans
                .iter()
                .map(|(r, st)| (r.clone(), st.hint_east_asia))
                .collect();
            script::classify(&text, &hints)
        }
        // 旧规则在每个 run 内部单独切。
        CharClass::Legacy => spans
            .iter()
            .flat_map(|(span, _)| {
                split_by_script(&text[span.clone()])
                    .into_iter()
                    .map(|(r, c)| (span.start + r.start..span.start + r.end, c))
            })
            .collect(),
    };

    let mut pieces: Vec<Piece> = Vec::new();
    // 已经排过几个行内对象：文字里第 k 个替身是第 k 个对象。
    let mut objects = 0;
    for (span, style) in spans {
        for (whole, class) in script::within(&classes, span) {
            for (abs, is_object) in split_objects(&text, whole) {
                if is_object {
                    let Some(obj) = para.objects.get(objects) else {
                        continue;
                    };
                    objects += 1;
                    let east = class == ScriptClass::EastAsian;
                    let family = style
                        .font_latin
                        .as_deref()
                        .or(style.font_east_asia.as_deref());
                    let Some(font) = book.resolve(family, east, false, false) else {
                        book.note_no_font();
                        continue;
                    };
                    pieces.push(Piece {
                        range: abs,
                        class,
                        font: font.id,
                        metrics_font: font.id,
                        synthetic_bold: false,
                        synthetic_italic: false,
                        size_pt: 1.0,
                        letter_spacing: 0.0,
                        letter_ends: Vec::new(),
                        metrics_size_pt: 1.0,
                        rise: style.position_pt,
                        color: style.color,
                        underline: None,
                        strike: false,
                        double_strike: false,
                        background: None,
                        link: style.link.clone(),
                        shaped: ShapedRun::object((obj.width * OBJECT_UPEM).round() as i32),
                        texts: vec![String::new()],
                        upem: OBJECT_UPEM,
                        gap_before: 0.0,
                        object: Some(ObjectBox {
                            index: objects - 1,
                            width: obj.width,
                            height: obj.height,
                        }),
                    });
                    continue;
                }
                let east = class == ScriptClass::EastAsian;
                let family = if east {
                    style
                        .font_east_asia
                        .as_deref()
                        .or(style.font_latin.as_deref())
                } else {
                    style
                        .font_latin
                        .as_deref()
                        .or(style.font_east_asia.as_deref())
                };
                let Some(primary) = book.resolve(family, east, style.bold, style.italic) else {
                    // 系统里一个字体都没有。无法排版，但要留痕而不是装作没事。
                    book.note_no_font();
                    continue;
                };
                for (part, font) in
                    split_by_coverage(&text, abs.clone(), primary, east, style, book)
                {
                    let face = book.face(font.id);
                    let m = face.metrics();
                    let upem = m.upem as f32;
                    // 上下标画小一号，抬高「上伸 + 行间距」、压低「下伸」的余下部分
                    // （LibreOffice 实测：58% 字号，12pt 时上标抬高 4.7pt、下标压低 1.1pt）。
                    let (size, rise) = match style.vert_align {
                        ir::VertAlign::Baseline => (style.size_pt, style.position_pt),
                        ir::VertAlign::Superscript => (
                            style.size_pt * SUPERSCRIPT_SCALE,
                            style.position_pt
                                + (m.ascender + m.line_gap) as f32 / upem
                                    * style.size_pt
                                    * (1.0 - SUPERSCRIPT_SCALE),
                        ),
                        ir::VertAlign::Subscript => (
                            style.size_pt * SUPERSCRIPT_SCALE,
                            style.position_pt
                                + m.descender as f32 / upem
                                    * style.size_pt
                                    * (1.0 - SUPERSCRIPT_SCALE),
                        ),
                    };
                    let shaped = if is_unpainted(&text[part.clone()]) {
                        ShapedRun::empty()
                    } else {
                        let kern = calib.kerning == Kerning::Always || style.kern;
                        shape_run_with(face, &text[part.clone()], class.to_rustybuzz(), kern)
                    };
                    // 字符间距加在每个字（cluster）的最后一个字形之后；记下到每个字形为止
                    // 有几个字的末尾，量宽度时一次减法就够。不折算成字体单位：
                    // 宋体每 em 只有 256 个单位，取整后每个字会差出 0.016pt。
                    // 字符网格：汉字、全角标点（一个 em 宽）撑满整格。字号比格宽略大时
                    // 仍占一格（公文的三号字就比格宽大 0.2pt），明显更大时占两格。
                    let grid_extra = match char_pitch {
                        Some(p) if east && !is_unpainted(&text[part.clone()]) => {
                            let cells = (style.size_pt / p - 0.05).ceil().max(1.0);
                            cells * p - style.size_pt
                        }
                        _ => 0.0,
                    };
                    let letter_spacing = style.char_spacing + grid_extra;
                    let letter_ends = if letter_spacing != 0.0 {
                        let g = &shaped.glyphs;
                        std::iter::once(0)
                            .chain((0..g.len()).scan(0u32, |n, i| {
                                if i + 1 == g.len() || g[i + 1].cluster != g[i].cluster {
                                    *n += 1;
                                }
                                Some(*n)
                            }))
                            .collect()
                    } else {
                        Vec::new()
                    };
                    collect_missing(&text[part.clone()], &shaped, book);
                    let texts = cluster_texts(&text[part.clone()], &shaped.glyphs)
                        .into_iter()
                        .map(|(_, t)| t)
                        .collect();
                    // 与紧邻的上一片之间插入中西文间距，加在哪里见 [`AutoSpace`]。
                    // 间距按两侧较大的字号算，跟 Word 的观感一致。回退字体切出来的片段
                    // 与主字体同文种，不会在它们之间加间距。
                    //
                    // 边界上已经有空白时**不加** —— 空格本身已经把两边分开了，再叠一层
                    // 会让行变宽并提前折行。实测参照：「正文第1段。」每个边界加 2.4pt，
                    // 而「正文第 1 段。」只有空格宽度、没有额外间距。
                    let spaced = |prev: &Piece| match calib.auto_space {
                        AutoSpace::Legacy => {
                            prev.class != class
                                && !text[prev.range.clone()].ends_with(char::is_whitespace)
                                && !text[part.clone()].starts_with(char::is_whitespace)
                        }
                        AutoSpace::Letters => {
                            let before = text[prev.range.clone()].chars().next_back();
                            let after = text[part.clone()].chars().next();
                            matches!((before, after), (Some(b), Some(a))
                            if autospaced((b, prev.class), (a, class)))
                        }
                    };
                    let gap_before = match pieces.last() {
                        Some(prev)
                            if para.auto_space && prev.range.end == part.start && spaced(prev) =>
                        {
                            CJK_LATIN_GAP_EM * prev.size_pt.max(style.size_pt)
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
                        size_pt: size,
                        letter_spacing,
                        letter_ends,
                        metrics_size_pt: style.size_pt,
                        rise,
                        color: style.color,
                        underline: style.underline,
                        strike: style.strike,
                        double_strike: style.double_strike,
                        background: style.background,
                        link: style.link.clone(),
                        shaped,
                        texts,
                        upem,
                        gap_before,
                        object: None,
                    });
                }
            }
        }
    }
    let breaks = linebreaks(&text).collect();
    let tabs = text.match_indices('\t').map(|(i, _)| i).collect();
    ShapedPara {
        text,
        pieces,
        breaks,
        tabs,
    }
}

/// 把区间按行内对象的替身切开：(子区间, 是不是一个对象)。
fn split_objects(text: &str, range: Range<usize>) -> Vec<(Range<usize>, bool)> {
    let mut out = Vec::new();
    let mut start = range.start;
    for (i, c) in text[range.clone()].char_indices() {
        if c == ir::OBJECT {
            let at = range.start + i;
            if at > start {
                out.push((start..at, false));
            }
            out.push((at..at + c.len_utf8(), true));
            start = at + c.len_utf8();
        }
    }
    if start < range.end {
        out.push((start..range.end, false));
    }
    out
}

/// 相邻两个字之间要不要加中西文间距（[`AutoSpace::Letters`]）：一边是汉字、假名、
/// 谚文，另一边是用西文字体的字母或数字。
fn autospaced((a, a_class): (char, ScriptClass), (b, b_class): (char, ScriptClass)) -> bool {
    let ideograph = |c: char, class| class == ScriptClass::EastAsian && c.is_alphabetic();
    let alnum = |c: char, class| class == ScriptClass::Latin && c.is_alphanumeric();
    (ideograph(a, a_class) && alnum(b, b_class)) || (alnum(a, a_class) && ideograph(b, b_class))
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
    style: &ir::RunStyle,
    book: &mut FontBook,
) -> Vec<(Range<usize>, Resolved)> {
    let mut out: Vec<(Range<usize>, Resolved)> = Vec::new();
    let mut sibling: Option<Option<Resolved>> = None;
    for (i, c) in text[range.clone()].char_indices() {
        let start = range.start + i;
        let end = start + c.len_utf8();
        // 换行符这类控制字符单独成片，不整形、不绘制：字体里本来就没有它，
        // 整形只会得到 .notdef —— 一条误报的缺字，还白占一个字宽，
        // 回退时更会平白多嵌一个字体。片本身要留着，空行的行高靠它撑起来。
        if c.is_control() {
            // 制表符属于 ASCII，用西文字体：它的前导符（一串点）要按西文字体的点来画。
            let font = if c == '\t' {
                book.resolve(
                    style
                        .font_latin
                        .as_deref()
                        .or(style.font_east_asia.as_deref()),
                    false,
                    style.bold,
                    style.italic,
                )
                .unwrap_or(primary)
            } else {
                primary
            };
            out.push((start..end, font));
            continue;
        }
        let font = if attaches_to_previous(c) {
            out.last().map(|(_, f)| *f).unwrap_or(primary)
        } else if book.face(primary.id).has_glyph(c) {
            primary
        } else {
            sibling
                .get_or_insert_with(|| sibling_font(style, east_asian, book))
                .filter(|s| book.face(s.id).has_glyph(c))
                .or_else(|| book.fallback(c, east_asian, style.bold, style.italic))
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
pub(super) fn is_unpainted(part: &str) -> bool {
    part.starts_with(char::is_control)
}

/// 同一个 run 的另一个字体：西文字体缺 ℃、② 时试中文字体，反过来也一样。
/// 那同样是作者给这段文字选的字体，比系统回退链里的任何字体都更贴近原文 ——
/// 宋体文档里的 ② 不该变成无衬线体。run 只写了一个字体名时，主字体已经是它了。
fn sibling_font(style: &ir::RunStyle, east_asian: bool, book: &mut FontBook) -> Option<Resolved> {
    let (own, other) = if east_asian {
        (&style.font_east_asia, &style.font_latin)
    } else {
        (&style.font_latin, &style.font_east_asia)
    };
    own.as_ref()?;
    book.resolve(other.as_deref(), !east_asian, style.bold, style.italic)
}

/// run 用了 Symbol / Wingdings 这类符号字体、而本机又没有时，把私用区码位换成
/// 意思相同的 Unicode 字符，交给回退字体去画。装了原字体就原样保留。
fn symbol_font_of<'a>(style: &'a ir::RunStyle, book: &FontBook) -> Option<&'a str> {
    [style.font_latin.as_deref(), style.font_east_asia.as_deref()]
        .into_iter()
        .flatten()
        .find(|f| pua::is_symbol_font(f) && !book.has_family(f))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(text: &str) -> ShapedPara {
        ShapedPara {
            text: text.to_string(),
            pieces: Vec::new(),
            breaks: Vec::new(),
            tabs: text.match_indices('\t').map(|(i, _)| i).collect(),
        }
    }

    fn stop(pos: f32, align: ir::TabAlign) -> ir::TabStop {
        ir::TabStop {
            pos,
            align,
            leader: ir::TabLeader::None,
        }
    }

    #[test]
    fn default_stops_count_from_the_margin() {
        let rules = TabRules {
            stops: &[],
            default: 21.0,
            implicit: None,
        };
        assert_eq!(rules.next(12.0).pos, 21.0);
        assert_eq!(rules.next(21.0).pos, 42.0, "正好停在位上时跳到下一个");
        assert_eq!(rules.next(62.0).pos, 63.0);
    }

    #[test]
    fn custom_stops_come_before_default_ones() {
        let stops = [
            stop(50.0, ir::TabAlign::Left),
            stop(200.0, ir::TabAlign::Right),
        ];
        let rules = TabRules {
            stops: &stops,
            default: 21.0,
            implicit: None,
        };
        // 默认位 21、42 在自定义位 50 之前，不用。
        assert_eq!(rules.next(12.0).pos, 50.0);
        assert_eq!(rules.next(60.0).pos, 200.0);
        // 越过最后一个自定义位之后才用默认位。
        assert_eq!(rules.next(210.0).pos, 210.0 + 21.0 - 210.0 % 21.0);
    }

    const SPACES: Hang = Hang {
        spaces: true,
        punct: false,
    };
    const BOTH: Hang = Hang {
        spaces: true,
        punct: true,
    };

    #[test]
    fn trailing_spaces_and_breaks_are_not_measured_when_they_hang() {
        let p = para("ab  cd  \n");
        assert_eq!(p.measured_end(0, 4, SPACES), 2);
        assert_eq!(p.measured_end(0, 9, SPACES), 6);
        assert_eq!(p.measured_end(4, 9, SPACES), 6);
        assert_eq!(p.measured_end(0, 9, Hang::default()), 9);
        // 全角空格不悬挂：它是一个正常的字。
        assert_eq!(para("甲\u{3000}").measured_end(0, 6, BOTH), 6);
    }

    /// 对照 LibreOffice 实测的几组相邻字符。
    #[test]
    fn autospace_goes_between_ideographs_and_alphanumerics_only() {
        use ScriptClass::{EastAsian as E, Latin as L};
        assert!(autospaced(('中', E), ('a', L)));
        assert!(autospaced(('1', L), ('中', E)));
        assert!(autospaced(('中', E), ('α', L)));
        assert!(autospaced(('①', L), ('中', E)));
        assert!(!autospaced(('，', E), ('a', L)));
        assert!(!autospaced(('a', L), ('。', E)));
        assert!(!autospaced(('中', E), ('×', L)));
        assert!(!autospaced(('中', E), ('(', L)));
        assert!(!autospaced(('“', L), ('中', E)));
        assert!(!autospaced(('1', L), ('㎡', E)));
    }

    #[test]
    fn one_sentence_punctuation_mark_hangs() {
        assert_eq!(para("甲乙，").measured_end(0, 9, BOTH), 6);
        assert_eq!(para("ab, ").measured_end(0, 4, BOTH), 2);
        // 只悬挂一个；后引号、后括号不悬挂。
        assert_eq!(para("甲。。").measured_end(0, 9, BOTH), 6);
        assert_eq!(para("甲。”").measured_end(0, 9, BOTH), 9);
        assert_eq!(para("甲，").measured_end(0, 6, SPACES), 6);
    }
}
