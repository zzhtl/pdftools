//! Phase 5 验收：PDF 压缩。
//!
//! 这里最值钱的不是「能压小」，而是那几条护栏：压不动的时候要老老实实
//! 把原文件还回去，而不是产出一个又大又糊的东西。

mod common;

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
    images_to_pdf::run(&[path], Tier::Lossless, &Default::default(), &NoProgress)
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

// ---------------------------------------------------------------- 色彩空间与 filter

/// 造一份只含一张整页图片的 PDF。图像字典的 ColorSpace、Filter 等由调用方补全。
fn pdf_with_image(
    w: u32,
    h: u32,
    data: &[u8],
    dict: impl FnOnce(&mut pdf_writer::writers::ImageXObject),
) -> Vec<u8> {
    use pdf_writer::{Content, Finish, Name, Pdf, Rect, Ref};
    let (catalog, pages, page, content, img) = (
        Ref::new(1),
        Ref::new(2),
        Ref::new(3),
        Ref::new(4),
        Ref::new(5),
    );
    let mut pdf = Pdf::new();
    pdf.catalog(catalog).pages(pages);
    pdf.pages(pages).kids([page]).count(1);
    {
        let mut p = pdf.page(page);
        p.media_box(Rect::new(0.0, 0.0, 595.0, 842.0));
        p.parent(pages);
        p.contents(content);
        p.resources().x_objects().pair(Name(b"Im0"), img);
        p.finish();
    }
    let mut c = Content::new();
    c.save_state();
    c.transform([595.0, 0.0, 0.0, 842.0, 0.0, 0.0]);
    c.x_object(Name(b"Im0"));
    c.restore_state();
    pdf.stream(content, &c.finish());
    {
        let mut x = pdf.image_xobject(img, data);
        x.width(w as i32);
        x.height(h as i32);
        x.bits_per_component(8);
        dict(&mut x);
        x.finish();
    }
    pdf.finish()
}

fn zlib(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    enc.write_all(data).unwrap();
    enc.finish().unwrap()
}

/// 色彩空间声明了几个分量。
fn declared_components(doc: &lopdf::Document, dict: &lopdf::Dictionary) -> Option<usize> {
    let cs = dict.get(b"ColorSpace").ok()?;
    let (_, cs) = doc.dereference(cs).ok()?;
    match cs {
        lopdf::Object::Name(n) => match n.as_slice() {
            b"DeviceGray" => Some(1),
            b"DeviceRGB" => Some(3),
            b"DeviceCMYK" => Some(4),
            _ => None,
        },
        lopdf::Object::Array(a) if a.first()?.as_name().ok()? == b"ICCBased" => {
            let (_, icc) = doc.dereference(a.get(1)?).ok()?;
            Some(icc.as_stream().ok()?.dict.get(b"N").ok()?.as_i64().ok()? as usize)
        }
        _ => None,
    }
}

/// 每个图像对象的实际数据必须与它声明的色彩空间对得上：
/// JPEG 的分量数、原始样本的长度。对不上的 PDF 在阅读器里是花屏或空白。
fn assert_images_consistent(pdf: &[u8]) {
    let doc = lopdf::Document::load_mem(pdf).unwrap();
    for (id, obj) in &doc.objects {
        let lopdf::Object::Stream(s) = obj else {
            continue;
        };
        if s.dict.get(b"Subtype").and_then(lopdf::Object::as_name).ok() != Some(b"Image".as_ref()) {
            continue;
        }
        let comps = declared_components(&doc, &s.dict)
            .unwrap_or_else(|| panic!("图像 {id:?} 的色彩空间无法识别"));
        let dct = s
            .filters()
            .map(|f| f.iter().any(|n| *n == b"DCTDecode"))
            .unwrap_or(false);
        if dct {
            let info = pdfcore::imaging::probe::jpeg_info(&s.content)
                .unwrap_or_else(|| panic!("图像 {id:?} 的 JPEG 数据无法解析"));
            assert_eq!(
                info.components as usize, comps,
                "图像 {id:?}：JPEG 有 {} 个分量，/ColorSpace 却声明 {comps} 个",
                info.components
            );
        } else {
            let w = s.dict.get(b"Width").unwrap().as_i64().unwrap() as usize;
            let h = s.dict.get(b"Height").unwrap().as_i64().unwrap() as usize;
            let raw = s.decompressed_content().unwrap();
            assert_eq!(raw.len(), w * h * comps, "图像 {id:?} 的样本长度不对");
        }
    }
}

