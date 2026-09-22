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

/// 指定 JPEG 质量写盘。质量直接决定源文件的体积，而体积会决定
/// 「重编码后不比原图小就退回直通」这条护栏是否触发。
fn write_jpeg_q(dir: &std::path::Path, name: &str, w: u32, h: u32, quality: u8) -> PathBuf {
    use image::ImageEncoder;
    let path = dir.join(name);
    let img = photo(w, h);
    let mut file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut file, quality)
        .write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgb8)
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
    // 合成的测试图没有 EXIF，「缺少拍摄时间」的提示是预期内的；
    // 但不该出现任何处理失败。
    let failures: Vec<_> = report
        .warnings
        .iter()
        .filter(|w| w.kind != pdfcore::WarningKind::CaptureTimeMissing)
        .collect();
    assert!(failures.is_empty(), "不该有失败类警告：{failures:?}");

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
    //
    // 源图用 q98 保存：这样「降采样 + q92 重编码」确实能省下体积，
    // 降采样这条路径才会被真正执行。若源图本身就是高压缩率的，
    // 「重编码后不比原图小就退回直通」的护栏会（正确地）接管，
    // 那验证的就是另一条规则了。
    let big = write_jpeg_q(&dir, "big.jpg", 4000, 3000, 98);
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

/// 造一张「平滑墙面」式的照片：颜色变化平缓、不同色数很少。
///
/// 这正是曾经把编码选择判错的那类图 —— 用「不同颜色占比」做判据时，
/// 真实的白墙照片只有约 0.8% 的不同色占比，会被当成图形/截图，
/// 于是以原始像素塞进 PDF，体积暴涨十几倍。
fn smooth_wall(w: u32, h: u32) -> image::RgbImage {
    image::RgbImage::from_fn(w, h, |x, y| {
        let base = 226u8;
        let shade = ((x as f32 / w as f32) * 14.0 + (y as f32 / h as f32) * 8.0) as u8;
        image::Rgb([base - shade / 2, base - shade / 2, base - shade])
    })
}

/// 造一张截图式的图：大片纯色 + 高对比的细线条。
fn screenshot(w: u32, h: u32) -> image::RgbImage {
    image::RgbImage::from_fn(w, h, |x, y| {
        if y % 40 < 6 && x > 40 && x < w - 40 {
            image::Rgb([20, 20, 20]) // 文字状横条
        } else if x > w * 3 / 4 {
            image::Rgb([70, 110, 200]) // 侧边栏色块
        } else {
            image::Rgb([250, 250, 250])
        }
    })
}

fn single_image_filter(img: image::RgbImage, name: &str) -> (String, usize) {
    let dir = tmp();
    let path = dir.join(name);
    image::DynamicImage::ImageRgb8(img)
        .save_with_format(&path, image::ImageFormat::Png)
        .unwrap();

    let report = images_to_pdf::run(&[path], Tier::HighQuality, &NoProgress).unwrap();
    let doc = lopdf::Document::load_mem(&report.value.pdf).unwrap();
    let stream = doc
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
    let filter = String::from_utf8_lossy(
        stream
            .dict
            .get(b"Filter")
            .and_then(lopdf::Object::as_name)
            .unwrap_or(b""),
    )
    .into_owned();
    (filter, report.value.pdf.len())
}

/// 平滑的照片必须走 JPEG，不能因为「颜色少」就被当成图形以原始像素存储。
#[test]
fn smooth_photo_is_encoded_as_jpeg_not_raw_pixels() {
    let (filter, size) = single_image_filter(smooth_wall(2000, 1500), "wall.png");
    assert_eq!(
        filter, "DCTDecode",
        "平滑照片被存成了 {filter}，应当是 JPEG"
    );
    // 2000×1500 的 RGB 原始数据是 9 MB。走对路径的话 PDF 应当远小于这个数。
    assert!(
        size < 2 * 1024 * 1024,
        "PDF 体积 {size} 字节，说明图像没有被有效压缩"
    );
}

/// 截图/线稿必须走无损，JPEG 会在文字边缘产生振铃 —— 那正是用户说的「发虚」。
#[test]
fn screenshot_is_stored_losslessly() {
    let (filter, _) = single_image_filter(screenshot(1600, 900), "shot.png");
    assert_eq!(
        filter, "FlateDecode",
        "截图被存成了 {filter}，应当走无损路径以避免文字边缘的 JPEG 振铃"
    );
}
