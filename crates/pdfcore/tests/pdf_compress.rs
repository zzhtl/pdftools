//! Phase 5 验收：PDF 压缩。
//!
//! 这里最值钱的不是「能压小」，而是那几条护栏：压不动的时候要老老实实
//! 把原文件还回去，而不是产出一个又大又糊的东西。

use std::path::PathBuf;

use pdfcore::imaging::Tier;
use pdfcore::ops::images_to_pdf;
use pdfcore::pdf::read::compress;
use pdfcore::NoProgress;

fn tmp() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("compress");
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 造一份「扫描件状」的 PDF：一张高分辨率照片铺满整页。
fn scanned_pdf() -> Vec<u8> {
    let dir = tmp();
    let path = dir.join("scan.jpg");
    let img = image::RgbImage::from_fn(2400, 3200, |x, y| {
        let n = (x
            .wrapping_mul(2_654_435_761)
            .wrapping_add(y.wrapping_mul(40_503))
            >> 7) as u8;
        image::Rgb([
            (x * 255 / 2400) as u8 ^ (n >> 3),
            (y * 255 / 3200) as u8 ^ (n >> 4),
            n,
        ])
    });
    image::DynamicImage::ImageRgb8(img)
        .save_with_format(&path, image::ImageFormat::Jpeg)
        .unwrap();
    images_to_pdf::run(&[path], Tier::Lossless, &NoProgress)
        .unwrap()
        .value
        .pdf
}

#[test]
fn scanned_pdf_shrinks_and_stays_readable() {
    let original = scanned_pdf();
    let report = compress::run(&original, Tier::Extreme, false, &NoProgress).unwrap();
    let out = &report.value;

    assert!(
        !out.returned_unchanged,
        "扫描件应当能被压缩，却触发了「压不动」护栏"
    );
    assert!(
        out.pdf.len() < original.len(),
        "压缩后 {} 字节，不小于原来的 {} 字节",
        out.pdf.len(),
        original.len()
    );
    assert_eq!(out.recompressed, 1, "应当恰好重编码了一张图");

    // 结构必须完好：页数不变，且还能被解析。
    let doc = lopdf::Document::load_mem(&out.pdf).unwrap();
    assert_eq!(doc.get_pages().len(), 1);

    eprintln!(
        "扫描件 {} KB → {} KB（-{:.0}%）",
        original.len() / 1024,
        out.pdf.len() / 1024,
        out.saved_ratio() * 100.0
    );
}

#[test]
fn already_compressed_pdf_is_never_made_worse() {
    let original = scanned_pdf();
    let once = compress::run(&original, Tier::Extreme, false, &NoProgress)
        .unwrap()
        .value
        .pdf;

    // 对同一份文件再压一次。此时图片已经是低质量 JPEG，重编码只会更大，
    // 逐图护栏应当保留原图，整体护栏则保证输出不会比输入大。
    let report = compress::run(&once, Tier::Extreme, false, &NoProgress).unwrap();
    let twice = &report.value;

    assert!(
        twice.pdf.len() <= once.len(),
        "二次压缩把文件变大了：{} → {}",
        once.len(),
        twice.pdf.len()
    );
    assert_eq!(
        twice.recompressed, 0,
        "已经压过的图不该再被重编码一次（会又大又糊）"
    );
}

#[test]
fn lossless_tier_does_not_touch_pixels() {
    let original = scanned_pdf();

    // 先取出原始的 JPEG 流
    let before: Vec<Vec<u8>> = image_streams(&original);

    let report = compress::run(&original, Tier::Lossless, false, &NoProgress).unwrap();
    let after = image_streams(&report.value.pdf);

    assert_eq!(report.value.recompressed, 0);
    assert_eq!(
        before, after,
        "「无损」档改动了图像数据 —— 这一档必须保证像素一字不动"
    );
}

#[test]
fn vector_only_pdf_reports_nothing_to_compress() {
    // 一份不含任何图像的 PDF。
    let mut doc = pdfcore::pdf::writer::DocBuilder::new();
    let mut page = pdfcore::pdf::writer::PageSpec::new(595.0, 842.0);
    page.content.save_state();
    page.content.set_fill_rgb(0.2, 0.3, 0.9);
    page.content.rect(100.0, 100.0, 200.0, 150.0);
    page.content.fill_nonzero();
    page.content.restore_state();
    doc.add_page(page);
    let pdf = doc.finish().unwrap();

    let report = compress::run(&pdf, Tier::Extreme, false, &NoProgress).unwrap();
    assert!(
        report.value.is_mostly_vector(),
        "不含图像的 PDF 应当被识别为「以文字/矢量为主」，好在界面上解释为什么没压下去"
    );
}

fn image_streams(pdf: &[u8]) -> Vec<Vec<u8>> {
    let doc = lopdf::Document::load_mem(pdf).unwrap();
    let mut out: Vec<Vec<u8>> = doc
        .objects
        .values()
        .filter_map(|o| match o {
            lopdf::Object::Stream(s)
                if s.dict.get(b"Subtype").and_then(lopdf::Object::as_name).ok()
                    == Some(b"Image".as_ref()) =>
            {
                Some(s.content.clone())
            }
            _ => None,
        })
        .collect();
    out.sort();
    out
}
