//! `document.xml` / `styles.xml` 的拉取式解析，产出 [`model`](super::model)。
//!
//! 元素一律按 local name 匹配，不看命名空间前缀：同一个元素在不同生成器里
//! 前缀不同（`w:`、`w14:`、默认命名空间），按前缀匹配会漏。

mod numbering;
mod props;
mod story;

use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

pub use numbering::parse_numbering;

use super::model::{Document, SectPr, Settings, Style, Styles, Theme};
use crate::error::{CoreError, Result};

pub(crate) type Rd<'a> = Reader<&'a [u8]>;

/// 取属性值（按 local name 匹配，忽略命名空间前缀），实体已还原。
pub(crate) fn attr(e: &BytesStart, name: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == name)
        .map(|a| {
            a.normalized_value(XmlVersion::Implicit1_0)
                .map(|v| v.into_owned())
                .unwrap_or_else(|_| a.value.to_string())
        })
}

pub(crate) fn attr_i32(e: &BytesStart, name: &str) -> Option<i32> {
    attr(e, name)?.trim().parse().ok()
}

/// OOXML 的布尔属性：`w:val` 缺省即为 true，"0"/"false"/"off" 为 false。
pub(crate) fn on_off(e: &BytesStart) -> bool {
    match attr(e, "val").as_deref() {
        None => true,
        Some(v) => !matches!(v, "0" | "false" | "off"),
    }
}

/// 解析 XML 实体引用。quick-xml 把它们作为独立事件抛出来，
/// 不处理的话文档里的 `&`、`<` 会直接丢掉。
pub(crate) fn resolve_entity(name: &str) -> Option<char> {
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

pub(crate) fn xml_err(e: quick_xml::Error) -> CoreError {
    CoreError::Docx(format!("XML 解析失败：{e}"))
}

/// 跳过当前元素的整棵子树（调用时刚读过它的 Start 事件）。
pub(crate) fn skip(r: &mut Rd, name: &str) -> Result<()> {
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

pub fn parse_document(xml: &str, styles: Styles, settings: Settings) -> Result<Document> {
    let mut r = Reader::from_str(xml);
    let mut body = Vec::new();
    let mut section = SectPr::default();
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) if e.local_name().as_ref() == "body" => {
                let (story, last) = story::parse_body(&mut r)?;
                body = story;
                if let Some(s) = last {
                    section = s;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(Document {
        body,
        section,
        styles,
        settings,
        theme: Default::default(),
        numbering: Default::default(),
        header_footer: Default::default(),
        hyperlinks: Default::default(),
    })
}

/// 解析 `settings.xml`。坏了不影响转换：读不到的开关按缺省处理。
pub fn parse_settings(xml: &str) -> Settings {
    let mut r = Reader::from_str(xml);
    let mut settings = Settings::default();
    while let Ok(ev) = r.read_event() {
        match ev {
            Event::Start(e) | Event::Empty(e)
                if e.local_name().as_ref() == "doNotUseHTMLParagraphAutoSpacing" =>
            {
                settings.no_html_paragraph_spacing = on_off(&e);
            }
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "defaultTabStop" => {
                settings.default_tab_stop = attr_i32(&e, "val").filter(|v| *v > 0);
            }
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "evenAndOddHeaders" => {
                settings.even_and_odd_headers = on_off(&e);
            }
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "themeFontLang" => {
                settings.theme_font_lang_east_asia = attr(&e, "eastAsia").filter(|v| !v.is_empty());
            }
            Event::Eof => break,
            _ => {}
        }
    }
    settings
}

/// 解析页眉或页脚部件。坏了不影响正文：读不出来就当没有。
pub fn parse_header_footer(xml: &str) -> super::model::Story {
    let mut r = Reader::from_str(xml);
    story::parse_part(&mut r).unwrap_or_default()
}

/// 解析主题部件里的字体方案（`a:fontScheme`）。颜色、效果这些与排版无关，不读。
pub fn parse_theme(xml: &str) -> Theme {
    let mut r = Reader::from_str(xml);
    let mut theme = Theme::default();
    // 当前在 majorFont（true）还是 minorFont（false）里。
    let mut major = None;
    while let Ok(ev) = r.read_event() {
        let (e, empty) = match &ev {
            Event::Start(e) => (e, false),
            Event::Empty(e) => (e, true),
            Event::End(e) => {
                if matches!(e.local_name().as_ref(), "majorFont" | "minorFont") {
                    major = None;
                }
                continue;
            }
            Event::Eof => break,
            _ => continue,
        };
        let name = e.local_name();
        match name.as_ref() {
            // 自闭合的 `<a:majorFont/>` 里没有字体，也没有结束标签可以把状态收回来。
            "majorFont" | "minorFont" if !empty => major = Some(name.as_ref() == "majorFont"),
            local => {
                let Some(major) = major else { continue };
                let fonts = if major {
                    &mut theme.major
                } else {
                    &mut theme.minor
                };
                let typeface = attr(e, "typeface").filter(|t| !t.is_empty());
                match local {
                    "latin" => fonts.latin = typeface,
                    "ea" => fonts.east_asia = typeface,
                    "cs" => fonts.complex_script = typeface,
                    "font" => {
                        if let (Some(script), Some(t)) = (attr(e, "script"), typeface) {
                            fonts.by_script.insert(script, t);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    theme
}

/// 解析 `styles.xml`：文档默认值 + 段落/字符样式定义。
///
/// 这里只把样式**存起来**，`basedOn` 链的展开留给 `resolve`。
/// 表格样式与编号样式也标着 `w:default="1"`，它们不能被当成默认段落样式。
pub fn parse_styles(xml: &str) -> Styles {
    let mut r = Reader::from_str(xml);
    let mut styles = Styles::default();
    // styles.xml 坏了不该让整份文档转换失败：能读到多少用多少。
    while let Ok(ev) = r.read_event() {
        match ev {
            Event::Start(e) => match e.local_name().as_ref() {
                "pPrDefault" => {
                    if let Ok(p) = props::parse_ppr_default(&mut r) {
                        styles.doc_default_ppr = p;
                    }
                }
                "rPrDefault" => {
                    if let Ok(rp) = props::parse_rpr_default(&mut r) {
                        styles.doc_default_rpr = rp;
                    }
                }
                "style" => {
                    let kind = attr(&e, "type").unwrap_or_else(|| "paragraph".into());
                    let id = attr(&e, "styleId").unwrap_or_default();
                    let is_default = attr(&e, "default").is_some_and(|v| v == "1" || v == "true");
                    let Ok(mut st) = parse_style_body(&mut r) else {
                        continue;
                    };
                    st.id = id.clone();
                    match kind.as_str() {
                        "paragraph" => {
                            if is_default && styles.default_paragraph_style.is_none() {
                                styles.default_paragraph_style = Some(id.clone());
                            }
                            styles.paragraph.insert(id, st);
                        }
                        "character" => {
                            styles.character.insert(id, st);
                        }
                        "numbering" => {
                            styles.numbering.insert(id, st);
                        }
                        // 表格样式：还不参与层叠。
                        _ => {}
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

fn parse_style_body(r: &mut Rd) -> Result<Style> {
    let mut st = Style::default();
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) => match e.local_name().as_ref() {
                "pPr" => st.ppr = props::parse_ppr(r)?.0,
                "rPr" => st.rpr = props::parse_rpr(r)?,
                // 表格样式的条件格式（首行、奇偶行……）里也有 pPr/rPr，
                // 不能让它们覆盖样式本身的格式。
                "tblStylePr" => skip(r, "tblStylePr")?,
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

#[cfg(test)]
mod tests;
