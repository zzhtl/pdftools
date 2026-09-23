//! 属性解析：`w:rPr`、`w:pPr`、`w:sectPr`。

use quick_xml::events::Event;

use super::{attr, attr_i32, on_off, skip, xml_err, Rd};
use crate::docx::model::{
    Align, Border, BorderStyle, DocGrid, FontRef, LineRule, PPr, RPr, SectPr, SectionStart,
    TabAlign, TabDef, TabLeader, ThemeFont, Underline, UnderlineStyle, VertAlign,
};
use crate::error::Result;

fn parse_color(s: &str) -> Option<[u8; 3]> {
    // "auto" 表示由阅读器决定，按黑色处理。
    if s.eq_ignore_ascii_case("auto") {
        return Some([0, 0, 0]);
    }
    let s = s.trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    Some([
        u8::from_str_radix(&s[0..2], 16).ok()?,
        u8::from_str_radix(&s[2..4], 16).ok()?,
        u8::from_str_radix(&s[4..6], 16).ok()?,
    ])
}

/// `w:shd` 实际的填充色。`w:val` 是图案：`clear` 只有底色（`w:fill`），`solid`
/// 全是前景色（`w:color`），`pctN` 是前景色按 N% 盖在底色上；其余图案按底色画。
/// 没有颜色（auto、`nil`）时返回 None。
fn parse_shd(e: &quick_xml::events::BytesStart) -> Option<[u8; 3]> {
    let val = attr(e, "val").unwrap_or_default();
    let color = |name| {
        attr(e, name)
            .filter(|c| !c.eq_ignore_ascii_case("auto"))
            .and_then(|c| parse_color(&c))
    };
    let fill = color("fill");
    if val == "nil" {
        return None;
    }
    if val == "solid" {
        return Some(color("color").unwrap_or([0, 0, 0]));
    }
    if let Some(pct) = val.strip_prefix("pct").and_then(|p| p.parse::<u32>().ok()) {
        let (fg, bg) = (
            color("color").unwrap_or([0, 0, 0]),
            fill.unwrap_or([0xFF, 0xFF, 0xFF]),
        );
        let t = pct.min(100) as f32 / 100.0;
        return Some(std::array::from_fn(|i| {
            (bg[i] as f32 * (1.0 - t) + fg[i] as f32 * t).round() as u8
        }));
    }
    fill
}

/// `w:pBdr` 里的一条边。多线样式（三线、粗细线）近似成双线，其余线型近似成单线。
fn parse_border(e: &quick_xml::events::BytesStart) -> Border {
    let style = match attr(e, "val").as_deref() {
        None | Some("nil") | Some("none") => BorderStyle::None,
        Some("dotted") => BorderStyle::Dotted,
        Some("dashed" | "dashSmallGap" | "dotDash" | "dotDotDash" | "dashDotStroked") => {
            BorderStyle::Dashed
        }
        Some(v)
            if v == "double"
                || v == "triple"
                || v.starts_with("thinThick")
                || v.starts_with("thickThin") =>
        {
            BorderStyle::Double
        }
        _ => BorderStyle::Single,
    };
    Border {
        style,
        // 没写线宽时按 Word 的缺省 1/2 磅。
        size_eighths: attr_i32(e, "sz").unwrap_or(4).max(0),
        space_pt: attr_i32(e, "space").unwrap_or(0).max(0),
        color: attr(e, "color")
            .filter(|c| !c.eq_ignore_ascii_case("auto"))
            .and_then(|c| parse_color(&c)),
    }
}

fn parse_underline(e: &quick_xml::events::BytesStart) -> Underline {
    use UnderlineStyle as U;
    let style = match attr(e, "val").as_deref() {
        None | Some("single") => U::Single,
        Some("none") => U::None,
        Some("words") => U::Words,
        Some("double") => U::Double,
        Some("thick") => U::Thick,
        Some("dotted") => U::Dotted,
        Some("dottedHeavy") => U::DottedHeavy,
        Some("dash") => U::Dash,
        Some("dashedHeavy") => U::DashedHeavy,
        Some("dashLong") => U::DashLong,
        Some("dashLongHeavy") => U::DashLongHeavy,
        Some("dotDash") => U::DotDash,
        Some("dashDotHeavy") => U::DashDotHeavy,
        Some("dotDotDash") => U::DotDotDash,
        Some("dashDotDotHeavy") => U::DashDotDotHeavy,
        Some("wave") => U::Wave,
        Some("wavyHeavy") => U::WavyHeavy,
        Some("wavyDouble") => U::WavyDouble,
        Some(_) => U::Single,
    };
    Underline {
        style,
        color: attr(e, "color")
            .as_deref()
            .filter(|c| !c.eq_ignore_ascii_case("auto"))
            .and_then(parse_color),
    }
}

