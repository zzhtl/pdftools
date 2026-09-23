//! 打开 .docx 包（本质是个 zip）并取出需要的部件。

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use crate::error::{CoreError, Result};

/// 单个部件解压后的大小上限。正常的 document.xml 不会有这么大，
/// 超过就说明要么是异常文件，要么是 zip 炸弹。
const MAX_PART_BYTES: u64 = 64 * 1024 * 1024;
/// 整包解压后的总大小上限。
const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

/// 一条关系。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relationship {
    /// 包内部件的完整路径（`word/media/image1.png`），外部链接则是原文（网址）。
    pub target: String,
    pub external: bool,
}

pub struct Package {
    /// `docProps/core.xml` 里的 `dcterms:created`，文档的创建时间。
    pub created: Option<crate::timestamp::Timestamp>,
    pub document: String,
    pub styles: Option<String>,
    pub numbering: Option<String>,
    pub settings: Option<String>,
    /// `word/theme/theme1.xml`：主题字体在这里。Word、WPS 都固定用这个名字。
    pub theme: Option<String>,
    /// `document.xml` 的关系：id → 目标。
    pub rels: HashMap<String, Relationship>,
    /// 部件路径 → 字节。只收 word/media/ 下的图片。
    pub media: HashMap<String, Vec<u8>>,
}

pub fn open(path: &Path) -> Result<Package> {
    let file = std::fs::File::open(path).map_err(|e| CoreError::io(path, e))?;

    // 老式 .doc 是 CFB 复合文档，不是 zip。单独识别出来给一句有用的话，
    // 而不是让用户看到「不是有效的 zip」。
    {
        use std::io::{Read as _, Seek as _};
        let mut f = &file;
        let mut magic = [0u8; 8];
        if f.read_exact(&mut magic).is_ok()
            && magic == [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]
        {
            return Err(CoreError::Unsupported(
                "这是老式的 .doc 二进制格式（或是加密的文档）。请先用 Word / WPS 另存为 .docx。"
                    .into(),
            ));
        }
        (&mut f).rewind().map_err(|e| CoreError::io(path, e))?;
    }

    let mut zip = zip::ZipArchive::new(file)
        .map_err(|e| CoreError::Docx(format!("无法打开 docx（zip 结构损坏）：{e}")))?;

    let mut total = 0u64;
    let mut document = None;
    let mut styles = None;
    let mut numbering = None;
    let mut settings = None;
    let mut theme = None;
    let mut rels_xml = None;
    let mut core_xml = None;
    let mut media = HashMap::new();

    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| CoreError::Docx(format!("读取 docx 条目失败：{e}")))?;
        if !entry.is_file() {
            continue;
        }
        let name = entry.name().to_string();
        let size = entry.size();
        if size > MAX_PART_BYTES {
            return Err(CoreError::Docx(format!("部件 {name} 过大（{size} 字节）")));
        }
        total += size;
        if total > MAX_TOTAL_BYTES {
            return Err(CoreError::Docx("文档解压后过大，已中止".into()));
        }

        let want_text = matches!(
            name.as_str(),
            "word/document.xml"
                | "word/styles.xml"
                | "word/numbering.xml"
                | "word/settings.xml"
                | "word/theme/theme1.xml"
                | "word/_rels/document.xml.rels"
                | "docProps/core.xml"
        );
        let want_media = name.starts_with("word/media/");
        if !want_text && !want_media {
            continue;
        }

        let mut buf = Vec::with_capacity(size as usize);
        entry
            .read_to_end(&mut buf)
            .map_err(|e| CoreError::Docx(format!("解压 {name} 失败：{e}")))?;

        if want_media {
            media.insert(name, buf);
            continue;
        }
        let text = String::from_utf8(buf)
            .map_err(|_| CoreError::Docx(format!("{name} 不是合法的 UTF-8")))?;
        match name.as_str() {
            "word/document.xml" => document = Some(text),
            "word/styles.xml" => styles = Some(text),
            "word/numbering.xml" => numbering = Some(text),
            "word/settings.xml" => settings = Some(text),
            "word/theme/theme1.xml" => theme = Some(text),
            "docProps/core.xml" => core_xml = Some(text),
            _ => rels_xml = Some(text),
        }
    }

    let document = document.ok_or_else(|| {
        CoreError::Docx("包里没有 word/document.xml，这不是一个有效的 Word 文档".into())
    })?;

    Ok(Package {
        created: core_xml.as_deref().and_then(parse_created),
        document,
        styles,
        numbering,
        settings,
        theme,
        rels: rels_xml.as_deref().map(parse_rels).unwrap_or_default(),
        media,
    })
}

/// 从 `docProps/core.xml` 取 `dcterms:created`，格式是 ISO 8601（`2024-01-15T08:30:00Z`）。
fn parse_created(xml: &str) -> Option<crate::timestamp::Timestamp> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut in_created = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.local_name().as_ref() == "created" => in_created = true,
            Ok(Event::Text(t)) if in_created => {
                return crate::timestamp::Timestamp::parse_iso8601(&t)
            }
            Ok(Event::End(e)) if e.local_name().as_ref() == "created" => in_created = false,
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    None
}

fn parse_rels(xml: &str) -> HashMap<String, Relationship> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut out = HashMap::new();
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(e)) | Ok(Event::Start(e))
                if e.local_name().as_ref() == "Relationship" =>
            {
                let mut id = None;
                let mut target = None;
                let mut external = false;
                for attr in e.attributes().flatten() {
                    let value = attr
                        .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .map(|v| v.into_owned())
                        .unwrap_or_else(|_| attr.value.to_string());
                    match attr.key.local_name().as_ref() {
                        "Id" => id = Some(value),
                        "Target" => target = Some(value),
                        "TargetMode" => external = value == "External",
                        _ => {}
                    }
                }
                if let (Some(id), Some(target)) = (id, target) {
                    // 外部链接（网址）原样保留；包内的 Target 通常是相对 word/ 的路径，
                    // 例如 "media/image1.png"。
                    let target = if external {
                        target
                    } else if target.starts_with("word/") || target.starts_with('/') {
                        target.trim_start_matches('/').to_string()
                    } else {
                        format!("word/{target}")
                    };
                    out.insert(id, Relationship { target, external });
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_targets_are_kept_as_written() {
        let rels = parse_rels(
            r#"<Relationships xmlns="r"><Relationship Id="rId1" Type="t/image" Target="media/a.png"/><Relationship Id="rId2" Type="t/hyperlink" Target="https://example.com/a?b=1&amp;c=2" TargetMode="External"/></Relationships>"#,
        );
        assert_eq!(
            rels["rId1"],
            Relationship {
                target: "word/media/a.png".into(),
                external: false
            }
        );
        assert_eq!(
            rels["rId2"],
            Relationship {
                target: "https://example.com/a?b=1&c=2".into(),
                external: true
            }
        );
    }
}
