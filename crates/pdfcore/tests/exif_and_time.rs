//! 方向与时间。
//!
//! 这两件事共用 EXIF，也共同决定了「不失真」这个承诺是否对手机照片成立：
//! 横拍的照片几乎都带方向标记，如果为了摆正而重新编码，承诺就落空了。

use std::path::PathBuf;

use pdfcore::imaging::Tier;
use pdfcore::ops::images_to_pdf;
use pdfcore::{NoProgress, TimeSource};

fn tmp() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("exif");
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 手工拼一段最小可用的 EXIF（APP1）并插进 JPEG 的 SOI 之后。
///
/// 只放两样东西：Orientation 和 DateTimeOriginal ——
/// 正好是本测试关心的全部内容。偏移量都相对 TIFF 头起点。
fn exif_app1(orientation: u16, datetime: &str) -> Vec<u8> {
    assert_eq!(datetime.len(), 19, "EXIF 时间格式固定为 19 个字符");

    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"MM\x00\x2a"); // 大端字节序
    tiff.extend_from_slice(&8u32.to_be_bytes()); // IFD0 在偏移 8

    // IFD0：2 个条目
    tiff.extend_from_slice(&2u16.to_be_bytes());
    // Orientation (0x0112)，SHORT，1 个。大端下 SHORT 值放在 4 字节字段的前 2 字节。
    tiff.extend_from_slice(&0x0112u16.to_be_bytes());
    tiff.extend_from_slice(&3u16.to_be_bytes());
    tiff.extend_from_slice(&1u32.to_be_bytes());
    tiff.extend_from_slice(&orientation.to_be_bytes());
    tiff.extend_from_slice(&[0, 0]);
    // ExifIFD 指针 (0x8769)，LONG，指向偏移 38
    tiff.extend_from_slice(&0x8769u16.to_be_bytes());
    tiff.extend_from_slice(&4u16.to_be_bytes());
    tiff.extend_from_slice(&1u32.to_be_bytes());
    tiff.extend_from_slice(&38u32.to_be_bytes());
    tiff.extend_from_slice(&0u32.to_be_bytes()); // 没有下一个 IFD

    // ExifIFD @38：1 个条目
    debug_assert_eq!(tiff.len(), 38);
    tiff.extend_from_slice(&1u16.to_be_bytes());
    // DateTimeOriginal (0x9003)，ASCII，20 字节（含结尾 NUL），放在偏移 56
    tiff.extend_from_slice(&0x9003u16.to_be_bytes());
    tiff.extend_from_slice(&2u16.to_be_bytes());
    tiff.extend_from_slice(&20u32.to_be_bytes());
    tiff.extend_from_slice(&56u32.to_be_bytes());
    tiff.extend_from_slice(&0u32.to_be_bytes());

    debug_assert_eq!(tiff.len(), 56);
    tiff.extend_from_slice(datetime.as_bytes());
    tiff.push(0);

    let mut payload = b"Exif\x00\x00".to_vec();
    payload.extend_from_slice(&tiff);

    let mut app1 = vec![0xFF, 0xE1];
    app1.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    app1.extend_from_slice(&payload);
    app1
}

fn photo(w: u32, h: u32) -> image::RgbImage {
    image::RgbImage::from_fn(w, h, |x, y| {
        let n = (x
            .wrapping_mul(2_654_435_761)
            .wrapping_add(y.wrapping_mul(40_503))
            >> 7) as u8;
        image::Rgb([
            (x * 255 / w) as u8 ^ (n >> 3),
            (y * 255 / h) as u8 ^ (n >> 4),
            n,
        ])
    })
}

/// 写一张带 EXIF 的 JPEG。
fn write_jpeg_with_exif(name: &str, w: u32, h: u32, orientation: u16, dt: &str) -> PathBuf {
    let mut jpeg = Vec::new();
    image::DynamicImage::ImageRgb8(photo(w, h))
        .write_to(
            &mut std::io::Cursor::new(&mut jpeg),
            image::ImageFormat::Jpeg,
        )
        .unwrap();

    // 把 APP1 插在 SOI（FFD8）之后
    let mut out = jpeg[..2].to_vec();
    out.extend_from_slice(&exif_app1(orientation, dt));
    out.extend_from_slice(&jpeg[2..]);

    let path = tmp().join(name);
    std::fs::write(&path, &out).unwrap();
    path
}