/// `ST_HighlightColor` 的颜色名。
fn highlight(name: &str) -> Option<[u8; 3]> {
    Some(match name {
        "black" => [0x00, 0x00, 0x00],
        "blue" => [0x00, 0x00, 0xFF],
        "cyan" => [0x00, 0xFF, 0xFF],
        "green" => [0x00, 0xFF, 0x00],
        "magenta" => [0xFF, 0x00, 0xFF],
        "red" => [0xFF, 0x00, 0x00],
        "yellow" => [0xFF, 0xFF, 0x00],
        "white" => [0xFF, 0xFF, 0xFF],
        "darkBlue" => [0x00, 0x00, 0x80],
        "darkCyan" => [0x00, 0x80, 0x80],
        "darkGreen" => [0x00, 0x80, 0x00],
        "darkMagenta" => [0x80, 0x00, 0x80],
        "darkRed" => [0x80, 0x00, 0x00],
        "darkYellow" => [0x80, 0x80, 0x00],
        "darkGray" => [0x80, 0x80, 0x80],
        "lightGray" => [0xC0, 0xC0, 0xC0],
        _ => return None,
    })
}

pub(super) fn parse_rpr(r: &mut Rd) -> Result<RPr> {
    let mut rpr = RPr::default();
    loop {
        match r.read_event().map_err(xml_err)? {
            // 修订前的旧格式。当成当前格式读，就会把改过的字号、加粗又改回去。
            Event::Start(e) if e.local_name().as_ref() == "rPrChange" => skip(r, "rPrChange")?,
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                "rStyle" => rpr.style_id = attr(&e, "val"),
                "b" => rpr.bold = Some(on_off(&e)),
                "i" => rpr.italic = Some(on_off(&e)),
                "u" => rpr.underline = Some(parse_underline(&e)),
                "strike" => rpr.strike = Some(on_off(&e)),
                "dstrike" => rpr.double_strike = Some(on_off(&e)),
                "highlight" => rpr.highlight = Some(attr(&e, "val").as_deref().and_then(highlight)),
                "shd" => rpr.shading = Some(parse_shd(&e)),
                "sz" => rpr.size_half_pt = attr_i32(&e, "val").map(|v| v.max(1) as u32),
                "color" => rpr.color = attr(&e, "val").as_deref().and_then(parse_color),
                "rFonts" => {
                    let theme = |name| {
                        attr(&e, name)
                            .and_then(|v| ThemeFont::parse(&v))
                            .map(FontRef::Theme)
                    };
                    let named = |name| attr(&e, name).map(FontRef::Name);
                    rpr.font_ascii = theme("asciiTheme")
                        .or_else(|| named("ascii"))
                        .or_else(|| theme("hAnsiTheme"))
                        .or_else(|| named("hAnsi"));
                    rpr.font_east_asia = theme("eastAsiaTheme").or_else(|| named("eastAsia"));
                    rpr.legacy_font_ascii = attr(&e, "ascii").or_else(|| attr(&e, "hAnsi"));
                    rpr.legacy_font_east_asia = attr(&e, "eastAsia");
                    rpr.hint_east_asia = attr(&e, "hint").map(|h| h == "eastAsia");
                }
                "spacing" => rpr.spacing = attr_i32(&e, "val"),
                "vertAlign" => {
                    rpr.vert_align = match attr(&e, "val").as_deref() {
                        Some("superscript") => Some(VertAlign::Superscript),
                        Some("subscript") => Some(VertAlign::Subscript),
                        Some("baseline") => Some(VertAlign::Baseline),
                        _ => None,
                    }
                }
                "position" => rpr.position = attr_i32(&e, "val"),
                "caps" => rpr.caps = Some(on_off(&e)),
                "smallCaps" => rpr.small_caps = Some(on_off(&e)),
                "kern" => rpr.kern = attr_i32(&e, "val").map(|v| v.max(0) as u32),
                "vanish" => rpr.vanish = Some(on_off(&e)),
                _ => {}
            },
            Event::End(e) if e.local_name().as_ref() == "rPr" => break,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(rpr)
}

/// 返回段落属性，以及其中的 `w:sectPr`（本段是一节的最后一段时才有）。
pub(super) fn parse_ppr(r: &mut Rd) -> Result<(PPr, Option<SectPr>)> {
    let mut ppr = PPr::default();
    let mut section = None;
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) if e.local_name().as_ref() == "rPr" => ppr.mark_rpr = parse_rpr(r)?,
            Event::Start(e) if e.local_name().as_ref() == "sectPr" => {
                section = Some(parse_sect_pr(r)?)
            }
            // 修订前的旧格式，理由同 rPrChange。
            Event::Start(e) if e.local_name().as_ref() == "pPrChange" => skip(r, "pPrChange")?,
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                "numPr" => ppr.numbering = true,
                // `w:numPr` 的两个子元素，在 `w:pPr` 里不会出现在别处。
                "numId" => ppr.num_id = attr_i32(&e, "val"),
                "ilvl" => ppr.num_ilvl = attr_i32(&e, "val"),
                "pStyle" => ppr.style_id = attr(&e, "val"),
                "jc" => {
                    ppr.align = match attr(&e, "val").as_deref() {
                        Some("center") => Some(Align::Center),
                        Some("right") | Some("end") => Some(Align::Right),
                        Some("both") => Some(Align::Both),
                        Some("distribute") => Some(Align::Distribute),
                        Some("left") | Some("start") => Some(Align::Left),
                        _ => None,
                    }
                }
                "ind" => {
                    let i = &mut ppr.indent;
                    i.left_twips = attr_i32(&e, "left").or_else(|| attr_i32(&e, "start"));
                    i.right_twips = attr_i32(&e, "right").or_else(|| attr_i32(&e, "end"));
                    i.first_line_twips = attr_i32(&e, "firstLine");
                    i.hanging_twips = attr_i32(&e, "hanging");
                    i.left_chars = attr_i32(&e, "leftChars").or_else(|| attr_i32(&e, "startChars"));
                    i.first_line_chars = attr_i32(&e, "firstLineChars");
                    i.hanging_chars = attr_i32(&e, "hangingChars");
                }
                "spacing" => {
                    ppr.space_before_twips = attr_i32(&e, "before");
                    ppr.space_after_twips = attr_i32(&e, "after");
                    ppr.line = attr_i32(&e, "line");
                    ppr.line_rule = match attr(&e, "lineRule").as_deref() {
                        Some("exact") => Some(LineRule::Exact),
                        Some("atLeast") => Some(LineRule::AtLeast),
                        // 缺省就是 auto。这条很关键：auto 时 w:line 是 240 分之一行的倍数，
                        // 当成 twips 处理的话行距会差一个数量级。
                        _ => Some(LineRule::Auto),
                    };
                }
                "pageBreakBefore" => ppr.page_break_before = Some(on_off(&e)),
                "snapToGrid" => ppr.snap_to_grid = Some(on_off(&e)),
                "autoSpaceDE" => ppr.auto_space_latin = Some(on_off(&e)),
                "autoSpaceDN" => ppr.auto_space_digits = Some(on_off(&e)),
                "overflowPunct" => ppr.overflow_punct = Some(on_off(&e)),
                "keepNext" => ppr.keep_next = Some(on_off(&e)),
                "keepLines" => ppr.keep_lines = Some(on_off(&e)),
                "widowControl" => ppr.widow_control = Some(on_off(&e)),
                "contextualSpacing" => ppr.contextual_spacing = Some(on_off(&e)),
                "shd" => ppr.shading = Some(parse_shd(&e)),
                // `w:pBdr` 的各条边。这些名字在 `w:pPr` 里只会出现在 `w:pBdr` 下面。
                "top" => ppr.borders.top = Some(parse_border(&e)),
                "left" | "start" => ppr.borders.left = Some(parse_border(&e)),
                "bottom" => ppr.borders.bottom = Some(parse_border(&e)),
                "right" | "end" => ppr.borders.right = Some(parse_border(&e)),
                "between" => ppr.borders.between = Some(parse_border(&e)),
                // 段落属性里的 `w:tab` 只会出现在 `w:tabs` 里。
                "tab" => {
                    if let Some(t) = parse_tab(&e) {
                        ppr.tabs.push(t);
                    }
                }
                _ => {}
            },
            Event::End(e) if e.local_name().as_ref() == "pPr" => break,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok((ppr, section))
}

