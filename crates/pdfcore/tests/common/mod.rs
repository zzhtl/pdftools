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
