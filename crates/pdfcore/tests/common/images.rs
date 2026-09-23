//! 测试用图片：照片状像素、手工拼的 EXIF 段。

use std::path::PathBuf;

/// 造一张「照片状」的图：逐像素都不同，不会被判成截图而走无损分支。
pub fn photo(w: u32, h: u32) -> image::RgbImage {
    image::RgbImage::from_fn(w, h, |x, y| {
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

/// 灰度版的照片状图。
pub fn gray_photo(w: u32, h: u32) -> image::GrayImage {
    image::GrayImage::from_fn(w, h, |x, y| {
        let n = (x
            .wrapping_mul(2_654_435_761)
            .wrapping_add(y.wrapping_mul(40_503))
            >> 7) as u8;
        image::Luma([((x + y) * 255 / (w + h).max(1)) as u8 ^ (n >> 4)])
    })
}

/// 以指定质量编码成 JPEG。
pub fn jpeg_q(img: &image::DynamicImage, quality: u8) -> Vec<u8> {
    let mut buf = Vec::new();
    img.write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(
        &mut buf, quality,
    ))
    .unwrap();
    buf
}

/// 把一个 APPn 段插在 JPEG 的 SOI 之后。
pub fn with_segment(jpeg: &[u8], segment: &[u8]) -> Vec<u8> {
    let mut out = jpeg[..2].to_vec();
    out.extend_from_slice(segment);
    out.extend_from_slice(&jpeg[2..]);
    out
}

fn app1(payload_after_exif_header: &[u8]) -> Vec<u8> {
    let mut payload = b"Exif\x00\x00".to_vec();
    payload.extend_from_slice(payload_after_exif_header);
    let mut app1 = vec![0xFF, 0xE1];
    app1.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    app1.extend_from_slice(&payload);
    app1
}

/// 最小可用的 EXIF（APP1）：IFD0 里放 Orientation，Exif 子 IFD 里放 DateTimeOriginal。
/// 偏移量都相对 TIFF 头起点。
pub fn exif_app1(orientation: u16, datetime: &str) -> Vec<u8> {
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
    app1(&tiff)
}

/// 只有 IFD0 `DateTime`（0x0132，文件最后修改时刻）的 EXIF。
/// 图片编辑软件一保存就会改写它，它不是拍摄时间。
pub fn exif_app1_ifd0_datetime(datetime: &str) -> Vec<u8> {
    assert_eq!(datetime.len(), 19);
    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"MM\x00\x2a");
    tiff.extend_from_slice(&8u32.to_be_bytes());
    tiff.extend_from_slice(&1u16.to_be_bytes());
    tiff.extend_from_slice(&0x0132u16.to_be_bytes());
    tiff.extend_from_slice(&2u16.to_be_bytes());
    tiff.extend_from_slice(&20u32.to_be_bytes());
    tiff.extend_from_slice(&26u32.to_be_bytes());
    tiff.extend_from_slice(&0u32.to_be_bytes());
    debug_assert_eq!(tiff.len(), 26);
    tiff.extend_from_slice(datetime.as_bytes());
    tiff.push(0);
    app1(&tiff)
}

/// 写一张带 EXIF（方向 + 拍摄时间）的照片状 JPEG 到 `target/tmp/<sub>/<name>`。
pub fn write_jpeg_with_exif(
    sub: &str,
    name: &str,
    w: u32,
    h: u32,
    orientation: u16,
    dt: &str,
) -> PathBuf {
    let jpeg = jpeg_q(&image::DynamicImage::ImageRgb8(photo(w, h)), 90);
    let path = super::tmp(sub).join(name);
    std::fs::write(&path, with_segment(&jpeg, &exif_app1(orientation, dt))).unwrap();
    path
}

/// 手工拼一个 `pages` 页的 TIFF，每页 4×4、8 位灰度、不压缩。
/// image crate 只会解第一页 —— 用来验证其余页被丢弃时有没有提示。
pub fn tiff_pages(pages: usize) -> Vec<u8> {
    const W: u16 = 4;
    const ENTRIES: usize = 9;
    let ifd_len = 2 + ENTRIES * 12 + 4;
    let page_len = ifd_len + (W as usize * W as usize);
    let mut out = Vec::new();
    out.extend_from_slice(b"II");
    out.extend_from_slice(&42u16.to_le_bytes());
    out.extend_from_slice(&8u32.to_le_bytes());
    for p in 0..pages {
        let ifd_at = 8 + p * page_len;
        let data_at = ifd_at + ifd_len;
        let next = if p + 1 < pages {
            (ifd_at + page_len) as u32
        } else {
            0
        };
        out.extend_from_slice(&(ENTRIES as u16).to_le_bytes());
        let mut entry = |tag: u16, ty: u16, value: u32| {
            out.extend_from_slice(&tag.to_le_bytes());
            out.extend_from_slice(&ty.to_le_bytes());
            out.extend_from_slice(&1u32.to_le_bytes());
            out.extend_from_slice(&value.to_le_bytes());
        };
        // SHORT 值放在 4 字节字段的低位（小端时即前两个字节）。
        entry(256, 3, W as u32); // ImageWidth
        entry(257, 3, W as u32); // ImageLength
        entry(258, 3, 8); // BitsPerSample
        entry(259, 3, 1); // Compression = none
        entry(262, 3, 1); // PhotometricInterpretation = BlackIsZero
        entry(273, 4, data_at as u32); // StripOffsets
        entry(277, 3, 1); // SamplesPerPixel
        entry(278, 3, W as u32); // RowsPerStrip
        entry(279, 4, (W * W) as u32); // StripByteCounts
        out.extend_from_slice(&next.to_le_bytes());
        out.extend((0..W * W).map(|i| (i as u8).wrapping_mul(16).wrapping_add(p as u8 * 40)));
    }
    out
}
