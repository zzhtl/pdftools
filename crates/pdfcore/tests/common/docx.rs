//! 在测试里现造 .docx。
//!
//! 造出来的包要同时能被我们和 LibreOffice 打开：LibreOffice 是靠
//! `word/_rels/document.xml.rels` 找样式、编号、页眉这些部件的，
//! 光把文件塞进 zip 它不认，所以关系和 content types 都按 Word 的写法补全。

use std::io::Write;
use std::path::PathBuf;

const W_NS: &str = r#"xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"
 xmlns:o="urn:schemas-microsoft-com:office:office"
 xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"
 xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math"
 xmlns:v="urn:schemas-microsoft-com:vml"
 xmlns:wp14="http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing"
 xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"
 xmlns:w10="urn:schemas-microsoft-com:office:word"
 xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
 xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml"
 xmlns:wpg="http://schemas.microsoft.com/office/word/2010/wordprocessingGroup"
 xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"
 xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
 xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"
 mc:Ignorable="w14 wp14""#;

const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

/// 现有用例一直用的页面：A4，上下 1 英寸，左右 2.8cm。
const DEFAULT_SECT: &str = r#"<w:pgSz w:w="11906" w:h="16838"/>
<w:pgMar w:top="1440" w:right="1588" w:bottom="1440" w:left="1588"/>"#;

struct Rel {
    id: String,
    kind: &'static str,
    target: String,
    external: bool,
}

/// 页眉页脚部件。它们有自己的关系文件（里面的图片、超链接要靠它解析）。
struct HdrFtr {
    is_header: bool,
    part: String,
    xml: String,
}

pub struct DocxBuilder {
    body: String,
    /// sectPr 里的 header/footer 引用。规范要求它们排在最前面，所以单独存。
    sect_refs: String,
    sect: String,
    sect_extra: String,
    styles: Option<String>,
    numbering: Option<String>,
    settings: Option<String>,
    theme: Option<String>,
    created: Option<String>,
    hdrftr: Vec<HdrFtr>,
    media: Vec<(String, Vec<u8>)>,
    rels: Vec<Rel>,
    /// 页眉页脚部件自己的关系（部件名 → 关系）。
    part_rels: Vec<(String, Rel)>,
}

impl Default for DocxBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl DocxBuilder {
    pub fn new() -> Self {
        Self {
            body: String::new(),
            sect_refs: String::new(),
            sect: DEFAULT_SECT.to_string(),
            sect_extra: String::new(),
            styles: None,
            numbering: None,
            settings: None,
            theme: None,
            created: None,
            hdrftr: Vec::new(),
            media: Vec::new(),
            rels: Vec::new(),
            part_rels: Vec::new(),
        }
    }

    /// 追加 `<w:body>` 里的内容（段落、表格……）。
    pub fn body(mut self, xml: &str) -> Self {
        self.body.push_str(xml);
        self
    }

    /// 替换最后那个 `w:sectPr` 的页面几何部分（pgSz、pgMar、cols……）。
    pub fn sect(mut self, xml: &str) -> Self {
        self.sect = xml.to_string();
        self
    }

    /// 在最后那个 `w:sectPr` 里追加内容（docGrid、titlePg……）。
    pub fn sect_extra(mut self, xml: &str) -> Self {
        self.sect_extra.push_str(xml);
        self
    }

    /// `word/styles.xml`，传 `<w:styles>` 里面的内容。
    pub fn styles(mut self, inner: &str) -> Self {
        self.styles = Some(inner.to_string());
        self
    }

    /// `word/numbering.xml`，传 `<w:numbering>` 里面的内容。
    pub fn numbering(mut self, inner: &str) -> Self {
        self.numbering = Some(inner.to_string());
        self
    }

    /// `word/settings.xml`，传 `<w:settings>` 里面的内容。
    pub fn settings(mut self, inner: &str) -> Self {
        self.settings = Some(inner.to_string());
        self
    }

    /// `word/theme/theme1.xml`，传完整文档。
    pub fn theme(mut self, xml: &str) -> Self {
        self.theme = Some(xml.to_string());
        self
    }

    /// `docProps/core.xml` 的 `dcterms:created`（ISO 8601）。
    pub fn created(mut self, iso: &str) -> Self {
        self.created = Some(iso.to_string());
        self
    }

    /// 加一个页眉（`kind` 是 default / first / even），并在 sectPr 里引用它。
    /// `inner` 是 `<w:hdr>` 里面的内容。
    pub fn header(self, kind: &str, inner: &str) -> Self {
        self.hdrftr_part(true, kind, inner)
    }

    /// 同 [`header`](Self::header)，页脚。
    pub fn footer(self, kind: &str, inner: &str) -> Self {
        self.hdrftr_part(false, kind, inner)
    }

