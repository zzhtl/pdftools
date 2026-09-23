//! 排版耗时：约 300 页的合成文档，转 3 遍取暖态平均。其中一段长约两万字 ——
//! 断行不能随段落长度退化成平方复杂度。
//!
//! 只报告，不设门槛（机器之间差得太多）。计时要在 release 下才有意义：
//! `cargo test --release -p pdfcore --test docx_timing -- --ignored --nocapture`
//!
//! 参考：重写排版引擎时在开发机上，296 页新引擎 229ms，重写前的引擎 294ms。

mod common;

use common::probes;
use pdfcore::ops::docx_to_pdf;
use pdfcore::NoProgress;

#[test]
#[ignore = "计时，手动在 release 下跑"]
fn timing_300_pages() {
    if !common::require_cjk_font() {
        return;
    }
    let ppr = r#"<w:spacing w:line="312" w:lineRule="auto"/><w:ind w:firstLine="480" w:firstLineChars="200"/>"#;
    let mut body: String = (0..1500)
        .map(|i| common::docx::probe_para(ppr, &probes::filler(i, 90)))
        .collect();
    body += &common::docx::probe_para(ppr, &probes::filler(7, 20_000));
    let path = common::docx::DocxBuilder::new()
        .body(&body)
        .sect_extra(r#"<w:docGrid w:type="lines" w:linePitch="312"/>"#)
        .build("timing_300_pages.docx");

    let first = docx_to_pdf::run(&path, &NoProgress).unwrap();
    let t = std::time::Instant::now();
    for _ in 0..3 {
        docx_to_pdf::run(&path, &NoProgress).unwrap();
    }
    eprintln!("{} 页：{:?}", first.value.pages, t.elapsed() / 3);
}
