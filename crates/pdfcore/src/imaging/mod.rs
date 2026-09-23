//! 图像流水线：探测 → 解码 → 方向校正 → 缩放 → 编码。
//!
//! 「多图转 PDF」和「图片批量压缩」共用这一条流水线。

pub mod probe;
mod quality;

pub use quality::{
    encode_jpeg as encode_jpeg_image, estimate_flate_len, resize as resize_image, ImageQuality,
    Tier, LOSSLESS_TOLERANCE,
};

/// PDF 压缩场景下的质量参数。
///
/// `grayscale` 单独传进来而不是塞进档位：转灰度是**内容改变**而不是压缩，
/// 法律文书上的红色公章和签名笔迹转灰就没了，所以它永远是一个独立的、默认关闭的选项。
pub fn quality_of(tier: Tier, grayscale: bool) -> ImageQuality {
    let mut q = tier.for_pdf();
    q.grayscale = grayscale;
    q
}

use std::path::Path;

use crate::error::{CoreError, Result};
use probe::Dpi;

/// 主图数据，以及它该以什么 filter 写进 PDF。
pub enum ColorData {
    /// 已经是 JPEG 字节，PDF 里直接用 `/DCTDecode`，不解码不重编码。
    Jpeg { bytes: Vec<u8>, gray: bool },
    /// 原始像素（8 位），写出时再无损压缩。
    Raw { bytes: Vec<u8>, gray: bool },
    /// 已经无损压好的像素（[`flate_image`](crate::pdf::writer::image::flate_image)）：
    /// 压缩很费时，在准备图片的线程里先做掉。
    Flate { bytes: Vec<u8>, gray: bool },
}

/// 这张图最终是以什么保真度进入 PDF 的。会在界面上以徽章形式如实告诉用户。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fidelity {
    /// 原始 JPEG 字节整段搬进 PDF。零损失。
    Passthrough,
    /// 解码后以无损方式存储（Flate）。像素完全一致。
    Lossless,
    /// 因为需要旋转或降采样而重新编码过。
    Reencoded,
}

impl Fidelity {
    pub fn label(self) -> &'static str {
        match self {
            Fidelity::Passthrough => "原图直通（无损）",
            Fidelity::Lossless => "无损",
            Fidelity::Reencoded => "已重新编码",
        }
    }
}

pub struct PreparedImage {
    pub color: ColorData,
    /// 灰度 alpha 平面，写成 `/SMask`。
    pub alpha: Option<Vec<u8>>,
    pub width: u32,
    pub height: u32,
    pub fidelity: Fidelity,
    /// 页面尺寸（点）。已按下文的规则定好，且已考虑方向带来的宽高互换。
    pub page_w_pt: f32,
    pub page_h_pt: f32,
    /// 多页 TIFF 被丢掉的页数。解码器只读第一页，调用方必须把这件事告诉用户。
    pub dropped_pages: usize,
    /// 仍需由 PDF 变换矩阵施加的 EXIF 方向。
    ///
    /// 走直通路径时像素没有被旋转过，方向靠内容流里的矩阵来纠正 ——
    /// 这样连横拍的手机照片也能保持字节级无损。
    /// 解码过的路径已经把方向作用在像素上了，这里会是 `NoTransforms`。
    pub orientation: image::metadata::Orientation,
}

impl PreparedImage {
    /// 把图片铺满整页的变换矩阵，含 EXIF 方向纠正。
    ///
    /// image XObject 永远画在 (0,0)-(1,1) 的单位方块里，且第一行样本在**上边**（y=1）。
    /// 下面每一条都是按「存储态的四个角应当落到页面的哪四个角」解出来的。
    pub fn placement_matrix(&self) -> [f32; 6] {
        use image::metadata::Orientation as O;
        let (w, h) = (self.page_w_pt, self.page_h_pt);
        match self.orientation {
            O::NoTransforms => [w, 0.0, 0.0, h, 0.0, 0.0],
            O::FlipHorizontal => [-w, 0.0, 0.0, h, w, 0.0],
            O::Rotate180 => [-w, 0.0, 0.0, -h, w, h],
            O::FlipVertical => [w, 0.0, 0.0, -h, 0.0, h],
            // 以下四种含 90/270 旋转，页面宽高已经互换过。
            O::Rotate90FlipH => [0.0, -h, -w, 0.0, w, h],
            O::Rotate90 => [0.0, -h, w, 0.0, 0.0, h],
            O::Rotate270FlipH => [0.0, h, w, 0.0, 0.0, 0.0],
            O::Rotate270 => [0.0, h, -w, 0.0, w, 0.0],
        }
    }
}

