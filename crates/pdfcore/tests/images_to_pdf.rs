//! Phase 2 验收：多图转 PDF。
//!
//! 最核心的一条断言是**字节级直通**：PDF 里那段 JPEG 流必须与源文件一字不差。
//! 这是个二值判定，不给「看起来还行」留模糊空间 —— 「不失真」这个承诺
//! 要么成立要么不成立。

mod common;

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

    let report =
        images_to_pdf::run(&[a, b], Tier::Lossless, &Default::default(), &NoProgress).unwrap();
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
    let report =
        images_to_pdf::run(&[wide], Tier::Lossless, &Default::default(), &NoProgress).unwrap();

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
    let report =
        images_to_pdf::run(&[big], Tier::HighQuality, &Default::default(), &NoProgress).unwrap();

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
///
/// 带 ±2 的轻微噪点：真实照片都有传感器噪点。数学上完美的无噪渐变
/// 用无损存储反而更小（实测 Flate 59 KB 对 JPEG 84 KB），那时选无损是对的，
/// 拿它当「照片」来断言必须走 JPEG 就测错了对象。
fn smooth_wall(w: u32, h: u32) -> image::RgbImage {
    image::RgbImage::from_fn(w, h, |x, y| {
        let base = 226u8;
        let shade = ((x as f32 / w as f32) * 14.0 + (y as f32 / h as f32) * 8.0) as u8;
        let noise = ((x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503)) >> 13) as u8 % 5;
        image::Rgb([
            base - shade / 2 + noise - 2,
            base - shade / 2 + noise - 2,
            base - shade + noise - 2,
        ])
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

    let report =
        images_to_pdf::run(&[path], Tier::HighQuality, &Default::default(), &NoProgress).unwrap();
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

/// 我们自己编码出来的 JPEG，必须能被我们自己的解码器正确解回来。
///
/// 这不是废话：zune-jpeg 0.5.15（image crate 用的解码器）会把 jpeg-encoder
/// 「优化 Huffman 表 + 4:2:0」编出的部分 JPEG 解成横条纹，而 libjpeg 解同一份
/// 文件完全正常。结果是：用户把我们压过的 PDF 再压一次（换档位或转灰度），
/// 图片会被悄悄毁掉；hayro 渲染我们的输出也会花屏。
#[test]
fn own_jpeg_output_decodes_correctly() {
    let (w, h) = (1190u32, 1488u32);
    let src = photo(w, h);
    let mean = |v: &[u8]| v.iter().map(|x| *x as f64).sum::<f64>() / v.len() as f64;
    let expected = mean(src.as_raw());
    for q in [58u8, 72, 85, 92] {
        let jpeg = pdfcore::imaging::encode_jpeg_image(
            &image::DynamicImage::ImageRgb8(src.clone()),
            q,
            false,
        )
        .unwrap();
        let back = image::load_from_memory(&jpeg).unwrap().to_rgb8();
        let got = mean(back.as_raw());
        assert!(
            (got - expected).abs() < 3.0,
            "质量 {q}：自己编码的 JPEG 解回来平均值 {got:.1}，原图 {expected:.1} —— 解码结果已损坏"
        );
    }
}

/// 所有档位都不能把 JPEG 越压越大。
///
/// 「平衡」「极致」档不走原图直通，曾经对不需要降采样的 JPEG 也照样解码再重编码 ——
/// 源图本身压缩率就高时，结果又大又多损失一代画质。
#[test]
fn lossy_tiers_never_grow_a_jpeg() {
    let dir = tmp();
    // 800×600 放在 A4 上约 68 DPI，任何档位都不需要降采样；
    // q10 的源图块效应很重，更高质量的重编码会把块边缘一并保留下来，必然更大。
    let src = write_jpeg_q(&dir, "small_q10.jpg", 800, 600, 10);
    let src_len = std::fs::metadata(&src).unwrap().len() as usize;
    for tier in [Tier::HighQuality, Tier::Balanced, Tier::Extreme] {
        let report = images_to_pdf::run(
            std::slice::from_ref(&src),
            tier,
            &Default::default(),
            &NoProgress,
        )
        .unwrap();
        let (filter, len) = single_image_stream(&report.value.pdf);
        assert!(
            len <= src_len,
            "{tier:?}：PDF 里的图 {len} 字节（{filter}），比源文件 {src_len} 字节还大"
        );
    }
}

fn single_image_stream(pdf: &[u8]) -> (String, usize) {
    let doc = lopdf::Document::load_mem(pdf).unwrap();
    doc.objects
        .values()
        .find_map(|o| match o {
            lopdf::Object::Stream(s)
                if s.dict.get(b"Subtype").and_then(lopdf::Object::as_name).ok()
                    == Some(b"Image".as_ref()) =>
            {
                let f = s
                    .dict
                    .get(b"Filter")
                    .and_then(lopdf::Object::as_name)
                    .map(|n| String::from_utf8_lossy(n).into_owned())
                    .unwrap_or_default();
                Some((f, s.content.len()))
            }
            _ => None,
        })
        .unwrap()
}

/// PDF 的 DCTDecode 只支持 8 位的基线/渐进式 JPEG。12 位、算术编码、无损 JPEG
/// 原样直通进去，阅读器打开就是空白或报错 —— 这种文件宁可明说处理不了。
#[test]
fn only_8bit_baseline_or_progressive_jpeg_passes_through() {
    let dir = tmp();
    let good = write_jpeg_q(&dir, "sof_good.jpg", 640, 480, 90);
    let bytes = std::fs::read(&good).unwrap();
    let sof = bytes
        .windows(2)
        .position(|w| w == [0xFF, 0xC0])
        .expect("应当是基线 JPEG");

    let mut twelve_bit = bytes.clone();
    twelve_bit[sof + 4] = 12; // SOF 里的样本精度
    let mut arithmetic = bytes.clone();
    arithmetic[sof + 1] = 0xC9; // SOF9：算术编码
    let p12 = dir.join("sof_12bit.jpg");
    let parith = dir.join("sof_arith.jpg");
    std::fs::write(&p12, &twelve_bit).unwrap();
    std::fs::write(&parith, &arithmetic).unwrap();

    let report = images_to_pdf::run(
        &[good.clone(), p12.clone(), parith.clone()],
        Tier::Lossless,
        &Default::default(),
        &NoProgress,
    )
    .unwrap();
    for (path, fidelity) in &report.value.fidelity {
        if path != &good {
            assert_ne!(
                *fidelity,
                pdfcore::imaging::Fidelity::Passthrough,
                "{} 不是 8 位基线/渐进式 JPEG，不能原样搬进 PDF",
                path.display()
            );
        }
    }
}

/// 多页 TIFF 只转第一页，这必须说出来 —— 静默丢页正是本项目最忌讳的事。
#[test]
fn multi_page_tiff_is_reported() {
    let dir = tmp();
    let path = dir.join("three_pages.tif");
    std::fs::write(&path, common::images::tiff_pages(3)).unwrap();
    let report = images_to_pdf::run(
        std::slice::from_ref(&path),
        Tier::Lossless,
        &Default::default(),
        &NoProgress,
    )
    .unwrap();
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.detail.contains("3 页") && w.detail.contains("第一页")),
        "多页 TIFF 丢了后面的页却没有提示：{:?}",
        report.warnings
    );
}

