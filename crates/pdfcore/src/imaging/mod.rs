//! 图像流水线：探测 → 解码 → 方向校正 → 缩放 → 编码。
//!
//! 「多图转 PDF」和「图片批量压缩」共用这一条流水线。

pub mod probe;
mod quality;

pub use quality::{encode_jpeg as encode_jpeg_image, resize as resize_image, ImageQuality, Tier};

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
use probe::{Container, Dpi};

/// 主图数据，以及它该以什么 filter 写进 PDF。
pub enum ColorData {
    /// 已经是 JPEG 字节，PDF 里直接用 `/DCTDecode`，不解码不重编码。
    Jpeg { bytes: Vec<u8>, gray: bool },
    /// 原始像素（8 位），PDF 里用 `/FlateDecode`。真无损。
    Raw { bytes: Vec<u8>, gray: bool },
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
    /// 页面尺寸（点）。已按下文的规则定好。
    pub page_w_pt: f32,
    pub page_h_pt: f32,
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

/// 为放进 PDF 准备一张图。
pub fn prepare_for_pdf(path: &Path, quality: &ImageQuality) -> Result<PreparedImage> {
    if probe::is_heif(path) {
        return Err(CoreError::Unsupported(format!(
            "{} 是 HEIC/HEIF 格式，本程序不支持。请先在系统相册里导出为 JPEG 或 PNG。",
            path.file_name().unwrap_or_default().to_string_lossy()
        )));
    }

    let bytes = std::fs::read(path).map_err(|e| CoreError::io(path, e))?;
    let container = probe::sniff(&bytes);

    // 先拿元数据决定「要不要解码」。对上百个文件逐个全量解码再决定，是本末倒置。
    let exif_raw = read_exif(&bytes);
    let dpi = probe::dpi(&bytes, exif_raw.as_deref());
    let orientation = read_orientation(exif_raw.as_deref());

    if container == Container::Jpeg && quality.allow_passthrough {
        if let Some(info) = probe::jpeg_info(&bytes) {
            let (page_w, page_h) = page_size_pt(info.width, info.height, dpi);
            let needs_scale = quality.needs_downscale(info.width, info.height, page_w, page_h);
            let upright = orientation == image::metadata::Orientation::NoTransforms;
            // CMYK（4 分量）不直通：PDF 里要写 /DeviceCMYK 并处理 Adobe APP14 的反相约定，
            // 判错了输出就是偏色而不是报错。识别不了的一律走解码重编码，不赌。
            let simple_color = info.components == 1 || info.components == 3;

            if !needs_scale && upright && simple_color {
                return Ok(PreparedImage {
                    color: ColorData::Jpeg {
                        bytes,
                        gray: info.components == 1,
                    },
                    alpha: None,
                    width: info.width,
                    height: info.height,
                    fidelity: Fidelity::Passthrough,
                    page_w_pt: page_w,
                    page_h_pt: page_h,
                });
            }
        }
    }

    // 走到这里就必须真的解码了。
    let mut img = image::load_from_memory(&bytes)
        .map_err(|e| CoreError::Image(format!("解码 {} 失败：{e}", path.display())))?;
    // image 自带 EXIF 方向支持，不需要我们手写 8 种变换。漏了这步手机横拍的照片会躺倒。
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

    // 照片走 JPEG，图形/截图走无损。对文字和纯色块，JPEG 的振铃会在边缘糊一圈，
    // 这正是用户说的「模糊」，而无损存储反而往往更小。
    let is_photo = quality::looks_photographic(&img);

    let (color, fidelity) = if quality.lossless_only() && !rescaled {
        (raw_color(&img, quality.grayscale), Fidelity::Lossless)
    } else if is_photo {
        let jpeg = quality::encode_jpeg(&img, quality.jpeg_quality, quality.grayscale)?;
        (
            ColorData::Jpeg {
                bytes: jpeg,
                gray: quality.grayscale,
            },
            Fidelity::Reencoded,
        )
    } else {
        (
            raw_color(&img, quality.grayscale),
            if rescaled {
                Fidelity::Reencoded
            } else {
                Fidelity::Lossless
            },
        )
    };

    Ok(PreparedImage {
        color,
        alpha,
        width: w,
        height: h,
        fidelity,
        page_w_pt: page_w,
        page_h_pt: page_h,
    })
}

fn raw_color(img: &image::DynamicImage, grayscale: bool) -> ColorData {
    if grayscale {
        ColorData::Raw {
            bytes: img.to_luma8().into_raw(),
            gray: true,
        }
    } else {
        ColorData::Raw {
            bytes: img.to_rgb8().into_raw(),
            gray: false,
        }
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