fn parse_tab(e: &quick_xml::events::BytesStart) -> Option<TabDef> {
    let align = match attr(e, "val")?.as_str() {
        "left" | "start" | "num" => TabAlign::Left,
        "center" => TabAlign::Center,
        "right" | "end" => TabAlign::Right,
        "decimal" => TabAlign::Decimal,
        "bar" => TabAlign::Bar,
        "clear" => TabAlign::Clear,
        _ => return None,
    };
    let leader = match attr(e, "leader").as_deref() {
        Some("dot") => TabLeader::Dot,
        Some("hyphen") => TabLeader::Hyphen,
        Some("underscore") => TabLeader::Underscore,
        Some("middleDot") => TabLeader::MiddleDot,
        Some("heavy") => TabLeader::Heavy,
        _ => TabLeader::None,
    };
    Some(TabDef {
        align,
        leader,
        pos: attr_i32(e, "pos")?,
    })
}

pub(super) fn parse_sect_pr(r: &mut Rd) -> Result<SectPr> {
    let mut s = SectPr::default();
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) if e.local_name().as_ref() == "sectPrChange" => {
                skip(r, "sectPrChange")?
            }
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                "pgSz" => {
                    if let Some(w) = attr_i32(&e, "w") {
                        s.page_w = w;
                    }
                    if let Some(h) = attr_i32(&e, "h") {
                        s.page_h = h;
                    }
                }
                kind @ ("headerReference" | "footerReference") => {
                    s.has_header_footer = true;
                    let refs = if kind == "headerReference" {
                        &mut s.headers
                    } else {
                        &mut s.footers
                    };
                    let id = attr(&e, "id");
                    match attr(&e, "type").as_deref() {
                        Some("first") => refs.first = id,
                        Some("even") => refs.even = id,
                        _ => refs.default = id,
                    }
                }
                "type" => {
                    s.start = match attr(&e, "val").as_deref() {
                        Some("continuous") => SectionStart::Continuous,
                        Some("evenPage") => SectionStart::EvenPage,
                        Some("oddPage") => SectionStart::OddPage,
                        Some("nextColumn") => SectionStart::NextColumn,
                        _ => SectionStart::NextPage,
                    }
                }
                "titlePg" => s.title_page = on_off(&e),
                "pgNumType" => {
                    s.page_number_start = attr_i32(&e, "start");
                    s.page_number_format = attr(&e, "fmt");
                }
                "docGrid" => {
                    if let Some(pitch) = attr_i32(&e, "linePitch") {
                        // 只有这三种 type 才吸附。实测 default / 不写 type 都不吸附。
                        let snaps = matches!(
                            attr(&e, "type").as_deref(),
                            Some("lines" | "linesAndChars" | "snapToChars")
                        );
                        let chars = matches!(
                            attr(&e, "type").as_deref(),
                            Some("linesAndChars" | "snapToChars")
                        );
                        s.doc_grid = Some(DocGrid {
                            line_pitch: pitch,
                            snaps,
                            chars,
                            char_space: attr_i32(&e, "charSpace"),
                        });
                    }
                }
                "pgMar" => {
                    if let Some(v) = attr_i32(&e, "top") {
                        s.margin_top = v;
                    }
                    if let Some(v) = attr_i32(&e, "bottom") {
                        s.margin_bottom = v;
                    }
                    if let Some(v) = attr_i32(&e, "left") {
                        s.margin_left = v;
                    }
                    if let Some(v) = attr_i32(&e, "right") {
                        s.margin_right = v;
                    }
                    if let Some(v) = attr_i32(&e, "header") {
                        s.header_dist = v;
                    }
                    if let Some(v) = attr_i32(&e, "footer") {
                        s.footer_dist = v;
                    }
                }
                _ => {}
            },
            Event::End(e) if e.local_name().as_ref() == "sectPr" => break,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(s)
}

pub(super) fn parse_ppr_default(r: &mut Rd) -> Result<PPr> {
    let mut out = PPr::default();
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) if e.local_name().as_ref() == "pPr" => out = parse_ppr(r)?.0,
            Event::End(e) if e.local_name().as_ref() == "pPrDefault" => break,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

pub(super) fn parse_rpr_default(r: &mut Rd) -> Result<RPr> {
    let mut out = RPr::default();
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) if e.local_name().as_ref() == "rPr" => out = parse_rpr(r)?,
            Event::End(e) if e.local_name().as_ref() == "rPrDefault" => break,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}
