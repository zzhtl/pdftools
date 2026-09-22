//! 开发期的手工验证入口。不参与发布，只为在没有界面的情况下跑通各条流水线。
//!
//! ```text
//! cargo run -p pdfcore --example convert -- doc.docx out.pdf
//! cargo run -p pdfcore --example convert -- out.pdf a.jpg b.jpg ...
//! cargo run -p pdfcore --example convert -- --compress in.pdf out.pdf
//! ```

use std::path::{Path, PathBuf};

use pdfcore::imaging::Tier;
use pdfcore::NoProgress;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("用法见文件头注释");
        std::process::exit(2);
    }

    let t = std::time::Instant::now();
    match args[0].as_str() {
        "--compress" => {
            let data = std::fs::read(&args[1])?;
            let report =
                pdfcore::pdf::read::compress::run(&data, Tier::Balanced, false, &NoProgress)?;
            std::fs::write(&args[2], &report.value.pdf)?;
            println!(
                "{} → {}：{} KB → {} KB（{:.0}%），耗时 {:?}",
                args[1],
                args[2],
                data.len() / 1024,
                report.value.pdf.len() / 1024,
                report.value.saved_ratio() * 100.0,
                t.elapsed()
            );
            print_warnings(&report.warnings);
        }
        first if first.to_ascii_lowercase().ends_with(".docx") => {
            let out = args.get(1).cloned().unwrap_or_else(|| "out.pdf".into());
            let report = pdfcore::ops::docx_to_pdf::run(Path::new(first), &NoProgress)?;
            std::fs::write(&out, &report.value.pdf)?;
            println!(
                "{first} → {out}：{} 页，{} KB，耗时 {:?}",
                report.value.pages,
                report.value.pdf.len() / 1024,
                t.elapsed()
            );
            print_warnings(&report.warnings);
        }
        out => {
            let images: Vec<PathBuf> = args[1..].iter().map(PathBuf::from).collect();
            let report = pdfcore::ops::images_to_pdf::run(
                &images,
                Tier::Lossless,
                &Default::default(),
                &NoProgress,
            )?;
            std::fs::write(out, &report.value.pdf)?;
            println!(
                "{} 张图 → {out}：{} KB，耗时 {:?}",
                images.len(),
                report.value.pdf.len() / 1024,
                t.elapsed()
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
