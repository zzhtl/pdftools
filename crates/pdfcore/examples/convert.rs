//! 开发期的手工验证入口：把一个 docx 转成 PDF 并打印转换报告。
//!
//! 用法：cargo run -p pdfcore --example convert -- <输入.docx> <输出.pdf>

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let input = args.next().ok_or("用法：convert <输入.docx> <输出.pdf>")?;
    let output = args.next().unwrap_or_else(|| "out.pdf".to_string());

    let t = std::time::Instant::now();
    let report =
        pdfcore::ops::docx_to_pdf::run(std::path::Path::new(&input), &pdfcore::NoProgress)?;
    std::fs::write(&output, &report.value.pdf)?;

    println!(
        "{} → {}：{} 页，{} KB，耗时 {:?}",
        input,
        output,
        report.value.pages,
        report.value.pdf.len() / 1024,
        t.elapsed()
    );
    if report.warnings.is_empty() {
        println!("无警告");
    } else {
        println!("警告 {} 条：", report.warnings.len());
        for w in &report.warnings {
            match w.page {
                Some(p) => println!("  - 第 {p} 页：{}", w.detail),
                None => println!("  - {}", w.detail),
            }
        }
    }
    Ok(())
}
