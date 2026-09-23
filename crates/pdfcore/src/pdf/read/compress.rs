//! PDF 压缩。
//!
//! 必须先说清楚这件事的边界：**这四个档位压的是图像**。对一份文字/矢量为主的
//! PDF（Word 导出、LaTeX 生成的那种），能省下来的只有对象流和 xref 流那点结构开销，
//! 通常 0-5%。界面必须如实说明，否则每个文字 PDF 的用户都会来报 bug。

use lopdf::{Document, Object, ObjectId};
use rayon::prelude::*;

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
    /// 跳过没处理的图片数（CMYK、蒙版、JPEG2000 等）。具体类别见警告。
    pub skipped: usize,
    /// 已经是合适形态、不需要动的图片数（不需要降采样的 JPEG）。
    pub already_optimal: usize,
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

    /// 这份 PDF 基本没有可压的图像（图标这类小图不算）。
    /// 用来在界面上解释「为什么没压下去」。
    pub fn is_mostly_vector(&self) -> bool {
        self.recompressed == 0
            && self.kept_original == 0
            && self.skipped == 0
            && self.already_optimal == 0
    }
}

/// 从页面里收集到的一张待处理图片。只记元数据，像素等处理到它时再按需解码 ——
/// 预先把所有图片数据复制一份，扫描件 PDF 的峰值内存会翻倍。
struct ImageEntry {
    id: ObjectId,
    width: u32,
    height: u32,
    filters: Vec<String>,
    model: std::result::Result<Model, Skip>,
    bits: u32,
    has_smask: bool,
    is_mask: bool,
    has_decode: bool,
    /// 流的存储长度（压缩态）。「不比原来大」就比它。
    stored_len: usize,
}

/// 颜色模型。只处理灰度与 RGB：重编码后分量数不变，原来的 `/ColorSpace`
/// （包括 ICCBased 这类引用）继续有效，不必重写。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Model {
    Gray,
    Rgb,
}

impl Model {
    fn components(self) -> usize {
        match self {
            Model::Gray => 1,
            Model::Rgb => 3,
        }
    }
}

/// 本版本不处理、原样保留的原因。按类别汇总进警告。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Skip {
    Cmyk,
    SpecialColorSpace,
    Mask,
    Transparency,
    Jpeg2000,
    OtherFilter,
    DecodeArray,
    Undecodable,
}

impl Skip {
    fn label(self) -> &'static str {
        match self {
            // CMYK 的 JPEG 解码后是 RGB，写回去就得改色彩空间并处理 Adobe 的反相约定，
            // 判错了输出是偏色而不是报错，所以不碰。
            Skip::Cmyk => "CMYK 图片",
            Skip::SpecialColorSpace => "索引色、专色等特殊色彩空间的图片",
            Skip::Mask => "位图蒙版或非 8 位的图片",
            Skip::Transparency => "带透明蒙版的图片",
            Skip::Jpeg2000 => "JPEG2000 图片",
            Skip::OtherFilter => "CCITT、JBIG2 等特殊编码的图片",
            Skip::DecodeArray => "带解码映射（/Decode）、无法转灰度的图片",
            Skip::Undecodable => "无法解码的图片",
        }
    }
}

enum Decision {
    /// 重编码后更小，替换。
    Replace(Replacement),
    /// 试过了，不比原来小，保留原图。
    KeptOriginal,
    /// 已经是 JPEG 且不需要降采样：重编码只会拿画质换那几个百分点。
    AlreadyOptimal,
    /// 图标、项目符号这类小图，从来不是体积问题。
    Tiny,
    Skip(Skip),
}

struct Replacement {
    data: Vec<u8>,
    /// true = DCTDecode（JPEG），false = FlateDecode（无损）。
    jpeg: bool,
    width: u32,
    height: u32,
    /// 新数据是单分量。
    gray: bool,
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
    let scan = placement::scan(&doc);
    let entries = collect_images(&doc, &scan.images);

    sink.emit(Progress::Started {
        total: entries.len().max(1),
    });

