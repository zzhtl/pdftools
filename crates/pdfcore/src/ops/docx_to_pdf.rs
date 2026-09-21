//! Word 文档转 PDF。

use std::path::Path;

use crate::bail_if_cancelled;
use crate::docx::{ir, layout, package, paint, parse};
use crate::error::{Report, Result};
use crate::progress::{Progress, ProgressSink};

pub struct Outcome {
    pub pdf: Vec<u8>,
    pub pages: usize,
}

// 手写 Debug：`pdf` 是几百 KB 的字节，derive 出来的输出没法看。
impl std::fmt::Debug for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Outcome")
            .field("pages", &self.pages)
            .field("pdf_bytes", &self.pdf.len())
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
    let mut book = layout::FontBook::new();
    let laid = layout::layout(&doc, &mut book);
    sink.emit(step(3, "排版"));
    bail_if_cancelled!(sink);

    let pdf = paint::paint(&laid, &doc.page, &book)?;
    sink.emit(step(4, "生成 PDF"));

    let pages = laid.pages.len();
    Ok(Report::with(Outcome { pdf, pages }, laid.warnings))
}

fn step(done: usize, label: &str) -> Progress {
    Progress::Item {
        done,
        total: 4,
        label: label.to_string(),
    }
}
