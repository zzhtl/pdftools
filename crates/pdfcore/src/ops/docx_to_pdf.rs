//! Word 文档转 PDF。

use std::path::Path;

use crate::bail_if_cancelled;
use crate::docx::{ir, layout, package, paint, parse};
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
    sink.emit(Progress::Started { total: 4 });

    let pkg = package::open(path)?;
    sink.emit(step(1, "读取文档"));
    bail_if_cancelled!(sink);

    let styles = pkg
        .styles
        .as_deref()
        .map(parse::parse_styles)
        .unwrap_or_default();
    let raw = parse::parse_document(&pkg.document, styles)?;
    sink.emit(step(2, "解析内容"));
    bail_if_cancelled!(sink);

    let doc = ir::build(&raw);
    let mut book = crate::fonts::FontBook::new();
    let laid = layout::layout(&doc, &mut book);
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
    let pdf = paint::paint(&laid, &doc.page, &book, info)?;
    sink.emit(step(4, "生成 PDF"));

    let pages = laid.pages.len();
    Ok(Report::with(
        Outcome {
            pdf,
            pages,
            creation: pkg.created,
        },
        laid.warnings,
    ))
}

fn step(done: usize, label: &str) -> Progress {
    Progress::Item {
        done,
        total: 4,
        label: label.to_string(),
    }
}