    let mut outcome = CompressOutcome {
        pdf: Vec::new(),
        original_bytes,
        recompressed: 0,
        kept_original: 0,
        skipped: 0,
        already_optimal: 0,
        returned_unchanged: false,
    };
    let mut skips: std::collections::BTreeMap<Skip, usize> = Default::default();

    if !quality.lossless_only() {
        // 几张图同时解码、缩放、重编码（只读文档），得出的新数据再依次写回去。
        let pool = crate::imaging::worker_pool()?;
        let window = pool.current_num_threads() * 2;
        let mut done = 0;
        for chunk in entries.chunks(window) {
            bail_if_cancelled!(sink);
            let decisions: Vec<Option<Decision>> = pool.install(|| {
                chunk
                    .par_iter()
                    .map(|entry| {
                        // 每张图独立隔离：一张图的怪色彩空间不该让整份文件失败。
                        (!sink.is_cancelled())
                            .then(|| try_recompress(&doc, entry, &scan.placements, &quality))
                    })
                    .collect()
            });
            bail_if_cancelled!(sink);
            for (entry, decision) in chunk.iter().zip(decisions) {
                match decision {
                    Some(Decision::Replace(r)) => {
                        apply(&mut doc, entry, r);
                        outcome.recompressed += 1;
                    }
                    Some(Decision::KeptOriginal) => outcome.kept_original += 1,
                    Some(Decision::AlreadyOptimal) => outcome.already_optimal += 1,
                    Some(Decision::Tiny) | None => {}
                    Some(Decision::Skip(reason)) => {
                        outcome.skipped += 1;
                        *skips.entry(reason).or_default() += 1;
                    }
                }
                done += 1;
                sink.emit(Progress::Item {
                    done,
                    total: entries.len(),
                    label: format!("图片 {done}/{}", entries.len()),
                });
            }
        }
    }

    // 内容被改写过，/ModDate 要如实更新；但 /CreationDate 必须原样保留 ——
    // 那记的是文档形成的时间，压缩不该改变它。
    touch_mod_date(&mut doc);

    // 结构层优化。这部分在任何档位下都做，也是「无损」档唯一的收益来源。
    // compress() 只作用于没有 /Filter 的流，所以不会把已有的 JPEG 二次 flate。
    doc.compress();
    doc.prune_objects();
    doc.renumber_objects();

    // 按现代格式（对象流 + 交叉引用流）写，对象多的文件能再省一截；写不出来才退回
    // 传统格式。只写一遍：以前两种各写一遍取小的，扫描件的输出要在内存里同时放两份。
    let mut buf = Vec::with_capacity(original_bytes);
    if doc.save_modern(&mut buf).is_err() {
        buf.clear();
        doc.save_to(&mut buf)
            .map_err(|e| CoreError::Pdf(format!("写出 PDF 失败：{e}")))?;
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
                "{} 张图片重新编码后不比原来小，已保留原图",
                outcome.kept_original
            ),
        ));
    }
    if !skips.is_empty() {
        // 按类别说清楚：用户看得懂「2 张 CMYK 图片」，看不懂「不处理的格式」。
        let parts: Vec<String> = skips
            .iter()
            .map(|(reason, n)| {
                let label = reason.label();
                let sep = if label.starts_with(|c: char| c.is_ascii()) {
                    " "
                } else {
                    ""
                };
                format!("{n} 张{sep}{label}")
            })
            .collect();
        warnings.push(Warning::new(
            WarningKind::ImageKeptOriginal,
            format!("{}本版本不处理，已原样保留", parts.join("、")),
        ));
    }

    Ok(Report::with(outcome, warnings))
}