/// 这几种方向会让显示出来的宽高相对存储态互换。
fn swaps_dimensions(o: image::metadata::Orientation) -> bool {
    use image::metadata::Orientation as O;
    matches!(
        o,
        O::Rotate90 | O::Rotate270 | O::Rotate90FlipH | O::Rotate270FlipH
    )
}

/// A4 长边，单位点。没有可信 DPI 元数据时，长边归一到这个值。
const A4_LONG_EDGE_PT: f32 = 841.89;
/// PDF 单页尺寸上限是 14400 个用户空间单位。
const MAX_PAGE_PT: f32 = 14_400.0;

/// 决定页面尺寸。
///
/// 规则：**页面比例永远跟随图片**（这才是用户看得见的「无白边」），
/// 而页面的**绝对大小**是一个策略。
///
/// 有可信 DPI 就按物理尺寸换算；否则把长边归一到 A4 长边。
///
/// 为什么 72 DPI 要当作「不可信」：它是无数工具在不知道真实分辨率时写下的默认值。
/// 手头的样张里就有两张 3904×2928 的照片标着 72 DPI —— 照字面算出来是
/// 1377mm × 1033mm 的页面，任何阅读器打开都是 8% 缩放，显然不是用户要的。
pub fn page_size_pt(width: u32, height: u32, dpi: Option<Dpi>) -> (f32, f32) {
    let (w, h) = (width.max(1) as f32, height.max(1) as f32);

    let trustworthy = dpi.filter(|d| {
        // 72 和 96 是两个最常见的「我不知道」默认值，不足以作为物理尺寸依据。
        let is_default = |v: f32| (v - 72.0).abs() < 0.5 || (v - 96.0).abs() < 0.5;
        !(is_default(d.x) && is_default(d.y))
    });

    let (mut pw, mut ph) = match trustworthy {
        Some(d) => (w / d.x * 72.0, h / d.y * 72.0),
        None => {
            let scale = A4_LONG_EDGE_PT / w.max(h);
            (w * scale, h * scale)
        }
    };

    // 超出 PDF 上限就等比收回来，不能写出打不开的文件。
    let over = (pw / MAX_PAGE_PT).max(ph / MAX_PAGE_PT);
    if over > 1.0 {
        pw /= over;
        ph /= over;
    }
    (pw.max(1.0), ph.max(1.0))
}

/// 读取一张图片的时间。
///
/// 依次尝试三个**嵌在文件内部**的来源：EXIF → XMP → IPTC。
/// 它们随文件本身走，重命名、复制、转发都不会改变，才配称为「拍摄时间」。
///
/// 三个都没有时，退到文件系统时间，但会标成 `FileSystem` —— 那**不是**拍摄时间，
/// 复制一次就被刷成当前时刻，调用方必须区别对待，不能拿它去填 PDF 的 `/CreationDate`。
pub fn read_time(path: &Path) -> Option<crate::timestamp::DatedFile> {
    let bytes = std::fs::read(path).unwrap_or_default();
    time_of(&bytes, path)
}

/// 同 [`read_time`]，文件内容已经读进来了（`bytes`，读不到时传空的）。
pub fn time_of(bytes: &[u8], path: &Path) -> Option<crate::timestamp::DatedFile> {
    use crate::timestamp::{DatedFile, TimeSource, Timestamp};

    if !bytes.is_empty() {
        let meta = read_metadata(bytes);

        if let Some(when) = meta.exif.as_deref().and_then(probe::capture_time) {
            return Some(DatedFile {
                when,
                source: TimeSource::Exif,
            });
        }
        if let Some(when) = meta
            .xmp
            .as_deref()
            .and_then(|x| std::str::from_utf8(x).ok())
            .and_then(probe::xmp_capture_time)
        {
            return Some(DatedFile {
                when,
                source: TimeSource::Xmp,
            });
        }
        if let Some(when) = meta.iptc.as_deref().and_then(probe::iptc_capture_time) {
            return Some(DatedFile {
                when,
                source: TimeSource::Iptc,
            });
        }
    }

    // 兜底：取创建时间与修改时间里更早的那个。复制会把两者都刷新，
    // 但至少「改内容」只动 mtime，取较早值能少受一点干扰。
    let md = std::fs::metadata(path).ok()?;
    let earliest = [md.created().ok(), md.modified().ok()]
        .into_iter()
        .flatten()
        .min()?;
    Some(DatedFile {
        when: Timestamp::from_system_time(earliest),
        source: TimeSource::FileSystem,
    })
}

