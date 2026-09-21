//! PDF 压缩。
//!
//! 必须先说清楚这件事的边界：**这四个档位压的是图像**。对一份文字/矢量为主的
//! PDF（Word 导出、LaTeX 生成的那种），能省下来的只有对象流和 xref 流那点结构开销，
//! 通常 0-5%。界面必须如实说明，否则每个文字 PDF 的用户都会来报 bug。

use lopdf::{Document, Object, ObjectId};

use super::placement;
use crate::bail_if_cancelled;
use crate::error::{CoreError, Report, Result, Warning, WarningKind};
use crate::imaging::{quality_of, Tier};
use crate::progress::{Progress, ProgressSink};

pub struct CompressOutcome {
    pub pdf: Vec<u8>,
    pub original_bytes: usize,
    /// 实际被重新编码的图片数。
    pub recompressed: usize,
    /// 试过但保留原样的图片数（重编码后反而更大）。
    pub kept_original: usize,
    /// 跳过没处理的图片数（不支持的色彩空间、蒙版、JPEG2000 等）。
    pub skipped: usize,
    /// 整体护栏触发：输出不比输入小，于是原样返回。
    pub returned_unchanged: bool,
}

impl CompressOutcome {
    pub fn saved_ratio(&self) -> f32 {
        if self.original_bytes == 0 {
            return 0.0;
        }
        1.0 - (self.pdf.len() as f32 / self.original_bytes as f32)
    }

    /// 这份 PDF 基本没有可压的图像。用来在界面上解释「为什么没压下去」。
    pub fn is_mostly_vector(&self) -> bool {
        self.recompressed == 0 && self.kept_original == 0 && self.skipped == 0
    }
}

/// 从页面里收集到的一张待处理图片。先收成 owned 数据，
/// 是因为随后要 `get_object_mut`，不能同时持有对 doc 的不可变借用。
struct ImageEntry {
    id: ObjectId,
    width: u32,
    height: u32,
    filters: Vec<String>,
    color_space: Option<String>,
    bits: u32,
    has_smask: bool,
    is_mask: bool,
    raw: Vec<u8>,
}

pub fn run(
    data: &[u8],
    tier: Tier,
    grayscale: bool,
    sink: &dyn ProgressSink,
) -> Result<Report<CompressOutcome>> {
    let original_bytes = data.len();
    let mut doc =
        Document::load_mem(data).map_err(|e| CoreError::Pdf(format!("无法解析该 PDF：{e}")))?;

    if doc.was_encrypted() || doc.is_encrypted() {
        return Err(CoreError::Unsupported(
            "该 PDF 已加密。请先在其他工具里去掉密码保护再来压缩。".into(),
        ));
    }
    if has_signature(&doc) {
        return Err(CoreError::Unsupported(
            "该 PDF 带有数字签名。任何改写都会让签名失效，因此不做处理。".into(),
        ));
    }

    let mut warnings = Vec::new();
    let quality = quality_of(tier, grayscale);
    let placements = placement::scan(&doc);
    let entries = collect_images(&doc);

    sink.emit(Progress::Started {
        total: entries.len().max(1),
    });

    let mut outcome = CompressOutcome {
        pdf: Vec::new(),
        original_bytes,
        recompressed: 0,
        kept_original: 0,
        skipped: 0,
        returned_unchanged: false,
    };

    if !quality.lossless_only() {
        for (i, entry) in entries.iter().enumerate() {
            bail_if_cancelled!(sink);

            // 每张图独立隔离：一张图的怪色彩空间不该让整份文件失败。
            match try_recompress(entry, &placements, &quality) {
                Ok(Some(new_data)) => {
                    apply(&mut doc, entry, new_data, grayscale);
                    outcome.recompressed += 1;
                }
                Ok(None) => outcome.kept_original += 1,
                Err(reason) => {
                    outcome.skipped += 1;
                    log::debug!("跳过图片 {:?}：{reason}", entry.id);
                }
            }
            sink.emit(Progress::Item {
                done: i + 1,
                total: entries.len(),
                label: format!("图片 {}/{}", i + 1, entries.len()),
            });
        }
    }

    // 结构层优化。这部分在任何档位下都做，也是「无损」档唯一的收益来源。
    // compress() 只作用于没有 /Filter 的流，所以不会把已有的 JPEG 二次 flate。
    doc.compress();
    doc.prune_objects();
    doc.renumber_objects();

    let mut buf = Vec::with_capacity(original_bytes);
    doc.save_to(&mut buf)
        .map_err(|e| CoreError::Pdf(format!("写出 PDF 失败：{e}")))?;

    // 再试一次现代格式（对象流 + 交叉引用流），对对象多的文件通常还能再省一截。
    let mut modern = Vec::with_capacity(buf.len());
    if doc.save_modern(&mut modern).is_ok() && modern.len() < buf.len() {
        buf = modern;
    }

    // 整体护栏：压完反而更大就原样返回。这条把最糟的结果变成一句诚实的说明。
    if buf.len() >= original_bytes {
        outcome.returned_unchanged = true;
        outcome.pdf = data.to_vec();
        warnings.push(Warning::new(
            WarningKind::ImageKeptOriginal,
            "该 PDF 已经过高度压缩，继续处理只会变大，因此原样返回。",
        ));
    } else {
        outcome.pdf = buf;
    }

    if outcome.kept_original > 0 {
        warnings.push(Warning::new(
            WarningKind::ImageKeptOriginal,
            format!(
                "{} 张图片重新编码后反而更大，已保留原图",
                outcome.kept_original
            ),
        ));
    }
    if outcome.skipped > 0 {
        warnings.push(Warning::new(
            WarningKind::ImageKeptOriginal,
            format!(
                "{} 张图片使用了本版本不处理的格式（JPEG2000、位图蒙版、特殊色彩空间等），已原样保留",
                outcome.skipped
            ),
        ));
    }

    Ok(Report::with(outcome, warnings))
}