/// 能拖进来的扩展名，必须是真的解得开的格式。avif / exr / hdr / dds 的解码器
/// 没有编进来：放进列表只会让用户在转换时才看到「解码失败」。
#[test]
fn accepted_extensions_match_compiled_decoders() {
    use pdfcore::imaging::probe::looks_like_image;
    use std::path::Path;
    for ext in [
        "jpg", "jpeg", "png", "gif", "bmp", "tif", "tiff", "webp", "ico", "tga", "pnm", "ppm",
        "pgm", "pbm", "qoi", "JPG",
    ] {
        assert!(
            looks_like_image(Path::new(&format!("a.{ext}"))),
            ".{ext} 应当被接受"
        );
    }
    for ext in ["avif", "exr", "hdr", "dds", "ff", "heic", "pdf", "txt"] {
        assert!(
            !looks_like_image(Path::new(&format!("a.{ext}"))),
            ".{ext} 的解码器没有编进来，不该被当成图片接受"
        );
    }
}

/// PDF 里第 `page` 页（0 起）那张图解出来的样本、分量数（灰度 1，RGB 3）。
fn page_image(pdf: &[u8], page: usize) -> (Vec<u8>, usize) {
    let doc = lopdf::Document::load_mem(pdf).unwrap();
    let page_id = doc.get_pages().values().copied().nth(page).unwrap();
    let (resources, _) = doc.get_page_resources(page_id).unwrap();
    let xobjects = resources
        .unwrap()
        .get(b"XObject")
        .and_then(lopdf::Object::as_dict)
        .unwrap();
    let (_, reference) = xobjects.iter().next().unwrap();
    let stream = doc
        .get_object(reference.as_reference().unwrap())
        .and_then(lopdf::Object::as_stream)
        .unwrap();
    let gray = stream
        .dict
        .get(b"ColorSpace")
        .and_then(lopdf::Object::as_name)
        .ok()
        == Some(b"DeviceGray".as_ref());
    (
        stream.decompressed_content().unwrap(),
        if gray { 1 } else { 3 },
    )
}