#[derive(Default)]
struct EmbeddedMetadata {
    exif: Option<Vec<u8>>,
    xmp: Option<Vec<u8>>,
    iptc: Option<Vec<u8>>,
}

/// 一次性取出三种元数据块。只开一次 decoder，避免把文件读三遍。
fn read_metadata(bytes: &[u8]) -> EmbeddedMetadata {
    use image::ImageDecoder;
    let Ok(reader) = image::ImageReader::new(std::io::BufReader::new(std::io::Cursor::new(bytes)))
        .with_guessed_format()
    else {
        return EmbeddedMetadata::default();
    };
    let Ok(mut decoder) = reader.into_decoder() else {
        return EmbeddedMetadata::default();
    };
    EmbeddedMetadata {
        exif: decoder.exif_metadata().ok().flatten(),
        xmp: decoder.xmp_metadata().ok().flatten(),
        iptc: decoder.iptc_metadata().ok().flatten(),
    }
}

/// 为放进 PDF 准备一张图。
pub fn prepare_for_pdf(path: &Path, quality: &ImageQuality) -> Result<PreparedImage> {
    let bytes = std::fs::read(path).map_err(|e| CoreError::io(path, e))?;
    prepare_bytes(bytes, path, quality)
}

/// 同 [`prepare_for_pdf`]，文件内容已经读进来了：取拍摄时间也用这一份，一张图只读一遍。
/// 会占很多 CPU（解码、缩放、编码），可以在多个线程里同时准备不同的图。
pub fn prepare_bytes(bytes: Vec<u8>, path: &Path, quality: &ImageQuality) -> Result<PreparedImage> {
    if probe::is_heif(path) {
        return Err(CoreError::Unsupported(format!(
            "{} 是 HEIC/HEIF 格式，本程序不支持。请先在系统相册里导出为 JPEG 或 PNG。",
            path.file_name().unwrap_or_default().to_string_lossy()
        )));
    }

    let container = probe::sniff(&bytes);
    let dropped_pages = if container == probe::Container::Tiff {
        probe::tiff_page_count(&bytes)
            .unwrap_or(1)
            .saturating_sub(1)
    } else {
        0
    };

    // 先靠元数据决定「要不要解码」。对上百个文件逐个全量解码再决定，是本末倒置。
    let exif_raw = read_exif(&bytes);
    let dpi = probe::dpi(&bytes, exif_raw.as_deref());
    let orientation = read_orientation(exif_raw.as_deref());

    // 这张图是否有资格走「原始字节直通」。
    //
    // 方向不再是障碍：旋转由 PDF 的变换矩阵施加，像素一个字节都不用动。
    // 这一点很关键 —— 手机横拍的照片几乎都带方向标记，
    // 如果为了摆正而重新编码，「绝不失真」这个承诺对它们就失效了。
    //
    // CMYK（4 分量）仍然不行：PDF 里要写 /DeviceCMYK 并处理 Adobe APP14 的反相约定，
    // 判错了输出是偏色而不是报错，不赌。12 位、算术编码等 PDF 不支持的 JPEG 同样不行。
    //
    // `jpeg_src` 与档位无关：即使这一档不主动直通，「重编码后不比原图小就退回原图」
    // 这条护栏也要用到它。
    let jpeg_src: Option<(u32, u32, bool)> = (container == probe::Container::Jpeg)
        .then(|| probe::jpeg_info(&bytes))
        .flatten()
        .filter(|i| i.embeddable_in_pdf() && (i.components == 1 || i.components == 3))
        .map(|i| (i.width, i.height, i.components == 1));
    let passthrough = jpeg_src.filter(|_| quality.allow_passthrough);

    // 页面按**显示后**的宽高算：旋转 90/270 时宽高互换。
    let page_of = |w: u32, h: u32| {
        if swaps_dimensions(orientation) {
            let (a, b) = page_size_pt(h, w, dpi.map(|d| Dpi { x: d.y, y: d.x }));
            (a, b)
        } else {
            page_size_pt(w, h, dpi)
        }
    };

    if let Some((w, h, gray)) = passthrough {
        let (page_w, page_h) = page_of(w, h);
        // 有效分辨率按显示尺寸算，所以这里传显示后的宽高。
        let (disp_w, disp_h) = if swaps_dimensions(orientation) {
            (h, w)
        } else {
            (w, h)
        };
        if !quality.needs_downscale(disp_w, disp_h, page_w, page_h) {
            return Ok(PreparedImage {
                color: ColorData::Jpeg { bytes, gray },
                alpha: None,
                width: w,
                height: h,
                fidelity: Fidelity::Passthrough,
                page_w_pt: page_w,
                page_h_pt: page_h,
                dropped_pages,
                orientation,
            });
        }
    }

    // 走到这里就必须真的解码了。
    let mut img = image::load_from_memory(&bytes)
        .map_err(|e| CoreError::Image(format!("解码 {} 失败：{e}", path.display())))?;
    // 这条路径上方向直接作用在像素上，后面不再需要变换矩阵。
    img.apply_orientation(orientation);

    let (mut w, mut h) = (img.width(), img.height());
    let (page_w, page_h) = page_size_pt(w, h, dpi);

    let mut rescaled = false;
    if let Some((tw, th)) = quality.downscale_target(w, h, page_w, page_h) {
        img = quality::resize(&img, tw, th)?;
        w = tw;
        h = th;
        rescaled = true;
    }

    let has_alpha = img.color().has_alpha();
    let alpha = has_alpha.then(|| extract_alpha(&img));
    // 灰度的源图按灰度存，扩成 RGB 只会让体积翻三倍。
    let gray = quality.grayscale || is_gray_source(&img);

    let (color, fidelity) = if quality.lossless_only() && !rescaled {
        (flate_color(&img, gray), Fidelity::Lossless)
    } else {
        // 到底该用 JPEG 还是无损存储？不靠「看起来像不像照片」这类猜测 ——
        // 试过用颜色数做判据，真实的白墙照片只有 0.8% 的不同色占比，
        // 会被一律误判成图形，然后以原始像素塞进 PDF，体积暴涨十几倍。
        //
        // 改成直接测量：两种编码各估一次体积，按实测结果决定。
        // 截图和线稿的 flate 体积远小于 JPEG，会自然选到无损；
        // 照片的无损体积是 JPEG 的十几倍，会自然选到 JPEG。不需要任何魔法阈值。
        let jpeg = quality::encode_jpeg(&img, quality.jpeg_quality, gray)?;
        let raw = samples(&img, gray);
        let components = if gray { 1 } else { 3 };
        let flate_est = quality::estimate_flate_len(&raw, w as usize * components, components);
        let use_flate = (flate_est as f32) <= jpeg.len() as f32 * quality::LOSSLESS_TOLERANCE;

        // 护栏（所有档位）：降采样 + 重编码之后不比原文件小，就退回原图直通。
        //
        // 这不是假想情况：源文件本就是高压缩率的 JPEG，我们把它解开、缩小、
        // 再以更高质量编回去，像素少了但字节多了。此时「压缩」既没省空间，
        // 还白白损失一代画质。原图直通在两个维度上都更优 —— 哪怕这一档本不主动直通。
        let chosen_len = if use_flate { flate_est } else { jpeg.len() };
        if let Some((pw, ph, pgray)) = jpeg_src {
            if chosen_len >= bytes.len() {
                let (page_w, page_h) = page_of(pw, ph);
                return Ok(PreparedImage {
                    color: ColorData::Jpeg { bytes, gray: pgray },
                    alpha: None,
                    width: pw,
                    height: ph,
                    fidelity: Fidelity::Passthrough,
                    page_w_pt: page_w,
                    page_h_pt: page_h,
                    dropped_pages,
                    orientation,
                });
            }
        }

        if use_flate {
            (
                ColorData::Flate {
                    bytes: crate::pdf::writer::image::flate_image(&raw, w, components),
                    gray,
                },
                if rescaled {
                    Fidelity::Reencoded
                } else {
                    Fidelity::Lossless
                },
            )
        } else {
            (ColorData::Jpeg { bytes: jpeg, gray }, Fidelity::Reencoded)
        }
    };

    Ok(PreparedImage {
        color,
        alpha,
        width: w,
        height: h,
        fidelity,
        page_w_pt: page_w,
        page_h_pt: page_h,
        dropped_pages,
        // 方向已经作用在像素上了。
        orientation: image::metadata::Orientation::NoTransforms,
    })
}