fn has_signature(doc: &Document) -> bool {
    doc.objects.values().any(|o| {
        o.as_dict()
            .ok()
            .and_then(|d| d.get(b"Type").ok())
            .and_then(|t| t.as_name().ok())
            == Some(b"Sig".as_ref())
    })
}

fn collect_images(doc: &Document) -> Vec<ImageEntry> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();

    for (_, page_id) in doc.get_pages() {
        let Ok(images) = doc.get_page_images(page_id) else {
            continue;
        };
        for img in images {
            if !seen.insert(img.id) {
                continue; // 同一张图被多页引用，只处理一次
            }
            let dict = img.origin_dict;
            out.push(ImageEntry {
                id: img.id,
                width: img.width.max(0) as u32,
                height: img.height.max(0) as u32,
                filters: img.filters.unwrap_or_default(),
                color_space: img.color_space,
                bits: img.bits_per_component.unwrap_or(8).max(0) as u32,
                has_smask: dict.get(b"SMask").is_ok() || dict.get(b"Mask").is_ok(),
                is_mask: dict
                    .get(b"ImageMask")
                    .and_then(Object::as_bool)
                    .unwrap_or(false),
                raw: img.content.to_vec(),
            });
        }
    }
    out
}

/// 尝试重新编码一张图。
///
/// - `Ok(Some(bytes))`：编码成功且更小，应当替换。
/// - `Ok(None)`：编码成功但不比原来小，保留原图。
/// - `Err(reason)`：这张图不在本版本处理范围内，原样跳过。
fn try_recompress(
    entry: &ImageEntry,
    placements: &placement::Placements,
    quality: &crate::imaging::ImageQuality,
) -> std::result::Result<Option<Vec<u8>>, String> {
    if entry.is_mask || entry.bits != 8 {
        // 位图蒙版是 1 位模板，CCITT/JBIG2 已经比我们能做的任何编码都小。
        return Err("位图蒙版或非 8 位样本".into());
    }
    if entry.has_smask {
        // 主图降采样了而蒙版没有，就会错位；蒙版走 JPEG 又会在边缘产生光晕。
        // 带透明的图在扫描件里几乎从不是体积大头，跳过是划算的。
        return Err("带透明蒙版".into());
    }
    if entry.filters.iter().any(|f| f == "JPXDecode") {
        return Err("JPEG2000，无法解码".into());
    }
    // 小图（图标、项目符号、签名章）不碰：DPI 估算对它们本来就不准，
    // 而且它们从来不是体积问题。
    if entry.width < 200 || entry.height < 200 {
        return Err("尺寸过小，不值得处理".into());
    }

    let components = match entry.color_space.as_deref() {
        Some("DeviceGray") | Some("CalGray") => 1,
        Some("DeviceRGB") | Some("CalRGB") => 3,
        // ICCBased 在字典里是个数组，PdfImage 给不出名字。对 DCTDecode 我们能
        // 从 JPEG 头里拿到真实分量数，所以留给下面判断。
        _ if entry.filters.iter().any(|f| f == "DCTDecode") => 0,
        other => return Err(format!("不处理的色彩空间：{other:?}")),
    };

    // 有效 DPI：优先用内容流里扫到的实际放置尺寸；扫不到就退化为「整页铺满」。
    let (place_w, place_h) = placements.get(&entry.id).copied().unwrap_or((595.0, 842.0));

    let target = quality.downscale_target(entry.width, entry.height, place_w, place_h);

    // 代际损失护栏：已经是 JPEG、又不需要重采样、也不转灰度，那就没有任何理由重编码。
    // 重新量化一次确实能再挤掉几个百分点，但那几个百分点是拿画质换的 ——
    // 同一份文件压两次就会肉眼可见地糊。这种情况下「什么都不做」才是正确答案。
    let already_jpeg = entry.filters.iter().any(|f| f == "DCTDecode");
    if already_jpeg && target.is_none() && !quality.grayscale {
        return Ok(None);
    }

    let img = decode(entry, components)?;
    let img = match target {
        Some((w, h)) => crate::imaging::resize_image(&img, w, h).map_err(|e| e.to_string())?,
        None => img,
    };

    let encoded = crate::imaging::encode_jpeg_image(&img, quality.jpeg_quality, quality.grayscale)
        .map_err(|e| e.to_string())?;

    // 核心护栏：不比原来小就不换。对一份已经 q60 的 JPEG 用 q85 重编，
    // 结果是**又大又差**，这条判断是不可商量的。
    Ok((encoded.len() < entry.raw.len()).then_some(encoded))
}

