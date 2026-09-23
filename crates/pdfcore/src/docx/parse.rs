//! `document.xml` / `styles.xml` 的拉取式解析。

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use super::model::*;
use crate::error::{CoreError, Result};

type Rd<'a> = Reader<&'a [u8]>;

/// 取属性值（按 local name 匹配，忽略命名空间前缀）。
fn attr(e: &BytesStart, name: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == name)
        .map(|a| a.value.to_string())
}

fn attr_i32(e: &BytesStart, name: &str) -> Option<i32> {
    attr(e, name)?.trim().parse().ok()
}

/// OOXML 的布尔属性：`w:val` 缺省即为 true，"0"/"false"/"off" 为 false。
fn on_off(e: &BytesStart) -> bool {
    match attr(e, "val").as_deref() {
        None => true,
        Some(v) => !matches!(v, "0" | "false" | "off"),
    }
}

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

/// 解析 XML 实体引用。quick-xml 0.42 把它们作为独立事件抛出来，
/// 不处理的话文档里的 `&`、`<` 会直接丢掉。
fn resolve_entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            let hex = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X"));
            let code = match hex {
                Some(h) => u32::from_str_radix(h, 16).ok()?,
                None => name.strip_prefix('#')?.parse().ok()?,
            };
            char::from_u32(code)
        }
    }
}

pub fn parse_document(xml: &str, styles: Styles) -> Result<RawDocument> {
    let mut reader = Reader::from_str(xml);
    let mut blocks = Vec::new();
    let mut section = SectPr::default();

    loop {
        match reader
            .read_event()
            .map_err(|e| CoreError::Docx(format!("XML 解析失败：{e}")))?
        {
            Event::Start(e) => match e.local_name().as_ref() {
                "p" => blocks.push(RawBlock::Para(parse_paragraph(&mut reader)?)),
                "tbl" => blocks.push(parse_table(&mut reader)?),
                "sectPr" => section = parse_sect_pr(&mut reader)?,
                // sdt（内容控件）本身不渲染，但它包裹的内容要照常处理，
                // 所以这里什么都不做，让内部的 w:p 被正常遇到。
                "sdt" | "sdtContent" | "body" | "document" => {}
                // mc:AlternateContent：取 Fallback 分支，丢掉 Choice 分支。
                "AlternateContent" => {
                    if let Some(b) = parse_alternate_content(&mut reader)? {
                        blocks.extend(b);
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }

    Ok(RawDocument {
        blocks,
        section,
        styles,
    })
}

/// `mc:AlternateContent` 里 Choice 是新版特性（通常是 DrawingML 形状），
/// Fallback 是给老版 Word 的降级表示。我们取 Fallback。
fn parse_alternate_content(r: &mut Rd) -> Result<Option<Vec<RawBlock>>> {
    let mut depth = 1usize;
    let mut out = Vec::new();
    let mut in_fallback = false;
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) => {
                match e.local_name().as_ref() {
                    "AlternateContent" => depth += 1,
                    "Fallback" => in_fallback = true,
                    "p" if in_fallback => {
                        out.push(RawBlock::Para(parse_paragraph(r)?));
                        continue;
                    }
                    _ => {}
                }
                if !in_fallback {
                    // Choice 分支整段跳过
                    continue;
                }
            }
            Event::End(e) => match e.local_name().as_ref() {
                "AlternateContent" => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                "Fallback" => in_fallback = false,
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Ok((!out.is_empty()).then_some(out))
}

fn xml_err(e: quick_xml::Error) -> CoreError {
    CoreError::Docx(format!("XML 解析失败：{e}"))
}

fn parse_paragraph(r: &mut Rd) -> Result<RawPara> {
    let mut para = RawPara::default();
    let mut depth = 1usize;

    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) => match e.local_name().as_ref() {
                "p" => depth += 1,
                "pPr" => para.ppr = parse_ppr(r)?,
                "r" => {
                    if let Some(run) = parse_run(r, &mut para.unsupported)? {
                        para.runs.push(run);
                    }
                }
                // 超链接不单独建模，把里面的 run 当普通 run 处理。
                "hyperlink" | "smartTag" | "ins" => {}
                // 删除的修订内容不应出现在输出里。
                "del" => skip_subtree(r, "del")?,
                _ => {}
            },
            Event::End(e) if e.local_name().as_ref() == "p" => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(para)
}

fn parse_run(r: &mut Rd, unsupported: &mut Vec<UnsupportedKind>) -> Result<Option<RawRun>> {
    let mut run = RawRun {
        rpr: RPr::default(),
        text: String::new(),
    };
    let mut preserve_space = false;

    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) => match e.local_name().as_ref() {
                "rPr" => run.rpr = parse_rpr(r)?,
                "t" => {
                    preserve_space = attr(&e, "space").as_deref() == Some("preserve");
                }
                "drawing" | "pict" | "object" => {
                    let name = e.local_name().as_ref().to_string();
                    // 记下来再跳过。静默丢弃是不允许的：用户会以为图片转过去了。
                    unsupported.push(UnsupportedKind::Drawing {
                        alt: find_alt_text(r, &name)?,
                    });
                }
                "oMath" | "oMathPara" => {
                    let name = e.local_name().as_ref().to_string();
                    unsupported.push(UnsupportedKind::Math);
                    skip_subtree(r, &name)?;
                }
                _ => {}
            },
            Event::Empty(e) => match e.local_name().as_ref() {
                "tab" => run.text.push('\t'),
                // w:br 默认是换行；type="page" 是分页符，在排版层处理，
                // 这里统一记成换行，分页由段落属性与显式标记驱动。
                "br" => run.text.push('\n'),
                "cr" => run.text.push('\n'),
                "noBreakHyphen" => run.text.push('\u{2011}'),
                _ => {}
            },
            Event::Text(t) => {
                let s: &str = &t;
                // 没有 xml:space="preserve" 时，OOXML 规定忽略 w:t 首尾的空白。
                // 不遵守的话，格式化过的 XML（元素之间有缩进换行）会在正文里
                // 凭空多出一堆空格。
                if preserve_space {
                    run.text.push_str(s);
                } else {
                    run.text
                        .push_str(s.trim_matches(|c: char| c.is_whitespace()));
                }
            }
            Event::GeneralRef(rf) => {
                if let Some(c) = resolve_entity(&rf) {
                    run.text.push(c);
                }
            }
            Event::End(e) if e.local_name().as_ref() == "r" => break,
            Event::End(e) if e.local_name().as_ref() == "t" => preserve_space = false,
            Event::Eof => break,
            _ => {}
        }
    }

    Ok((!run.text.is_empty()).then_some(run))
}

