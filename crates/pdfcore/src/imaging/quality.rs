//! 质量档位，以及缩放/编码的具体实现。

use image::{DynamicImage, GenericImageView};

use crate::error::{CoreError, Result};

/// 用户可选的四档。语义在「图片」和「PDF 压缩」两个场景下略有差别，
/// 所以用两个构造方法分别给出参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// 不改变任何像素。
    Lossless,
    HighQuality,
    Balanced,
    Extreme,
}

impl Tier {
    pub const ALL: [Tier; 4] = [
        Tier::Lossless,
        Tier::HighQuality,
        Tier::Balanced,
        Tier::Extreme,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Tier::Lossless => "无损",
            Tier::HighQuality => "高质量",
            Tier::Balanced => "平衡",
            Tier::Extreme => "极致压缩",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Tier::Lossless => "不改动任何像素，只做无损的结构优化",
            Tier::HighQuality => "上限 300 DPI（印刷级），肉眼不可见差异",
            Tier::Balanced => "上限 200 DPI，屏幕阅读足够清晰",
            Tier::Extreme => "上限 144 DPI，体积优先",
        }
    }

    /// 图片转 PDF / 图片批量压缩用的参数。
    pub fn for_images(self) -> ImageQuality {
        match self {
            Tier::Lossless => ImageQuality {
                max_dpi: None,
                jpeg_quality: 100,
                allow_passthrough: true,
                grayscale: false,
                lossless: true,
            },
            Tier::HighQuality => ImageQuality {
                max_dpi: Some(300.0),
                jpeg_quality: 92,
                allow_passthrough: true,
                grayscale: false,
                lossless: false,
            },
            Tier::Balanced => ImageQuality {
                max_dpi: Some(200.0),
                jpeg_quality: 80,
                allow_passthrough: false,
                grayscale: false,
                lossless: false,
            },
            Tier::Extreme => ImageQuality {
                max_dpi: Some(144.0),
                jpeg_quality: 65,
                allow_passthrough: false,
                grayscale: false,
                lossless: false,
            },
        }
    }

    /// 压缩已有 PDF 时用的参数。比图片场景更激进一点，
    /// 因为用户点「压缩」时的诉求就是把体积降下来。
    pub fn for_pdf(self) -> ImageQuality {
        match self {
            Tier::Lossless => ImageQuality {
                max_dpi: None,
                jpeg_quality: 100,
                allow_passthrough: true,
                grayscale: false,
                lossless: true,
            },
            Tier::HighQuality => ImageQuality {
                max_dpi: Some(300.0),
                jpeg_quality: 85,
                allow_passthrough: false,
                grayscale: false,
                lossless: false,
            },
            Tier::Balanced => ImageQuality {
                max_dpi: Some(200.0),
                jpeg_quality: 72,
                allow_passthrough: false,
                grayscale: false,
                lossless: false,
            },
            Tier::Extreme => ImageQuality {
                max_dpi: Some(144.0),
                jpeg_quality: 58,
                allow_passthrough: false,
                grayscale: false,
                lossless: false,
            },
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ImageQuality {
    /// 相对图片在页面上的物理尺寸，允许的最大有效分辨率。None 表示不限。
    pub max_dpi: Option<f32>,
    pub jpeg_quality: u8,
    /// 允许 JPEG 原始字节直通。
    pub allow_passthrough: bool,
    /// 转灰度。**默认必须是 false**：法律文书上的红色公章和签名笔迹
    /// 一旦转灰就没了，那是内容改变，不是压缩。
    pub grayscale: bool,
    /// 只做无损处理，不碰像素。
    pub lossless: bool,
}

impl ImageQuality {
    pub fn lossless_only(&self) -> bool {
        self.lossless
    }

    /// 图片在页面上的有效分辨率。
    fn effective_dpi(&self, px_w: u32, px_h: u32, page_w_pt: f32, page_h_pt: f32) -> f32 {
        let dpi_x = px_w as f32 / (page_w_pt / 72.0).max(f32::EPSILON);
        let dpi_y = px_h as f32 / (page_h_pt / 72.0).max(f32::EPSILON);
        dpi_x.max(dpi_y)
    }

    pub fn needs_downscale(&self, px_w: u32, px_h: u32, page_w_pt: f32, page_h_pt: f32) -> bool {
        self.downscale_target(px_w, px_h, page_w_pt, page_h_pt)
            .is_some()
    }

    /// 只在超过上限时才降采样，且**从不放大**。
    pub fn downscale_target(
        &self,
        px_w: u32,
        px_h: u32,
        page_w_pt: f32,
        page_h_pt: f32,
    ) -> Option<(u32, u32)> {
        let cap = self.max_dpi?;
        let current = self.effective_dpi(px_w, px_h, page_w_pt, page_h_pt);
        if current <= cap * 1.02 {
            // 留 2% 余量，避免因为浮点误差把 300.0001 DPI 的图也重新采样一遍。
            return None;
        }
        let scale = cap / current;
        let tw = ((px_w as f32 * scale).round() as u32).max(1);
        let th = ((px_h as f32 * scale).round() as u32).max(1);
        (tw < px_w || th < px_h).then_some((tw, th))
    }
}

/// Lanczos3 降采样。这是 `ResizeOptions` 的默认算法，也是降采样的正确选择：
/// 双线性会糊，最近邻会锯齿。
pub fn resize(src: &DynamicImage, w: u32, h: u32) -> Result<DynamicImage> {
    let mut dst = DynamicImage::new(w, h, src.color());
    fast_image_resize::Resizer::new()
        .resize(src, &mut dst, None)
        .map_err(|e| CoreError::Image(format!("缩放失败：{e}")))?;
    Ok(dst)
}

/// JPEG 的最大边长是 65535 像素。
const JPEG_MAX_DIM: u32 = 65_535;

pub fn encode_jpeg(img: &DynamicImage, quality: u8, grayscale: bool) -> Result<Vec<u8>> {
    let (w, h) = (img.width(), img.height());
    if w > JPEG_MAX_DIM || h > JPEG_MAX_DIM {
        return Err(CoreError::Image(format!(
            "图片尺寸 {w}×{h} 超过 JPEG 的 65535 像素上限"
        )));
    }

    let mut buf = Vec::new();
    let mut enc = jpeg_encoder::Encoder::new(&mut buf, quality);
    // 优化 Huffman 表基本白送 3-5% 体积。
    enc.set_optimized_huffman_tables(true);
    // 不用 progressive：它对 PDF 里的 DCTDecode 流没有好处（PDF 不做渐进显示），
    // 却可能让个别老阅读器出问题。
    enc.set_progressive(false);

    if grayscale {
        let g = img.to_luma8();
        enc.encode(
            g.as_raw(),
            w as u16,
            h as u16,
            jpeg_encoder::ColorType::Luma,
        )
    } else {
        let rgb = img.to_rgb8();
        enc.encode(
            rgb.as_raw(),
            w as u16,
            h as u16,
            jpeg_encoder::ColorType::Rgb,
        )
    }
    .map_err(|e| CoreError::Image(format!("JPEG 编码失败：{e}")))?;

    Ok(buf)
}

/// 判断是照片还是图形/截图。
///
/// 这个判断决定用 JPEG 还是无损存储。对文字和大片纯色，JPEG 会在边缘产生振铃，
/// 那正是用户说的「模糊」；而这类图无损存储往往反而更小。
///
/// 手段是采样统计不同颜色数：照片几乎每个像素都不一样，截图则大量重复。
pub fn looks_photographic(img: &DynamicImage) -> bool {
    use std::collections::HashSet;

    const GRID: u32 = 64; // 最多采样 64×64 = 4096 个点
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return false;
    }
    let step_x = (w / GRID).max(1);
    let step_y = (h / GRID).max(1);

    let mut seen: HashSet<[u8; 3]> = HashSet::new();
    let mut total = 0u32;
    let mut y = 0;
    while y < h {
        let mut x = 0;
        while x < w {
            let p = img.get_pixel(x, y);
            seen.insert([p.0[0], p.0[1], p.0[2]]);
            total += 1;
            x += step_x;
        }
        y += step_y;
    }

    // 照片的采样点里不同颜色占比很高；截图/线稿通常远低于这个比例。
    total > 0 && (seen.len() as f32 / total as f32) > 0.5
}
