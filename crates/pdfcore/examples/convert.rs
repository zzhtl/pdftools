//! 开发期的手工验证入口。不参与发布，只为在没有界面的情况下跑通各条流水线。
//!
//! ```text
//! cargo run -p pdfcore --example convert -- doc.docx out.pdf
//! cargo run -p pdfcore --example convert -- out.pdf a.jpg b.jpg ...
//! cargo run -p pdfcore --example convert -- --compress in.pdf out.pdf
//! ```
//!
//! 加 `--repeat N` 会把同一次转换跑 N 遍，分别报告第一遍与其余各遍的平均耗时
//! —— 第一遍含字体扫描等一次性开销，其余各遍反映的是缓存热了以后的真实速度。
//! 图片转 PDF 缺省用无损档，`--tier high|balanced|extreme` 换档。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use pdfcore::imaging::Tier;
use pdfcore::NoProgress;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let repeat = match args.iter().position(|a| a == "--repeat") {
        Some(i) => {
            let n: usize = args
                .get(i + 1)
                .and_then(|v| v.parse().ok())
                .ok_or("--repeat 后面要跟次数")?;
            args.drain(i..i + 2);
            n.max(1)
        }
        None => 1,
    };
    let tier = match args.iter().position(|a| a == "--tier") {
        Some(i) => {
            let tier = match args.get(i + 1).map(String::as_str) {
                Some("lossless") => Tier::Lossless,
                Some("high") => Tier::HighQuality,
                Some("balanced") => Tier::Balanced,
                Some("extreme") => Tier::Extreme,
                _ => return Err("--tier 后面要跟 lossless / high / balanced / extreme".into()),
            };
            args.drain(i..i + 2);
            tier
        }
        None => Tier::Lossless,
    };
    if args.is_empty() {
        eprintln!("用法见文件头注释");
        std::process::exit(2);
    }

    match args[0].as_str() {
        "--compress" => {
            let data = std::fs::read(&args[1])?;
            let (report, timing) = timed(repeat, || {
                pdfcore::ops::pdf_compress::run(&data, Tier::Balanced, false, &NoProgress)
            })?;
            std::fs::write(&args[2], &report.value.pdf)?;
            println!(
                "{} → {}：{} KB → {} KB（{:.0}%），{timing}",
                args[1],
                args[2],
                data.len() / 1024,
                report.value.pdf.len() / 1024,
                report.value.saved_ratio() * 100.0,
            );
            print_warnings(&report.warnings);
        }
        first if first.to_ascii_lowercase().ends_with(".docx") => {
            let out = args.get(1).cloned().unwrap_or_else(|| "out.pdf".into());
            let (report, timing) = timed(repeat, || {
                pdfcore::ops::docx_to_pdf::run(Path::new(first), &NoProgress)
            })?;
            std::fs::write(&out, &report.value.pdf)?;
            println!(
                "{first} → {out}：{} 页，{} KB，{timing}",
                report.value.pages,
                report.value.pdf.len() / 1024,
            );
            print_warnings(&report.warnings);
        }
        out => {
            let images: Vec<PathBuf> = args[1..].iter().map(PathBuf::from).collect();
            let (report, timing) = timed(repeat, || {
                pdfcore::ops::images_to_pdf::run(&images, tier, &Default::default(), &NoProgress)
            })?;
            std::fs::write(out, &report.value.pdf)?;
            println!(
                "{} 张图 → {out}：{} KB，{timing}",
                images.len(),
                report.value.pdf.len() / 1024,
            );
            for (p, f) in &report.value.fidelity {
                println!(
                    "  {:<28} {}",
                    p.file_name().unwrap_or_default().to_string_lossy(),
                    f.label()
                );
            }
            print_warnings(&report.warnings);
        }
    }
    Ok(())
}

/// 跑 `n` 遍，返回最后一遍的结果与耗时说明。
fn timed<T>(n: usize, mut f: impl FnMut() -> pdfcore::Result<T>) -> pdfcore::Result<(T, String)> {
    let t = Instant::now();
    let mut value = f()?;
    let first = t.elapsed();
    if n == 1 {
        return Ok((value, format!("耗时 {first:?}")));
    }
    let mut rest = Duration::ZERO;
    for _ in 1..n {
        let t = Instant::now();
        value = f()?;
        rest += t.elapsed();
    }
    let avg = rest / (n as u32 - 1);
    Ok((
        value,
        format!("首遍 {first:?}，其余 {} 遍平均 {avg:?}", n - 1),
    ))
}

fn print_warnings(warnings: &[pdfcore::Warning]) {
    if warnings.is_empty() {
        println!("无警告");
        return;
    }
    println!("警告 {} 条：", warnings.len());
    for w in warnings {
        match w.page {
            Some(p) => println!("  - 第 {p} 页：{}", w.detail),
            None => println!("  - {}", w.detail),
        }
    }
}