fn parse_rpr(r: &mut Rd) -> Result<RPr> {
    let mut rpr = RPr::default();
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                "rStyle" => rpr.style_id = attr(&e, "val"),
                "b" => rpr.bold = Some(on_off(&e)),
                "i" => rpr.italic = Some(on_off(&e)),
                "u" => {
                    rpr.underline = Some(attr(&e, "val").as_deref() != Some("none"));
                }
                "strike" => rpr.strike = Some(on_off(&e)),
                "sz" => rpr.size_half_pt = attr_i32(&e, "val").map(|v| v.max(1) as u32),
                "color" => rpr.color = attr(&e, "val").as_deref().and_then(parse_color),
                "rFonts" => {
                    rpr.font_ascii = attr(&e, "ascii").or_else(|| attr(&e, "hAnsi"));
                    rpr.font_east_asia = attr(&e, "eastAsia");
                }
                _ => {}
            },
            Event::End(e) if e.local_name().as_ref() == "rPr" => break,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(rpr)
}

fn parse_ppr(r: &mut Rd) -> Result<PPr> {
    let mut ppr = PPr::default();
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) if e.local_name().as_ref() == "rPr" => {
                ppr.mark_rpr = parse_rpr(r)?;
            }
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                "pStyle" => ppr.style_id = attr(&e, "val"),
                // 只标记，不展开 numbering.xml —— 本版本不生成编号文字。
                "numPr" => ppr.numbering = true,
                "jc" => {
                    ppr.align = match attr(&e, "val").as_deref() {
                        Some("center") => Some(Align::Center),
                        Some("right") | Some("end") => Some(Align::Right),
                        Some("both") | Some("distribute") => Some(Align::Justify),
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
                _ => {}
            },
            Event::End(e) if e.local_name().as_ref() == "pPr" => break,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(ppr)
}

