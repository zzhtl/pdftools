//! Phase 2 验收：多图转 PDF。
//!
//! 最核心的一条断言是**字节级直通**：PDF 里那段 JPEG 流必须与源文件一字不差。
//! 这是个二值判定，不给「看起来还行」留模糊空间 —— 「不失真」这个承诺
//! 要么成立要么不成立。

use std::path::PathBuf;

use pdfcore::imaging::Tier;
use pdfcore::ops::images_to_pdf;
use pdfcore::NoProgress;

/// 造一张「照片状」的图：颜色足够多，不会被判成截图而走无损分支。
fn photo(w: u32, h: u32) -> image::RgbImage {
    image::RgbImage::from_fn(w, h, |x, y| {
        // 掺入一点伪随机扰动，制造照片那种逐像素都不同的特征
        let n = (x
            .wrapping_mul(2_654_435_761)
            .wrapping_add(y.wrapping_mul(40_503))
            >> 7) as u8;
        image::Rgb([
            (x * 255 / w.max(1)) as u8 ^ (n >> 3),
            (y * 255 / h.max(1)) as u8 ^ (n >> 4),
            n,
        ])
    })
}

fn write_jpeg(dir: &std::path::Path, name: &str, w: u32, h: u32) -> PathBuf {
    let path = dir.join(name);
    let img = image::DynamicImage::ImageRgb8(photo(w, h));
    img.save_with_format(&path, image::ImageFormat::Jpeg)
        .unwrap();
    path
}

fn tmp() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("img2pdf");
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn jpeg_streams_are_byte_identical_to_source() {
    let dir = tmp();
    let a = write_jpeg(&dir, "a.jpg", 800, 600);
    let b = write_jpeg(&dir, "b.jpg", 600, 800);
    let originals: Vec<Vec<u8>> = [&a, &b].iter().map(|p| std::fs::read(p).unwrap()).collect();

    let report = images_to_pdf::run(&[a, b], Tier::Lossless, &NoProgress).unwrap();
    assert!(
        report.warnings.is_empty(),
        "不该有警告：{:?}",
        report.warnings
    );

    // 两张都应该走直通
    for (path, fidelity) in &report.value.fidelity {
        assert_eq!(
            *fidelity,
            pdfcore::imaging::Fidelity::Passthrough,
            "{} 没有走直通路径",
            path.display()
        );
    }

    let doc = lopdf::Document::load_mem(&report.value.pdf).unwrap();
    assert_eq!(doc.get_pages().len(), 2);

    // 把 PDF 里所有 DCTDecode 的图像流抓出来，与源文件逐字节比对。
    let mut streams: Vec<Vec<u8>> = Vec::new();
    for obj in doc.objects.values() {
        let lopdf::Object::Stream(s) = obj else {
            continue;
        };
        if s.dict.get(b"Subtype").and_then(lopdf::Object::as_name).ok() != Some(b"Image".as_ref()) {
            continue;
        }
        assert_eq!(
            s.dict.get(b"Filter").and_then(lopdf::Object::as_name).ok(),
            Some(b"DCTDecode".as_ref()),
            "图像流应当是 DCTDecode"
        );
        streams.push(s.content.clone());
    }
    assert_eq!(streams.len(), 2, "应当恰好有两个图像流");

    for original in &originals {
        assert!(
            streams.iter().any(|s| s == original),
            "PDF 里找不到与源文件字节完全一致的流 —— 直通路径被破坏了"
        );
    }
}

#[test]
fn page_aspect_follows_the_image() {
    let dir = tmp();
    let wide = write_jpeg(&dir, "wide.jpg", 1600, 900);
    let report = images_to_pdf::run(&[wide], Tier::Lossless, &NoProgress).unwrap();

    let doc = lopdf::Document::load_mem(&report.value.pdf).unwrap();
    let page_id = *doc.get_pages().values().next().unwrap();
    let media = doc
        .get_dictionary(page_id)
        .unwrap()
        .get(b"MediaBox")
        .unwrap()
        .as_array()
        .unwrap();
    let get = |i: usize| media[i].as_float().unwrap();
    let (w, h) = (get(2) - get(0), get(3) - get(1));

    let ratio = w / h;
    assert!(
        (ratio - 1600.0 / 900.0).abs() < 0.01,
        "页面宽高比 {ratio} 应当等于图片的 16:9，否则就会出现白边"
    );
    // 无可信 DPI 元数据时，长边归一到 A4 长边。
    assert!((w - 841.89).abs() < 1.0, "长边应当是 A4 长边，实际 {w}");
}

#[test]
fn oversized_images_are_capped_at_the_dpi_limit() {
    let dir = tmp();
    // 4000×3000 无 DPI 元数据 → 页面长边 841.89pt（约 11.7 英寸），
    // 有效分辨率约 342 DPI，超过「高质量」档的 300 DPI 上限，应当被降采样。
    let big = write_jpeg(&dir, "big.jpg", 4000, 3000);
    let report = images_to_pdf::run(&[big], Tier::HighQuality, &NoProgress).unwrap();

    assert_eq!(
        report.value.fidelity[0].1,
        pdfcore::imaging::Fidelity::Reencoded,
        "超过 DPI 上限的图应当被降采样并重新编码"
    );

    let doc = lopdf::Document::load_mem(&report.value.pdf).unwrap();
    let img = doc
        .objects
        .values()
        .find_map(|o| match o {
            lopdf::Object::Stream(s)
                if s.dict.get(b"Subtype").and_then(lopdf::Object::as_name).ok()
                    == Some(b"Image".as_ref()) =>
            {
                Some(s)
            }
            _ => None,
        })
        .unwrap();
    let w = img.dict.get(b"Width").unwrap().as_i64().unwrap();
    assert!(w < 4000, "宽度应当已被降到 4000 以下，实际 {w}");
    assert!(w > 3000, "降采样不该过头，实际 {w}");
}
