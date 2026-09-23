//! 属性解析：`w:rPr`、`w:pPr`、`w:sectPr`。

use quick_xml::events::Event;

use super::{attr, attr_i32, on_off, skip, xml_err, Rd};
use crate::docx::model::{
    Align, DocGrid, LineRule, PPr, RPr, SectPr, TabAlign, TabDef, TabLeader, Underline,
    UnderlineStyle,
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
                "shd" => {
                    rpr.shading = Some(
                        attr(&e, "fill")
                            .as_deref()
                            .filter(|f| !f.eq_ignore_ascii_case("auto"))
                            .and_then(parse_color),
                    )
                }
                "sz" => rpr.size_half_pt = attr_i32(&e, "val").map(|v| v.max(1) as u32),
                "color" => rpr.color = attr(&e, "val").as_deref().and_then(parse_color),
                "rFonts" => {
                    rpr.font_ascii = attr(&e, "ascii").or_else(|| attr(&e, "hAnsi"));
                    rpr.font_east_asia = attr(&e, "eastAsia");
                }
                "spacing" => rpr.spacing = attr_i32(&e, "val"),
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
                // 只标记，不展开 numbering.xml。
                "numPr" => ppr.numbering = true,
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
                "headerReference" | "footerReference" => s.has_header_footer = true,
                "docGrid" => {
                    if let Some(pitch) = attr_i32(&e, "linePitch") {
                        // 只有这三种 type 才吸附。实测 default / 不写 type 都不吸附。
                        let snaps = matches!(
                            attr(&e, "type").as_deref(),
                            Some("lines" | "linesAndChars" | "snapToChars")
                        );
                        s.doc_grid = Some(DocGrid {
                            line_pitch: pitch,
                            snaps,
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