/// 整页渲染后的平均亮度。压缩前后应当几乎不变 —— 分量数写错时画面会面目全非。
fn mean_luma(pdf: &[u8]) -> f64 {
    let pages = common::raster::render_gray(pdf, 18.0);
    let g = &pages[0];
    g.px.iter().map(|v| *v as f64).sum::<f64>() / g.px.len() as f64
}

#[test]
fn flate_images_are_recompressed_too() {
    // PNG predictor 15：每行前面一个过滤类型字节（0 = 不过滤）。
    // 真实 PDF 里的 Flate 图大多带 predictor，lopdf 返回的是未解压的原始流 ——
    // 曾经因此每张 Flate 图都被当成「长度不对」跳过，一张都没压。
    let (w, h) = (1600u32, 2000u32);
    let rgb = common::images::photo(w, h).into_raw();
    let mut rows = Vec::with_capacity(rgb.len() + h as usize);
    for r in rgb.chunks(w as usize * 3) {
        rows.push(0);
        rows.extend_from_slice(r);
    }
    let original = pdf_with_image(w, h, &zlib(&rows), |x| {
        x.color_space().device_rgb();
        x.filter(pdf_writer::Filter::FlateDecode);
        x.decode_parms()
            .predictor(pdf_writer::types::Predictor::PngOptimum)
            .colors(3)
            .columns(w as i32)
            .bits_per_component(8);
    });

    let report = compress::run(&original, Tier::Extreme, false, &NoProgress).unwrap();
    let out = &report.value;
    assert_eq!(
        out.recompressed, 1,
        "Flate 图没有被处理：{:?}",
        report.warnings
    );
    assert!(out.pdf.len() < original.len());
    assert_images_consistent(&out.pdf);
    let (before, after) = (mean_luma(&original), mean_luma(&out.pdf));
    assert!(
        (before - after).abs() < 8.0,
        "压缩后画面变了：平均亮度 {before:.1} → {after:.1}"
    );
}

#[test]
fn grayscale_jpeg_stays_grayscale() {
    // 灰度扫描件很常见。曾经解码成灰度、却按三分量 RGB 重新编码，
    // 字典里仍写着 /DeviceGray —— 阅读器按一个分量去解三个分量的数据。
    let (w, h) = (2400u32, 3200u32);
    let jpeg = common::images::jpeg_q(
        &image::DynamicImage::ImageLuma8(common::images::gray_photo(w, h)),
        92,
    );
    let original = pdf_with_image(w, h, &jpeg, |x| {
        x.color_space().device_gray();
        x.filter(pdf_writer::Filter::DctDecode);
    });

    let report = compress::run(&original, Tier::Extreme, false, &NoProgress).unwrap();
    assert_eq!(report.value.recompressed, 1);
    assert_images_consistent(&report.value.pdf);
    let (before, after) = (mean_luma(&original), mean_luma(&report.value.pdf));
    assert!(
        (before - after).abs() < 8.0,
        "压缩后画面变了：平均亮度 {before:.1} → {after:.1}"
    );
}

#[test]
fn cmyk_jpeg_is_kept_and_explained() {
    // CMYK 的 JPEG（印刷流程产出的扫描件常见）解码后变成 RGB，
    // 重编码再写回 /DeviceCMYK 就是花屏。本版本不处理，必须原样保留并说清楚。
    let (w, h) = (2400u32, 3200u32);
    let cmyk: Vec<u8> = common::images::photo(w, h)
        .pixels()
        .flat_map(|p| [p.0[0], p.0[1], p.0[2], 255 - p.0[0].max(p.0[1]).max(p.0[2])])
        .collect();
    let mut jpeg = Vec::new();
    jpeg_encoder::Encoder::new(&mut jpeg, 90)
        .encode(&cmyk, w as u16, h as u16, jpeg_encoder::ColorType::Cmyk)
        .unwrap();
    let original = pdf_with_image(w, h, &jpeg, |x| {
        x.color_space().device_cmyk();
        x.filter(pdf_writer::Filter::DctDecode);
    });

    let report = compress::run(&original, Tier::Extreme, false, &NoProgress).unwrap();
    assert_eq!(report.value.recompressed, 0, "CMYK 图不该被重编码");
    assert_eq!(
        image_streams(&original),
        image_streams(&report.value.pdf),
        "CMYK 图的数据必须原样保留"
    );
    assert!(
        report.warnings.iter().any(|w| w.detail.contains("CMYK")),
        "应当说明 CMYK 图被原样保留了：{:?}",
        report.warnings
    );
    assert_images_consistent(&report.value.pdf);
}
