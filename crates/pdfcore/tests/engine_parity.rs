//! 新排版引擎与重写前的引擎逐坐标对照。
//!
//! `tests/docx_to_pdf.rs` 的每个用例都顺带做这项对照；这里再把全部构造探针
//! （能跨页、覆盖行距、网格、缩进、对齐、分页……）过一遍。

mod common;

use common::probes;
use pdfcore::ops::docx_to_pdf;
use pdfcore::NoProgress;

#[test]
fn every_probe_lays_out_exactly_as_before() {
    if !common::require_cjk_font() {
        return;
    }
    let dir = common::tmp("engine_parity");
    for probe in probes::all() {
        // 旧引擎丢掉自闭合的空段落 `<w:p/>`，新引擎照 Word 的样子把它排成一个空行。
        // 这一处差异单独验证，见下一个用例。
        if probe.name == "empty_paragraphs" {
            continue;
        }
        let path = dir.join(format!("{}.docx", probe.name));
        std::fs::write(&path, probe.doc.to_bytes()).unwrap();
        let report = docx_to_pdf::run(&path, &NoProgress).expect("转换失败");
        common::assert_same_as_legacy(&path, &report);
    }
}

/// `<w:p/>` 与 `<w:p></w:p>` 是同一个空段落。新引擎两种写法排得一样，
/// 而且与旧引擎排 `<w:p></w:p>` 的结果一样 —— 差异只在旧引擎丢了自闭合写法。
#[test]
fn self_closing_empty_paragraphs_are_kept() {
    if !common::require_cjk_font() {
        return;
    }
    let dir = common::tmp("engine_parity");
    let bare = dir.join("bare.docx");
    let open_close = dir.join("open_close.docx");
    std::fs::write(&bare, probes::empty_paragraphs("<w:p/>").to_bytes()).unwrap();
    std::fs::write(
        &open_close,
        probes::empty_paragraphs("<w:p></w:p>").to_bytes(),
    )
    .unwrap();

    let ours = docx_to_pdf::run(&bare, &NoProgress).expect("转换失败");
    let reference = docx_to_pdf::run(&open_close, &NoProgress).expect("转换失败");
    let (a, b) = (
        common::pdftext::extract(&ours.value.pdf),
        common::pdftext::extract(&reference.value.pdf),
    );
    common::metrics::same_layout(&a, &b, 1e-3).expect("两种写法排得不一样");
    common::assert_same_as_legacy(&open_close, &reference);
}

/// 约 300 页的合成文档，新旧引擎各转 3 遍取暖态平均；同时确认排版一致。
/// 其中一段长约两万字 —— 断行不能随段落长度退化成平方复杂度。
///
/// 计时要在 release 下才有意义：
/// `cargo test --release -p pdfcore --test engine_parity -- --ignored --nocapture`
#[test]
#[ignore]
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

    let time = |f: &dyn Fn() -> pdfcore::Report<docx_to_pdf::Outcome>| {
        let first = f();
        let t = std::time::Instant::now();
        for _ in 0..3 {
            f();
        }
        (first, t.elapsed() / 3)
    };
    let (ours, t_new) = time(&|| docx_to_pdf::run(&path, &NoProgress).unwrap());
    let (_, t_old) = time(&|| docx_to_pdf::run_legacy(&path).unwrap());
    eprintln!(
        "{} 页：新引擎 {t_new:?}，重写前 {t_old:?}",
        ours.value.pages
    );
    common::assert_same_as_legacy(&path, &ours);
    // 预算按实测定：本机 release 下 296 页新引擎 229ms、重写前 294ms。
    assert!(
        t_new.as_secs_f64() <= t_old.as_secs_f64() * 1.25,
        "新引擎比重写前慢了 25% 以上"
    );
}
