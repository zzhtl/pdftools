//! 重写前的排版引擎，原样冻结。
//!
//! 新引擎按子步逐项替换旧行为；在全部替换完之前，这份代码是「行为没变」的对照：
//! 新引擎用 [`Calib::legacy`](super::layout::Calib::legacy) 排出来的结果必须与它逐坐标一致。
//! 不要在这里修 bug —— 修在新引擎里，再用校准开关区分新旧行为。

// 原样搬过来的代码里有些条目在新位置上用不到了；冻结的代码不做清理。
#![allow(dead_code)]

mod ir;
mod layout;
mod model;
mod paint;
mod parse;
mod style;

use std::path::Path;

use crate::error::{Report, Result};
use crate::ops::docx_to_pdf::Outcome;

pub fn run(path: &Path) -> Result<Report<Outcome>> {
    let pkg = super::package::open(path)?;
    let styles = pkg
        .styles
        .as_deref()
        .map(parse::parse_styles)
        .unwrap_or_default();
    let raw = parse::parse_document(&pkg.document, styles)?;
    let doc = ir::build(&raw);
    let mut book = crate::fonts::FontBook::new();
    let laid = layout::layout(&doc, &mut book);
    let info = crate::pdf::writer::DocInfo {
        title: path.file_stem().map(|s| s.to_string_lossy().into_owned()),
        subject: None,
        creation: pkg.created,
        modified: Some(crate::timestamp::Timestamp::now()),
    };
    let pdf = paint::paint(&laid, &doc.page, &book, info)?;
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
