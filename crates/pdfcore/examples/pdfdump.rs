//! 打印 PDF 每一行的基线位置、各片段的起点、字体、字号与文字。
//!
//! ```text
//! cargo run -p pdfcore --example pdfdump -- a.pdf [b.pdf]
//! ```
//!
//! 给两个文件时，改为报告两者排版是否完全一致（容差 0.001pt）以及第一处差异 ——
//! 排版引擎重构时用它确认「行为零变化」。
//!
//! `--glyphs` 打印每个字形的起点 x（量两端对齐、字距时用）：
//!
//! ```text
//! cargo run -p pdfcore --example pdfdump -- --glyphs a.pdf
//! ```
//!
//! 解析与比对代码与集成测试共用（`tests/common/`），手工看到的和测试断言的是同一个东西。

#![allow(dead_code)]

#[path = "../tests/common/metrics.rs"]
mod metrics;
#[path = "../tests/common/pdftext.rs"]
mod pdftext;
#[path = "../tests/common/raster.rs"]
mod raster;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let glyphs = args.first().is_some_and(|a| a == "--glyphs");
    if glyphs {
        args.remove(0);
    }
    if args.is_empty() || args.len() > 2 || (glyphs && args.len() != 1) {
        eprintln!("用法：pdfdump [--glyphs] a.pdf [b.pdf]");
        std::process::exit(2);
    }
    if glyphs {
        for (i, page) in pdftext::extract(&std::fs::read(&args[0])?)
            .iter()
            .enumerate()
        {
            for line in &page.lines {
                let cells: Vec<String> = line
                    .frags
                    .iter()
                    .flat_map(|f| &f.glyphs)
                    .map(|(t, x)| format!("{t}@{x:.2}"))
                    .collect();
                println!("p{} y={:.3} | {}", i + 1, line.y, cells.join(" "));
            }
        }
        return Ok(());
    }
    let a = pdftext::extract(&std::fs::read(&args[0])?);
    if args.len() == 1 {
        print!("{}", pdftext::dump(&a));
        return Ok(());
    }
    let b = pdftext::extract(&std::fs::read(&args[1])?);
    match metrics::same_layout(&a, &b, 1e-3) {
        Ok(()) => println!("排版一致（{} 页）", a.len()),
        Err(d) => {
            println!("排版不同：{d}");
            std::process::exit(1);
        }
    }
    Ok(())
}