/// 核心承诺：横拍的照片也必须字节级直通。
///
/// 旋转由 PDF 的变换矩阵完成，像素一个字节都不动。若退化成
/// 「解码 → 旋转 → 重新编码」，「绝不失真」对手机照片就失效了 ——
/// 而手机照片正是这个功能最主要的输入。
#[test]
fn rotated_jpeg_still_passes_through_byte_identically() {
    // orientation = 6 表示需要顺时针旋转 90 度
    let path = write_jpeg_with_exif("rot90.jpg", 1600, 900, 6, "2024:03:15 14:30:22");
    let original = std::fs::read(&path).unwrap();

    let report = images_to_pdf::run(&[path], Tier::Lossless, &NoProgress).unwrap();
    assert_eq!(
        report.value.fidelity[0].1,
        pdfcore::imaging::Fidelity::Passthrough,
        "带 EXIF 方向的 JPEG 没有走直通路径"
    );

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
    assert_eq!(
        stream.content, original,
        "PDF 里的流与源文件不一致 —— 为了摆正方向重新编码了"
    );

    // 页面宽高必须按旋转后的显示尺寸互换：源图 1600×900（横），
    // 顺时针转 90 度后显示为竖向，所以页面应当是高大于宽。
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
    assert!(
        h > w,
        "页面是 {w}×{h}，旋转 90 度后应当是竖向；说明宽高没有互换"
    );
}

/// 拍摄时间要能读出来，并且要标明来源。
#[test]
fn exif_capture_time_is_read_and_labelled() {
    let path = write_jpeg_with_exif("dated.jpg", 400, 300, 1, "2024:03:15 14:30:22");
    let t = pdfcore::imaging::read_time(&path).expect("读不到时间");

    assert_eq!(t.source, TimeSource::Captured, "应当识别为 EXIF 拍摄时间");
    assert_eq!(t.when.year, 2024);
    assert_eq!(t.when.month, 3);
    assert_eq!(t.when.day, 15);
    assert_eq!(t.when.hour, 14);
    assert_eq!(t.when.minute, 30);
    assert_eq!(t.when.second, 22);
}

/// PDF 的 /CreationDate 必须是**最早一张**照片的拍摄时间。
#[test]
fn pdf_creation_date_is_the_earliest_capture_time() {
    let late = write_jpeg_with_exif("late.jpg", 400, 300, 1, "2024:06:01 09:00:00");
    let early = write_jpeg_with_exif("early.jpg", 400, 300, 1, "2024:03:15 14:30:22");

    // 故意把晚的排在前面，验证取的是最早值而不是第一个
    let report = images_to_pdf::run(&[late, early], Tier::Lossless, &NoProgress).unwrap();
    let creation = report.value.creation.expect("没有写入创建时间");
    assert_eq!(
        (creation.year, creation.month, creation.day),
        (2024, 3, 15),
        "创建时间应当取最早的那张（2024-03-15），实际 {}",
        creation.display()
    );

    // 时间跨度也要正确
    let (first, last) = report.value.time_span.unwrap();
    assert_eq!((first.month, last.month), (3, 6));

    // 真的写进了 PDF 的 Info 字典
    let doc = lopdf::Document::load_mem(&report.value.pdf).unwrap();
    let info_ref = doc.trailer.get(b"Info").unwrap().as_reference().unwrap();
    let info = doc.get_dictionary(info_ref).unwrap();
    let raw = info.get(b"CreationDate").unwrap().as_str().unwrap();
    let text = String::from_utf8_lossy(raw);
    assert!(
        text.starts_with("D:20240315143022"),
        "PDF 里的 CreationDate 是 {text}，应当是 2024-03-15 14:30:22"
    );
}