    fn hdrftr_part(mut self, is_header: bool, kind: &str, inner: &str) -> Self {
        let n = self
            .hdrftr
            .iter()
            .filter(|h| h.is_header == is_header)
            .count()
            + 1;
        let (stem, rel_kind) = if is_header {
            ("header", "header")
        } else {
            ("footer", "footer")
        };
        let part = format!("{stem}{n}.xml");
        let id = self.next_rel_id();
        self.rels.push(Rel {
            id: id.clone(),
            kind: rel_kind,
            target: part.clone(),
            external: false,
        });
        let tag = if is_header {
            "headerReference"
        } else {
            "footerReference"
        };
        self.sect_refs
            .push_str(&format!(r#"<w:{tag} w:type="{kind}" r:id="{id}"/>"#));
        self.hdrftr.push(HdrFtr {
            is_header,
            part,
            xml: inner.to_string(),
        });
        self
    }

    /// 放一张图片到 `word/media/`，返回给 `a:blip r:embed` 用的关系 id。
    pub fn media(&mut self, name: &str, bytes: Vec<u8>) -> String {
        let id = self.next_rel_id();
        self.rels.push(Rel {
            id: id.clone(),
            kind: "image",
            target: format!("media/{name}"),
            external: false,
        });
        self.media.push((name.to_string(), bytes));
        id
    }

    /// 给页眉/页脚部件里的图片用：关系要写进那个部件自己的 rels。
    /// `part_index` 是第几个页眉（或页脚），从 1 开始。
    pub fn media_in_part(
        &mut self,
        is_header: bool,
        part_index: usize,
        name: &str,
        bytes: Vec<u8>,
    ) -> String {
        let stem = if is_header { "header" } else { "footer" };
        let part = format!("{stem}{part_index}.xml");
        let id = format!("rIdP{}", self.part_rels.len() + 1);
        self.part_rels.push((
            part,
            Rel {
                id: id.clone(),
                kind: "image",
                target: format!("media/{name}"),
                external: false,
            },
        ));
        if !self.media.iter().any(|(n, _)| n == name) {
            self.media.push((name.to_string(), bytes));
        }
        id
    }

    /// 外部超链接，返回给 `w:hyperlink r:id` 用的关系 id。
    pub fn hyperlink(&mut self, url: &str) -> String {
        let id = self.next_rel_id();
        self.rels.push(Rel {
            id: id.clone(),
            kind: "hyperlink",
            target: url.to_string(),
            external: true,
        });
        id
    }

    fn next_rel_id(&self) -> String {
        // rId1..rId9 留给固定部件（样式、编号……），用户部件从 rId10 起，避免撞号。
        format!("rId{}", self.rels.len() + 10)
    }

    /// 写到 `target/tmp/docx/<name>`。
    pub fn build(&self, name: &str) -> PathBuf {
        let path = super::tmp("docx").join(name);
        std::fs::write(&path, self.to_bytes()).unwrap();
        path
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            let mut put = |name: &str, bytes: &[u8]| {
                zip.start_file(name, opts).unwrap();
                zip.write_all(bytes).unwrap();
            };

            put("[Content_Types].xml", self.content_types().as_bytes());
            put("_rels/.rels", self.package_rels().as_bytes());
            put("word/document.xml", self.document_xml().as_bytes());
            put(
                "word/_rels/document.xml.rels",
                self.document_rels().as_bytes(),
            );

            let wrap = |root: &str, inner: &str| {
                format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:{root} {W_NS}>{inner}</w:{root}>"
                )
            };
            if let Some(s) = &self.styles {
                put("word/styles.xml", wrap("styles", s).as_bytes());
            }
            if let Some(n) = &self.numbering {
                put("word/numbering.xml", wrap("numbering", n).as_bytes());
            }
            if let Some(s) = &self.settings {
                put("word/settings.xml", wrap("settings", s).as_bytes());
            }
            if let Some(t) = &self.theme {
                put("word/theme/theme1.xml", t.as_bytes());
            }
            for h in &self.hdrftr {
                let root = if h.is_header { "hdr" } else { "ftr" };
                put(&format!("word/{}", h.part), wrap(root, &h.xml).as_bytes());
                let rels: Vec<&Rel> = self
                    .part_rels
                    .iter()
                    .filter(|(p, _)| *p == h.part)
                    .map(|(_, r)| r)
                    .collect();
                if !rels.is_empty() {
                    put(
                        &format!("word/_rels/{}.rels", h.part),
                        rels_xml(rels.into_iter()).as_bytes(),
                    );
                }
            }
            for (name, bytes) in &self.media {
                put(&format!("word/media/{name}"), bytes);
            }
            if let Some(c) = &self.created {
                put("docProps/core.xml", core_xml(c).as_bytes());
            }
            zip.finish().unwrap();
        }
        buf.into_inner()
    }