/// 文档里嵌着的一张图（docx 的 `word/media/…`），准备写进 PDF。
pub struct EmbeddedImage {
    pub color: ColorData,
    pub alpha: Option<Vec<u8>>,
    pub width: u32,
    pub height: u32,
}

/// 能直通的 JPEG 原样用，其余解码成像素（有 alpha 的另存一个平面）。显示大小由文档
/// 定，这里不缩放。EMF、WMF、SVG 这类矢量图解不开，返回错误。
pub fn prepare_embedded(bytes: &[u8]) -> Result<EmbeddedImage> {
    if probe::sniff(bytes) == probe::Container::Jpeg {
        if let Some(i) = probe::jpeg_info(bytes)
            .filter(|i| i.embeddable_in_pdf() && (i.components == 1 || i.components == 3))
        {
            return Ok(EmbeddedImage {
                color: ColorData::Jpeg {
                    bytes: bytes.to_vec(),
                    gray: i.components == 1,
                },
                alpha: None,
                width: i.width,
                height: i.height,
            });
        }
    }
    let img = image::load_from_memory(bytes).map_err(|e| CoreError::Image(format!("{e}")))?;
    let alpha = img.color().has_alpha().then(|| extract_alpha(&img));
    let gray = is_gray_source(&img);
    Ok(EmbeddedImage {
        color: ColorData::Raw {
            bytes: samples(&img, gray),
            gray,
        },
        alpha,
        width: img.width(),
        height: img.height(),
    })
}

