//! 质量档位，以及缩放/编码的具体实现。

use image::DynamicImage;

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
            Tier::Lossless => "绝不改动任何像素。JPEG 原样搬入，其余格式无损存储",
            Tier::HighQuality => "上限 300 DPI（印刷级）。超过上限的图会被重新采样",
            Tier::Balanced => "上限 200 DPI。超过上限的图会被重新采样",
            Tier::Extreme => "上限 144 DPI，体积优先。超过上限的图会被重新采样",
        }
    }

    /// 这一档是否会改动像素。界面上要据此给出醒目提示 ——
    /// 用户选了「不失真」就该真的不失真，选了别的也该知道自己在换什么。
    pub fn is_lossy(self) -> bool {
        !matches!(self, Tier::Lossless)
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
    // 不用优化 Huffman 表，尽管它能省不少体积（12 张实拍照片：q58 省 14%，q80 省 7%）：
    // zune-jpeg 0.5.15（image crate 与 hayro 用的解码器）会把 jpeg-encoder 优化过的表
    // 与 4:2:0 采样组合编出的部分 JPEG 解成横条纹，libjpeg 解同一份文件却完全正常。
    // 后果是我们压过的 PDF 再压一次就会被悄悄毁掉。zune-jpeg 0.5.16 已修复（尚未正式发布），
    // 等 `cargo update` 能拿到它、`own_jpeg_output_decodes_correctly` 在开启时也能通过，再打开。
    enc.set_optimized_huffman_tables(false);
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

/// 估算原始像素按 [`flate_image`](crate::pdf::writer::image::flate_image) 无损压缩后
/// 的体积。`bpp` 是每个像素几个字节（灰度 1，RGB 3）。
///
/// 整份压一遍太贵 —— 一张 1200 万像素的 RGB 图有 36 MB 原始数据，
/// 而我们只是想知道个数量级。按行采样 1/8 再外推，flate 的压缩率在这个尺度上足够稳定。
/// 采样的行照样对着它真正的上一行做 PNG 预测，估出来的才与实际压缩一致。
pub fn estimate_flate_len(raw: &[u8], row_bytes: usize, bpp: usize) -> usize {
    use flate2::write::ZlibEncoder;
    use std::io::Write;

    if row_bytes == 0 || raw.len() <= row_bytes {
        return raw.len();
    }
    const SAMPLE_EVERY: usize = 8;
    let zeros = vec![0u8; row_bytes];
    let mut sample = Vec::with_capacity(raw.len() / SAMPLE_EVERY + 2 * row_bytes);
    let mut rows = 0usize;
    let mut offset = 0usize;
    while offset + row_bytes <= raw.len() {
        let above = match offset.checked_sub(row_bytes) {
            Some(start) => &raw[start..offset],
            None => &zeros,
        };
        crate::pdf::writer::image::predict_row(
            above,
            &raw[offset..offset + row_bytes],
            bpp,
            &mut sample,
        );
        rows += 1;
        offset += row_bytes * SAMPLE_EVERY;
    }
    if rows == 0 {
        return raw.len();
    }

    let mut enc = ZlibEncoder::new(Vec::new(), flate2::Compression::new(7));
    if enc.write_all(&sample).is_err() {
        return raw.len();
    }
    let Ok(compressed) = enc.finish() else {
        return raw.len();
    };

    let total_rows = raw.len() / row_bytes;
    compressed.len().saturating_mul(total_rows) / rows.max(1)
}

/// 无损编码可以比 JPEG 大多少，仍然值得选。
///
/// 给无损一点余量是有意的：对文字、线稿、纯色块，JPEG 在边缘产生的振铃
/// 正是用户说的「发虚」，为此多付 50% 的体积是划算的。
/// 但照片的无损体积通常是 JPEG 的十几倍，这个余量挡得住。
pub const LOSSLESS_TOLERANCE: f32 = 1.5;