/// 无损档里的 PNG：按行做 PNG 预测再压缩，解出来与源图逐像素一致；灰度图按灰度存，
/// 不扩成 RGB。
#[test]
fn lossless_png_round_trips_through_the_pdf() {
    let dir = tmp();
    let rgb = screenshot(301, 97);
    let gray = image::GrayImage::from_fn(123, 45, |x, y| image::Luma([(x * 2 + y * 3) as u8]));
    let (rgb_path, gray_path) = (dir.join("rt_rgb.png"), dir.join("rt_gray.png"));
    rgb.save(&rgb_path).unwrap();
    gray.save(&gray_path).unwrap();

    let report = images_to_pdf::run(
        &[rgb_path, gray_path],
        Tier::Lossless,
        &Default::default(),
        &NoProgress,
    )
    .unwrap();
    let pdf = &report.value.pdf;
    assert_eq!(page_image(pdf, 0), (rgb.into_raw(), 3));
    assert_eq!(page_image(pdf, 1), (gray.into_raw(), 1));
}

/// 几张图同时准备，放进 PDF 还是原来的顺序；中间坏了一张不影响其余的。
#[test]
fn pages_keep_the_input_order() {
    let dir = tmp();
    let mut paths: Vec<PathBuf> = (0..11)
        .map(|i| write_jpeg(&dir, &format!("order_{i}.jpg"), 100 + i * 37, 100))
        .collect();
    let broken = dir.join("order_broken.jpg");
    std::fs::write(&broken, b"not an image").unwrap();
    paths.insert(5, broken);

    let report =
        images_to_pdf::run(&paths, Tier::Lossless, &Default::default(), &NoProgress).unwrap();
    let doc = lopdf::Document::load_mem(&report.value.pdf).unwrap();
    let ratios: Vec<f32> = doc
        .get_pages()
        .values()
        .map(|&id| {
            let page = doc.get_dictionary(id).unwrap();
            let b: Vec<f32> = page
                .get(b"MediaBox")
                .and_then(lopdf::Object::as_array)
                .unwrap()
                .iter()
                .map(|v| v.as_float().unwrap())
                .collect();
            (b[2] - b[0]) / (b[3] - b[1])
        })
        .collect();
    let expected: Vec<f32> = (0..11).map(|i| (100 + i * 37) as f32 / 100.0).collect();
    assert_eq!(ratios.len(), expected.len());
    for (got, want) in ratios.iter().zip(&expected) {
        assert!((got - want).abs() < 0.01, "{ratios:?}");
    }
    assert_eq!(
        report
            .warnings
            .iter()
            .filter(|w| w.detail.starts_with("order_broken.jpg"))
            .count(),
        1,
        "{:?}",
        report.warnings
    );
}

/// 进度报到第 3 张时取消：整个任务以「已取消」结束，不会把剩下的图做完。
#[test]
fn cancelling_stops_the_batch() {
    use pdfcore::{Progress, ProgressSink};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CancelAt {
        at: usize,
        done: AtomicUsize,
    }
    impl ProgressSink for CancelAt {
        fn emit(&self, p: Progress) {
            if let Progress::Item { done, .. } = p {
                self.done.store(done, Ordering::SeqCst);
            }
        }
        fn is_cancelled(&self) -> bool {
            self.done.load(Ordering::SeqCst) >= self.at
        }
    }

    let dir = tmp();
    let paths: Vec<PathBuf> = (0..40)
        .map(|i| write_jpeg(&dir, &format!("cancel_{i}.jpg"), 64, 48))
        .collect();
    let sink = CancelAt {
        at: 3,
        done: AtomicUsize::new(0),
    };
    let result = images_to_pdf::run(&paths, Tier::Lossless, &Default::default(), &sink);
    assert!(
        matches!(result, Err(pdfcore::CoreError::Cancelled)),
        "{:?}",
        result.map(|r| r.value)
    );
    assert!(sink.done.load(Ordering::SeqCst) < paths.len());
}
