//! 集成测试共用的工具。
//!
//! 每个测试文件各自 `mod common;` 一份，用不到的函数在那份编译单元里就是死代码，
//! 所以这里整体放开 `dead_code`，否则 CI 的 `-D warnings` 会拦下来。
#![allow(dead_code)]

pub mod calib;
pub mod docx;
pub mod images;
pub mod lo;
pub mod metrics;
pub mod pdfpaths;
pub mod pdftext;
pub mod probes;
pub mod raster;

use std::path::PathBuf;

/// 测试产物目录（`target/tmp/<sub>`）。
pub fn tmp(sub: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(sub);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 依赖中文字形的用例的前置条件。
///
/// 没有中文字体时，所有汉字都会落到同一个 `.notdef`，文字回抽必然对不上。
/// 那种失败信息指向的是环境而不是代码，容易把人带偏，所以这里明确跳过并说明。
/// CI 上会安装 `fonts-noto-cjk`，因此这些用例在 CI 里是实打实跑过的。
pub fn require_cjk_font() -> bool {
    if pdfcore::fonts::system::SystemFonts::shared().has_cjk() {
        return true;
    }
    eprintln!("跳过：本机没有中文字体（安装 fonts-noto-cjk 或思源黑体后可跑）");
    false
}

/// 新排版引擎按重写前的规则（`Calib::legacy()`）排出来的，与重写前的引擎一模一样：
/// 逐行的基线、片段起点、字体、字号、文字（容差 0.001pt），以及全部警告。
///
/// 排版引擎按子步重写，每一步都要能证明「没改的地方真没变」；
/// 有意改掉的规则都在校准开关里，按旧取值排就该与旧引擎一致。
pub fn assert_same_as_legacy(path: &std::path::Path) {
    use pdfcore::ops::docx_to_pdf;
    let report = docx_to_pdf::run_with(
        path,
        &pdfcore::NoProgress,
        &pdfcore::docx::layout::Calib::legacy(),
    )
    .expect("转换失败");
    let legacy = docx_to_pdf::run_legacy(path).expect("旧引擎转换失败");
    let ours = pdftext::extract(&report.value.pdf);
    let theirs = pdftext::extract(&legacy.value.pdf);
    if let Err(diff) = metrics::same_layout(&ours, &theirs, 1e-3) {
        panic!("{}：新引擎与重写前的排版不同 —— {diff}", path.display());
    }
    let warnings = |r: &pdfcore::Report<_>| -> Vec<(Option<usize>, String, String)> {
        r.warnings
            .iter()
            .map(|w| (w.page, format!("{:?}", w.kind), w.detail.clone()))
            .collect()
    };
    assert_eq!(
        warnings(&report),
        warnings(&legacy),
        "{}：新引擎与重写前的警告不同",
        path.display()
    );
}