    fn document_xml(&self) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:document {W_NS}><w:body>{}\n<w:sectPr>{}{}{}</w:sectPr>\n</w:body></w:document>",
            self.body, self.sect_refs, self.sect, self.sect_extra
        )
    }

    fn content_types(&self) -> String {
        let mut s = String::from(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Default Extension="png" ContentType="image/png"/>
<Default Extension="jpeg" ContentType="image/jpeg"/>
<Default Extension="jpg" ContentType="image/jpeg"/>
<Default Extension="gif" ContentType="image/gif"/>
<Default Extension="bmp" ContentType="image/bmp"/>
<Default Extension="emf" ContentType="image/x-emf"/>
<Default Extension="wmf" ContentType="image/x-wmf"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
"#,
        );
        let ov = |s: &mut String, part: &str, ty: &str| {
            s.push_str(&format!(
                "<Override PartName=\"/{part}\" ContentType=\"application/vnd.openxmlformats-officedocument.{ty}\"/>\n"
            ));
        };
        if self.styles.is_some() {
            ov(&mut s, "word/styles.xml", "wordprocessingml.styles+xml");
        }
        if self.numbering.is_some() {
            ov(
                &mut s,
                "word/numbering.xml",
                "wordprocessingml.numbering+xml",
            );
        }
        if self.settings.is_some() {
            ov(&mut s, "word/settings.xml", "wordprocessingml.settings+xml");
        }
        if self.theme.is_some() {
            ov(&mut s, "word/theme/theme1.xml", "theme+xml");
        }
        for h in &self.hdrftr {
            let ty = if h.is_header {
                "wordprocessingml.header+xml"
            } else {
                "wordprocessingml.footer+xml"
            };
            ov(&mut s, &format!("word/{}", h.part), ty);
        }
        if self.created.is_some() {
            s.push_str("<Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/>\n");
        }
        s.push_str("</Types>");
        s
    }

    fn package_rels(&self) -> String {
        let mut s = String::from(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
"#,
        );
        if self.created.is_some() {
            s.push_str(r#"<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/>
"#);
        }
        s.push_str("</Relationships>");
        s
    }

    fn document_rels(&self) -> String {
        let mut fixed: Vec<Rel> = Vec::new();
        let mut add = |id: &str, kind: &'static str, target: &str| {
            fixed.push(Rel {
                id: id.to_string(),
                kind,
                target: target.to_string(),
                external: false,
            })
        };
        if self.styles.is_some() {
            add("rId1", "styles", "styles.xml");
        }
        if self.numbering.is_some() {
            add("rId2", "numbering", "numbering.xml");
        }
        if self.settings.is_some() {
            add("rId3", "settings", "settings.xml");
        }
        if self.theme.is_some() {
            add("rId4", "theme", "theme/theme1.xml");
        }
        rels_xml(fixed.iter().chain(self.rels.iter()))
    }
}

fn rels_xml<'a>(rels: impl Iterator<Item = &'a Rel>) -> String {
    let mut s = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
"#,
    );
    for r in rels {
        let mode = if r.external {
            r#" TargetMode="External""#
        } else {
            ""
        };
        s.push_str(&format!(
            "<Relationship Id=\"{}\" Type=\"{REL_NS}/{}\" Target=\"{}\"{mode}/>\n",
            r.id,
            r.kind,
            xml_escape(&r.target)
        ));
    }
    s.push_str("</Relationships>");
    s
}

fn core_xml(created: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><dcterms:created xsi:type="dcterms:W3CDTF">{created}</dcterms:created></cp:coreProperties>"#
    )
}

pub fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// 现有用例的标准段落：单倍行距、Times New Roman + 宋体、12 磅。
pub fn para(text: &str) -> String {
    format!(
        r#"<w:p><w:pPr><w:spacing w:line="240" w:lineRule="auto"/></w:pPr>
<w:r><w:rPr><w:rFonts w:ascii="Times New Roman" w:eastAsia="宋体"/><w:sz w:val="24"/></w:rPr>
<w:t xml:space="preserve">{text}</w:t></w:r></w:p>"#
    )
}

/// 与 LibreOffice 逐坐标比对用的段落。
///
/// 字体写的是**本机真实存在的族名**：写「宋体」的话，LibreOffice 会替换成
/// Noto Sans CJK，而我们替换成 Noto Serif CJK —— 两边字体不同，坐标就没有可比性。
pub fn probe_para(ppr: &str, text: &str) -> String {
    format!(
        r#"<w:p><w:pPr>{ppr}</w:pPr><w:r><w:rPr><w:rFonts w:ascii="Liberation Serif" w:hAnsi="Liberation Serif" w:eastAsia="Noto Serif CJK SC"/><w:sz w:val="24"/></w:rPr><w:t xml:space="preserve">{text}</w:t></w:r></w:p>"#
    )
}