/// 更新 `/Info` 里的 `/ModDate`。没有 Info 字典就什么都不做 ——
/// 为此新建一个反而是在给文件添加原本没有的元数据。
fn touch_mod_date(doc: &mut Document) {
    let Ok(info_ref) = doc.trailer.get(b"Info").and_then(Object::as_reference) else {
        return;
    };
    let stamp = crate::timestamp::Timestamp::now().to_pdf_string();
    if let Ok(Object::Dictionary(d)) = doc.get_object_mut(info_ref) {
        d.set("ModDate", Object::string_literal(stamp));
    }
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

/// 扫描找到的图片（对象号）逐张读出元数据。
fn collect_images(doc: &Document, ids: &[ObjectId]) -> Vec<ImageEntry> {
    ids.iter()
        .filter_map(|&id| {
            let stream = doc.get_object(id).and_then(Object::as_stream).ok()?;
            let dict = &stream.dict;
            // 宽高之类可能写成间接引用。
            let int = |key: &[u8]| {
                dict.get(key)
                    .ok()
                    .and_then(|o| doc.dereference(o).ok())
                    .and_then(|(_, o)| o.as_i64().ok())
            };
            let filters: Vec<String> = stream
                .filters()
                .map(|f| {
                    f.iter()
                        .map(|n| String::from_utf8_lossy(n).into_owned())
                        .collect()
                })
                .unwrap_or_default();
            Some(ImageEntry {
                id,
                width: int(b"Width")?.max(0) as u32,
                height: int(b"Height")?.max(0) as u32,
                model: color_model(doc, dict, &filters, &stream.content),
                bits: int(b"BitsPerComponent").unwrap_or(8).max(0) as u32,
                has_smask: dict.get(b"SMask").is_ok() || dict.get(b"Mask").is_ok(),
                is_mask: dict
                    .get(b"ImageMask")
                    .and_then(Object::as_bool)
                    .unwrap_or(false),
                has_decode: dict.get(b"Decode").is_ok(),
                stored_len: stream.content.len(),
                filters,
            })
        })
        .collect()
}

/// 定出颜色模型。字典里声明的色彩空间与 JPEG 头里的实际分量数都要看：
/// DCT 流的 `/ColorSpace` 可能是 ICCBased 数组，也可能干脆缺省。
fn color_model(
    doc: &Document,
    dict: &lopdf::Dictionary,
    filters: &[String],
    data: &[u8],
) -> std::result::Result<Model, Skip> {
    let declared = match dict.get(b"ColorSpace").ok() {
        None => None,
        Some(cs) => {
            let (_, cs) = doc.dereference(cs).map_err(|_| Skip::SpecialColorSpace)?;
            Some(declared_components(doc, cs)?)
        }
    };
    let components = if filters.iter().any(|f| f == "DCTDecode") {
        let info = crate::imaging::probe::jpeg_info(data).ok_or(Skip::Undecodable)?;
        let n = info.components as usize;
        // 声明与数据对不上的图，原文件本身就是坏的，不碰。
        if declared.is_some_and(|d| d != n) {
            return Err(Skip::Undecodable);
        }
        n
    } else {
        declared.ok_or(Skip::SpecialColorSpace)?
    };
    match components {
        1 => Ok(Model::Gray),
        3 => Ok(Model::Rgb),
        4 => Err(Skip::Cmyk),
        _ => Err(Skip::SpecialColorSpace),
    }
}

fn declared_components(doc: &Document, cs: &Object) -> std::result::Result<usize, Skip> {
    match cs {
        Object::Name(n) => match n.as_slice() {
            b"DeviceGray" | b"CalGray" => Ok(1),
            b"DeviceRGB" | b"CalRGB" => Ok(3),
            b"DeviceCMYK" => Ok(4),
            _ => Err(Skip::SpecialColorSpace),
        },
        Object::Array(a) => match a.first().and_then(|o| o.as_name().ok()) {
            Some(b"CalGray") => Ok(1),
            Some(b"CalRGB") => Ok(3),
            Some(b"ICCBased") => {
                let n = a
                    .get(1)
                    .and_then(|o| doc.dereference(o).ok())
                    .and_then(|(_, o)| o.as_stream().ok())
                    .and_then(|s| s.dict.get(b"N").ok())
                    .and_then(|n| n.as_i64().ok());
                match n {
                    Some(n @ (1 | 3 | 4)) => Ok(n as usize),
                    _ => Err(Skip::SpecialColorSpace),
                }
            }
            _ => Err(Skip::SpecialColorSpace),
        },
        _ => Err(Skip::SpecialColorSpace),
    }
}

/// 决定一张图怎么处理。
fn try_recompress(
    doc: &Document,
    entry: &ImageEntry,
    placements: &placement::Placements,
    quality: &crate::imaging::ImageQuality,
) -> Decision {
    if entry.is_mask || entry.bits != 8 {
        // 位图蒙版是 1 位模板，CCITT/JBIG2 已经比我们能做的任何编码都小。
        return Decision::Skip(Skip::Mask);
    }
    if entry.has_smask {
        // 主图降采样了而蒙版没有，就会错位；蒙版走 JPEG 又会在边缘产生光晕。
        // 带透明的图在扫描件里几乎从不是体积大头，跳过是划算的。
        return Decision::Skip(Skip::Transparency);
    }
    if entry.filters.iter().any(|f| f == "JPXDecode") {
        return Decision::Skip(Skip::Jpeg2000);
    }
    // 小图（图标、项目符号、签名章）不碰：DPI 估算对它们本来就不准，
    // 而且它们从来不是体积问题。
    if entry.width < 200 || entry.height < 200 {
        return Decision::Tiny;
    }
    let model = match entry.model {
        Ok(m) => m,
        Err(reason) => return Decision::Skip(reason),
    };
    // 转灰度会改变分量数，而 /Decode 数组的长度随分量数而定，改完就对不上了。
    let to_gray = quality.grayscale && model == Model::Rgb;
    if to_gray && entry.has_decode {
        return Decision::Skip(Skip::DecodeArray);
    }

    // 有效 DPI：优先用内容流里扫到的实际放置尺寸；扫不到就退化为「整页铺满」。
    let (place_w, place_h) = placements.get(&entry.id).copied().unwrap_or((595.0, 842.0));
    let target = quality.downscale_target(entry.width, entry.height, place_w, place_h);

    // 代际损失护栏：已经是 JPEG、又不需要重采样、也不转灰度，那就没有任何理由重编码。
    // 重新量化一次确实能再挤掉几个百分点，但那几个百分点是拿画质换的 ——
    // 同一份文件压两次就会肉眼可见地糊。这种情况下「什么都不做」才是正确答案。
    let dct = entry.filters.iter().any(|f| f == "DCTDecode");
    if dct && target.is_none() && !to_gray {
        return Decision::AlreadyOptimal;
    }

    let Some(stream) = doc
        .get_object(entry.id)
        .ok()
        .and_then(|o| o.as_stream().ok())
    else {
        return Decision::Skip(Skip::Undecodable);
    };
    let img = match decode(stream, entry, model, dct) {
        Ok(img) => img,
        Err(reason) => return Decision::Skip(reason),
    };
    let img = match target {
        Some((w, h)) => match crate::imaging::resize_image(&img, w, h) {
            Ok(img) => img,
            Err(_) => return Decision::Skip(Skip::Undecodable),
        },
        None => img,
    };

    let gray = to_gray || model == Model::Gray;
    let (w, h) = (img.width(), img.height());
    let Ok(jpeg) = crate::imaging::encode_jpeg_image(&img, quality.jpeg_quality, gray) else {
        return Decision::Skip(Skip::Undecodable);
    };

    // 原本就是无损存储的图，多半是截图、线稿、文字扫描：两种编码各估一次，按实测体积选，
    // 与「图片转 PDF」是同一条规则 —— 一律转 JPEG 会在文字边缘压出振铃。
    let candidate = if dct {
        Replacement {
            data: jpeg,
            jpeg: true,
            width: w,
            height: h,
            gray,
        }
    } else {
        let raw = if gray {
            img.to_luma8().into_raw()
        } else {
            img.to_rgb8().into_raw()
        };
        let components = if gray { 1 } else { 3 };
        let flate_est =
            crate::imaging::estimate_flate_len(&raw, w as usize * components, components);
        if flate_est as f32 <= jpeg.len() as f32 * crate::imaging::LOSSLESS_TOLERANCE {
            Replacement {
                data: crate::pdf::writer::image::flate_image(&raw, w, components),
                jpeg: false,
                width: w,
                height: h,
                gray,
            }
        } else {
            Replacement {
                data: jpeg,
                jpeg: true,
                width: w,
                height: h,
                gray,
            }
        }
    };

    // 核心护栏：不比原来小就不换。对一份已经 q60 的 JPEG 用 q85 重编，
    // 结果是**又大又差**，这条判断是不可商量的。
    if candidate.data.len() < entry.stored_len {
        Decision::Replace(candidate)
    } else {
        Decision::KeptOriginal
    }
}

fn decode(
    stream: &lopdf::Stream,
    entry: &ImageEntry,
    model: Model,
    dct: bool,
) -> std::result::Result<image::DynamicImage, Skip> {
    if dct {
        // DCTDecode 前面再套一层 Flate 之类的组合极少见，不处理。
        if entry.filters.len() != 1 {
            return Err(Skip::OtherFilter);
        }
        return image::load_from_memory_with_format(&stream.content, image::ImageFormat::Jpeg)
            .map_err(|_| Skip::Undecodable);
    }

    const LOSSLESS: [&str; 5] = [
        "FlateDecode",
        "LZWDecode",
        "RunLengthDecode",
        "ASCII85Decode",
        "ASCIIHexDecode",
    ];
    if !entry.filters.iter().all(|f| LOSSLESS.contains(&f.as_str())) {
        return Err(Skip::OtherFilter);
    }
    let expected = entry.width as usize * entry.height as usize * model.components();
    // 带上限地解压：predictor 每行多一个字节，再留点余量；
    // 恶意构造的流（解压炸弹）会在这里被拒绝，而不是把内存吃光。
    let limit = expected + entry.height as usize + 64;
    let samples = stream
        .decompressed_content_with_limit(limit)
        .map_err(|_| Skip::Undecodable)?;
    if samples.len() != expected {
        return Err(Skip::Undecodable);
    }
    match model {
        Model::Gray => image::GrayImage::from_raw(entry.width, entry.height, samples)
            .map(image::DynamicImage::ImageLuma8),
        Model::Rgb => image::RgbImage::from_raw(entry.width, entry.height, samples)
            .map(image::DynamicImage::ImageRgb8),
    }
    .ok_or(Skip::Undecodable)
}

/// 把新数据写回对象，只改必要的键。
///
/// 刻意不重建整个字典：`/Decode`、`/Intent`、`/Interpolate` 以及任何我们不认识的键
/// 都必须原样保留 —— 那些是我们看不懂但阅读器可能需要的信息。
fn apply(doc: &mut Document, entry: &ImageEntry, r: Replacement) {
    let Ok(Object::Stream(stream)) = doc.get_object_mut(entry.id) else {
        return;
    };
    // set_plain_content 会一并去掉旧的 /Filter 与 /DecodeParms ——
    // 数据已不再是原来的 filter 链，留着旧参数阅读器会拿它去解新数据。
    stream.set_plain_content(r.data);
    let filter: &[u8] = if r.jpeg { b"DCTDecode" } else { b"FlateDecode" };
    stream.dict.set("Filter", Object::Name(filter.to_vec()));
    stream.dict.set("Width", r.width as i64);
    stream.dict.set("Height", r.height as i64);
    stream.dict.set("BitsPerComponent", 8i64);
    // 无损的数据按 PNG 预测压的（见 `flate_image`），解码要知道每行多宽、几个分量。
    if !r.jpeg {
        let mut parms = lopdf::Dictionary::new();
        parms.set("Predictor", 15i64);
        parms.set("Colors", if r.gray { 1i64 } else { 3 });
        parms.set("BitsPerComponent", 8i64);
        parms.set("Columns", r.width as i64);
        stream.dict.set("DecodeParms", Object::Dictionary(parms));
    }
    // 分量数只在「彩色转灰度」时变化，这时色彩空间必须跟着改；
    // 其余情况下分量数不变，原 /ColorSpace（含 ICCBased 引用）继续有效。
    if r.gray && entry.model == Ok(Model::Rgb) {
        stream
            .dict
            .set("ColorSpace", Object::Name(b"DeviceGray".to_vec()));
    }
    stream.allows_compression = false; // 数据已经压缩过，别再套一层 flate
}
