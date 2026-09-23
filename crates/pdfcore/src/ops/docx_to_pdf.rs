//! Word 文档转 PDF。

use std::collections::HashMap;
use std::path::Path;

use crate::bail_if_cancelled;
use crate::docx::{ir, layout, model, package, paint, parse};
use crate::error::{Report, Result};
use crate::progress::{Progress, ProgressSink};

pub struct Outcome {
    pub pdf: Vec<u8>,
    pub pages: usize,
    /// 写进 PDF 的创建时间，来自 docx 的 `docProps/core.xml`。
    pub creation: Option<crate::timestamp::Timestamp>,
}

// 手写 Debug：`pdf` 是几百 KB 的字节，derive 出来的输出没法看。
impl std::fmt::Debug for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Outcome")
            .field("pages", &self.pages)
            .field("pdf_bytes", &self.pdf.len())
            .field("creation", &self.creation)
            .finish()
    }
}

pub fn run(path: &Path, sink: &dyn ProgressSink) -> Result<Report<Outcome>> {
    run_with(path, sink, &layout::Calib::current())
}

/// 按指定的排版规则转换。只给测试用：新旧引擎对照、校准前后对比。
#[doc(hidden)]
pub fn run_with(
    path: &Path,
    sink: &dyn ProgressSink,
    calib: &layout::Calib,
) -> Result<Report<Outcome>> {
    sink.emit(Progress::Started { total: 4 });

    let pkg = package::open(path)?;
    sink.emit(step(1, "读取文档"));
    bail_if_cancelled!(sink);

    let styles = pkg
        .styles
        .as_deref()
        .map(parse::parse_styles)
        .unwrap_or_default();
    let settings = pkg
        .settings
        .as_deref()
        .map(parse::parse_settings)
        .unwrap_or_default();
    let mut raw = parse::parse_document(&pkg.document, styles, settings)?;
    raw.theme = pkg
        .theme
        .as_deref()
        .map(parse::parse_theme)
        .unwrap_or_default();
    raw.numbering = pkg
        .numbering
        .as_deref()
        .map(parse::parse_numbering)
        .unwrap_or_default();
    // 页眉页脚里的图片查它们自己的关系表。
    resolve_pictures(&mut raw.body, Some(&pkg.rels));
    // 页眉页脚按关系 id 存：各节的 w:headerReference 写的是关系 id。
    raw.header_footer = pkg
        .rels
        .iter()
        .filter_map(|(id, rel)| {
            let xml = pkg.header_footer.get(&rel.target)?;
            let mut story = parse::parse_header_footer(xml);
            resolve_pictures(&mut story, pkg.part_rels.get(&rel.target));
            Some((id.clone(), story))
        })
        .collect();
    raw.hyperlinks = pkg
        .rels
        .iter()
        .filter(|(_, r)| r.external)
        .map(|(id, r)| (id.clone(), r.target.clone()))
        .collect();
    sink.emit(step(2, "解析内容"));
    bail_if_cancelled!(sink);

    let doc = ir::build(&raw, calib);
    let mut book = crate::fonts::FontBook::new();
    let laid = layout::layout(&doc, &mut book, calib);
    sink.emit(step(3, "排版"));
    bail_if_cancelled!(sink);

    // 文档的创建时间沿用 docx 自己的，而不是「导出的那一刻」——
    // 别人拿到 PDF 看属性，关心的是文档什么时候写的。
    let title = path.file_stem().map(|s| s.to_string_lossy().into_owned());
    let info = crate::pdf::writer::DocInfo {
        title,
        subject: None,
        creation: pkg.created,
        modified: Some(crate::timestamp::Timestamp::now()),
    };
    let (pdf, painted) = paint::paint(&laid, &book, info, &pkg.media)?;
    sink.emit(step(4, "生成 PDF"));

    let pages = laid.pages.len();
    let mut warnings = laid.warnings;
    warnings.extend(painted);
    Ok(Report::with(
        Outcome {
            pdf,
            pages,
            creation: pkg.created,
        },
        warnings,
    ))
}

/// 图片写的是关系 id，换成包里的部件路径。找不到、或者指向包外的，留空。
fn resolve_pictures(
    story: &mut model::Story,
    rels: Option<&HashMap<String, package::Relationship>>,
) {
    model::for_each_picture(story, &mut |pic| {
        pic.target = pic
            .target
            .as_ref()
            .and_then(|id| rels?.get(id))
            .filter(|r| !r.external)
            .map(|r| r.target.clone());
    });
}

/// 用重写前的排版引擎转换。只给测试做新旧对照用。
#[doc(hidden)]
pub fn run_legacy(path: &Path) -> Result<Report<Outcome>> {
    crate::docx::legacy::run(path)
}

fn step(done: usize, label: &str) -> Progress {
    Progress::Item {
        done,
        total: 4,
        label: label.to_string(),
    }
}