/// 无损档绝不改动像素，无论图片多大。
#[test]
fn lossless_tier_never_resamples() {
    // 4000×3000 远超任何 DPI 上限，但无损档不该因此降采样。
    let path = write_jpeg_with_exif("huge.jpg", 4000, 3000, 1, "2024:01:01 00:00:00");
    let original = std::fs::read(&path).unwrap();

    let report = images_to_pdf::run(&[path], Tier::Lossless, &NoProgress).unwrap();
    assert_eq!(
        report.value.fidelity[0].1,
        pdfcore::imaging::Fidelity::Passthrough
    );

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
    assert_eq!(stream.content, original, "无损档改动了图像数据");
    assert_eq!(stream.dict.get(b"Width").unwrap().as_i64().unwrap(), 4000);
}

/// 从 PDF 第一页的内容流里取出 `cm` 的六个操作数。
fn page_matrix(pdf: &[u8]) -> [f32; 6] {
    let doc = lopdf::Document::load_mem(pdf).unwrap();
    let page_id = *doc.get_pages().values().next().unwrap();
    let content = doc.get_and_decode_page_content(page_id).unwrap();
    for op in &content.operations {
        if op.operator == "cm" && op.operands.len() == 6 {
            let mut m = [0.0f32; 6];
            for (i, o) in op.operands.iter().enumerate() {
                m[i] = match o {
                    lopdf::Object::Real(v) => *v,
                    lopdf::Object::Integer(v) => *v as f32,
                    _ => panic!("cm 的操作数不是数字"),
                };
            }
            return m;
        }
    }
    panic!("内容流里没有 cm 操作符");
}

/// 方向必须真的落到内容流的变换矩阵上。
///
/// 这条测试是为一个具体的教训写的：`placement_matrix()` 本身算对了，
/// 但调用点仍在用写死的 `[w,0,0,h,0,0]`。字节级直通的测试照样通过，
/// 因为流里的字节确实没变 —— 变错的是页面上的摆法。
/// 只断言「字节一致」是不够的，必须同时断言「矩阵用对了」。
#[test]
fn orientation_reaches_the_content_stream_matrix() {
    // 方向 6：顺时针旋转 90 度。页面因此是竖向。
    let path = write_jpeg_with_exif("m6.jpg", 1600, 900, 6, "2024:01:01 00:00:00");
    let pdf = images_to_pdf::run(&[path], Tier::Lossless, &NoProgress)
        .unwrap()
        .value
        .pdf;
    let m = page_matrix(&pdf);

    // 旋转 90 度的矩阵形式是 [0, -h, w, 0, 0, h]：对角线为 0，反对角线非 0。
    assert!(
        m[0].abs() < 0.01 && m[3].abs() < 0.01,
        "矩阵 {m:?} 的 a、d 不为零，说明根本没有旋转（很可能仍在用写死的 [w,0,0,h,0,0]）"
    );
    assert!(m[1] < 0.0 && m[2] > 0.0, "旋转方向反了：{m:?}");

    // 方向 1 才应当是那个平凡形式，用作对照。
    let plain = write_jpeg_with_exif("m1.jpg", 1600, 900, 1, "2024:01:01 00:00:00");
    let pdf = images_to_pdf::run(&[plain], Tier::Lossless, &NoProgress)
        .unwrap()
        .value
        .pdf;
    let m = page_matrix(&pdf);
    assert!(
        m[1].abs() < 0.01 && m[2].abs() < 0.01 && m[0] > 0.0 && m[3] > 0.0,
        "无方向标记的图不该被变换：{m:?}"
    );
}

/// 八个方向的矩阵形状各不相同 —— 任何两个相同都意味着有分支写错了。
#[test]
fn all_eight_orientations_produce_distinct_matrices() {
    let mut seen: Vec<[i32; 6]> = Vec::new();
    for o in 1..=8u16 {
        let p = write_jpeg_with_exif(&format!("d{o}.jpg"), 1600, 900, o, "2024:01:01 00:00:00");
        let pdf = images_to_pdf::run(&[p], Tier::Lossless, &NoProgress)
            .unwrap()
            .value
            .pdf;
        // 量化成整数再比，规避浮点抖动
        let m = page_matrix(&pdf).map(|v| v.round() as i32);
        assert!(
            !seen.contains(&m),
            "方向 {o} 的矩阵 {m:?} 与之前某个方向重复了"
        );
        seen.push(m);
    }
}