/// 解码一张图，并把 EXIF 方向作用到像素上。
///
/// 重新编码会丢掉 EXIF，方向标记也就跟着没了 —— 不先转正，横拍的手机照片
/// 压完就是躺着的。
pub fn decode_oriented(bytes: &[u8]) -> Result<image::DynamicImage> {
    let mut img =
        image::load_from_memory(bytes).map_err(|e| CoreError::Image(format!("解码失败：{e}")))?;
    img.apply_orientation(read_orientation(read_exif(bytes).as_deref()));
    Ok(img)
}

/// 源图本身就是灰度的（含带透明通道的灰度）。
pub fn is_gray_source(img: &image::DynamicImage) -> bool {
    use image::ColorType as C;
    matches!(img.color(), C::L8 | C::La8 | C::L16 | C::La16)
}

/// 8 位的样本：灰度一个分量，否则 RGB 三个。
fn samples(img: &image::DynamicImage, gray: bool) -> Vec<u8> {
    if gray {
        img.to_luma8().into_raw()
    } else {
        img.to_rgb8().into_raw()
    }
}

/// 无损压好的样本。
fn flate_color(img: &image::DynamicImage, gray: bool) -> ColorData {
    let components = if gray { 1 } else { 3 };
    ColorData::Flate {
        bytes: crate::pdf::writer::image::flate_image(&samples(img, gray), img.width(), components),
        gray,
    }
}

fn extract_alpha(img: &image::DynamicImage) -> Vec<u8> {
    img.to_rgba8().pixels().map(|p| p.0[3]).collect()
}

/// 取出容器里的 EXIF 原始字节。`image` 能给我们这段字节，但不解析分辨率字段。
fn read_exif(bytes: &[u8]) -> Option<Vec<u8>> {
    use image::ImageDecoder;
    let cursor = std::io::Cursor::new(bytes);
    let reader = image::ImageReader::new(std::io::BufReader::new(cursor))
        .with_guessed_format()
        .ok()?;
    let mut decoder = reader.into_decoder().ok()?;
    decoder.exif_metadata().ok().flatten()
}

fn read_orientation(exif_raw: Option<&[u8]>) -> image::metadata::Orientation {
    exif_raw
        .and_then(image::metadata::Orientation::from_exif_chunk)
        .unwrap_or(image::metadata::Orientation::NoTransforms)
}