fn decode(entry: &ImageEntry, components: u8) -> std::result::Result<image::DynamicImage, String> {
    if entry.filters.iter().any(|f| f == "DCTDecode") {
        return image::load_from_memory_with_format(&entry.raw, image::ImageFormat::Jpeg)
            .map_err(|e| format!("JPEG 解码失败：{e}"));
    }
    if entry.filters.is_empty() || entry.filters.iter().all(|f| f == "FlateDecode") {
        let expected = entry.width as usize * entry.height as usize * components as usize;
        if components == 0 || entry.raw.len() != expected {
            return Err(format!(
                "原始样本长度 {} 与 {}×{}×{} 不符",
                entry.raw.len(),
                entry.width,
                entry.height,
                components
            ));
        }
        return match components {
            1 => image::GrayImage::from_raw(entry.width, entry.height, entry.raw.clone())
                .map(image::DynamicImage::ImageLuma8)
                .ok_or_else(|| "灰度样本装载失败".to_string()),
            3 => image::RgbImage::from_raw(entry.width, entry.height, entry.raw.clone())
                .map(image::DynamicImage::ImageRgb8)
                .ok_or_else(|| "RGB 样本装载失败".to_string()),
            n => Err(format!("不支持 {n} 个分量")),
        };
    }
    Err(format!("不处理的 filter 组合：{:?}", entry.filters))
}

/// 把新的 JPEG 数据写回对象，只改必要的键。
///
/// 刻意不重建整个字典：`/Decode`、`/Intent`、`/Interpolate` 以及任何我们不认识的键
/// 都必须原样保留 —— 那些是我们看不懂但阅读器可能需要的信息。
fn apply(doc: &mut Document, entry: &ImageEntry, data: Vec<u8>, grayscale: bool) {
    let Ok(obj) = doc.get_object_mut(entry.id) else {
        return;
    };
    let Object::Stream(stream) = obj else { return };

    let (w, h) = image_dims(&data).unwrap_or((entry.width, entry.height));

    stream.set_plain_content(data);
    stream
        .dict
        .set("Filter", Object::Name(b"DCTDecode".to_vec()));
    stream.dict.set("Width", w as i64);
    stream.dict.set("Height", h as i64);
    stream.dict.set("BitsPerComponent", 8i64);
    // 重编码后数据已不再是原来的 filter 链，DecodeParms 必须去掉，否则阅读器会按
    // 旧参数去解一段新数据。
    stream.dict.remove(b"DecodeParms");
    if grayscale {
        stream
            .dict
            .set("ColorSpace", Object::Name(b"DeviceGray".to_vec()));
    }
    // 没转灰度时保留原 /ColorSpace：分量数没变，ICCBased 之类的引用继续有效。
    stream.allows_compression = false; // 已经是 DCTDecode，别再套一层 flate
}

fn image_dims(jpeg: &[u8]) -> Option<(u32, u32)> {
    crate::imaging::probe::jpeg_info(jpeg).map(|i| (i.width, i.height))
}