fn parse_sect_pr(r: &mut Rd) -> Result<SectPr> {
    let mut s = SectPr::default();
    loop {
        match r.read_event().map_err(xml_err)? {
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

/// 表格本版本不渲染，但要把每个单元格的文字抽出来 ——
/// 法律文书里表格中的内容往往是最重要的部分，画不出格子也不能把字丢了。
fn parse_table(r: &mut Rd) -> Result<RawBlock> {
    let mut depth = 1usize;
    let mut rows = 0usize;
    let mut max_cols = 0usize;
    let mut cols_in_row = 0usize;
    let mut texts: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut in_cell = false;

    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) => match e.local_name().as_ref() {
                "tbl" => depth += 1,
                "tr" => {
                    rows += 1;
                    cols_in_row = 0;
                }
                "tc" => {
                    cols_in_row += 1;
                    max_cols = max_cols.max(cols_in_row);
                    in_cell = true;
                    cell.clear();
                }
                _ => {}
            },
            Event::Text(t) if in_cell => cell.push_str(&t),
            Event::GeneralRef(rf) if in_cell => {
                if let Some(c) = resolve_entity(&rf) {
                    cell.push(c);
                }
            }
            Event::End(e) => match e.local_name().as_ref() {
                "tc" => {
                    in_cell = false;
                    let trimmed = cell.trim();
                    if !trimmed.is_empty() {
                        texts.push(trimmed.to_string());
                    }
                }
                "tbl" => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }

    Ok(RawBlock::Unsupported {
        kind: UnsupportedKind::Table {
            rows,
            cols: max_cols,
        },
        text: texts,
    })
}

/// 跳过一棵图片子树，顺便把 `wp:docPr/@descr`（替代文字）捞出来。
/// 有替代文字的话，占位提示就能说清楚「这里原本是什么图」。
fn find_alt_text(r: &mut Rd, name: &str) -> Result<Option<String>> {
    let mut depth = 1usize;
    let mut alt = None;
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) => {
                if e.local_name().as_ref() == "docPr" {
                    alt = alt.or_else(|| attr(&e, "descr").filter(|s| !s.trim().is_empty()));
                }
                if e.local_name().as_ref() == name {
                    depth += 1;
                }
            }
            Event::Empty(e) => {
                if e.local_name().as_ref() == "docPr" {
                    alt = alt.or_else(|| attr(&e, "descr").filter(|s| !s.trim().is_empty()));
                }
            }
            Event::End(e) if e.local_name().as_ref() == name => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(alt)
}

fn skip_subtree(r: &mut Rd, name: &str) -> Result<()> {
    let mut depth = 1usize;
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) if e.local_name().as_ref() == name => depth += 1,
            Event::End(e) if e.local_name().as_ref() == name => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(())
}

/// 解析 `styles.xml`：文档默认值 + 段落/字符样式定义。
///
/// 这里只把样式**存起来**，`basedOn` 链的展开留给 `style.rs`。
pub fn parse_styles(xml: &str) -> Styles {
    let mut reader = Reader::from_str(xml);
    let mut styles = Styles::default();

    while let Ok(ev) = reader.read_event() {
        match ev {
            Event::Start(e) => match e.local_name().as_ref() {
                "pPrDefault" => {
                    if let Ok(p) = parse_ppr_default(&mut reader, "pPrDefault") {
                        styles.doc_default_ppr = p;
                    }
                }
                "rPrDefault" => {
                    if let Ok(rp) = parse_rpr_default(&mut reader, "rPrDefault") {
                        styles.doc_default_rpr = rp;
                    }
                }
                "style" => {
                    let kind = attr(&e, "type").unwrap_or_default();
                    let id = attr(&e, "styleId").unwrap_or_default();
                    let is_default = attr(&e, "default").as_deref() == Some("1");
                    if let Ok(mut st) = parse_style_body(&mut reader) {
                        st.id = id.clone();
                        st.is_default = is_default;
                        match kind.as_str() {
                            "character" => {
                                styles.character.insert(id, st);
                            }
                            _ => {
                                if is_default && styles.default_paragraph_style.is_none() {
                                    styles.default_paragraph_style = Some(id.clone());
                                }
                                styles.paragraph.insert(id, st);
                            }
                        }
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    styles
}

fn parse_ppr_default(r: &mut Rd, end: &str) -> Result<PPr> {
    let mut out = PPr::default();
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) if e.local_name().as_ref() == "pPr" => out = parse_ppr(r)?,
            Event::End(e) if e.local_name().as_ref() == end => break,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

fn parse_rpr_default(r: &mut Rd, end: &str) -> Result<RPr> {
    let mut out = RPr::default();
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) if e.local_name().as_ref() == "rPr" => out = parse_rpr(r)?,
            Event::End(e) if e.local_name().as_ref() == end => break,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

fn parse_style_body(r: &mut Rd) -> Result<Style> {
    let mut st = Style::default();
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) => match e.local_name().as_ref() {
                "pPr" => st.ppr = parse_ppr(r)?,
                "rPr" => st.rpr = parse_rpr(r)?,
                _ => {}
            },
            Event::Empty(e) if e.local_name().as_ref() == "basedOn" => {
                st.based_on = attr(&e, "val");
            }
            Event::End(e) if e.local_name().as_ref() == "style" => break,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(st)
}
