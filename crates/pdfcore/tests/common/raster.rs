//! 用 hayro（纯 Rust）把 PDF 渲染成位图，再算「墨迹」重合度。
//!
//! 文本比对看不见的东西靠它兜底：表格线、图片、底纹画没画、画在哪。
//! 用纯 Rust 渲染器而不是 pdftoppm，是为了在三平台 CI 上都能跑。

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{render, RenderCache, RenderSettings};

/// 灰度位图，一字节一像素，行优先。
pub struct Gray {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

impl Gray {
    pub fn at(&self, x: usize, y: usize) -> u8 {
        self.px[y * self.w + x]
    }
}

/// 逐页渲染到白底灰度图。`dpi` = 72 时 1pt = 1px。
pub fn render_gray(pdf: &[u8], dpi: f32) -> Vec<Gray> {
    let pdf = Pdf::new(pdf.to_vec()).expect("hayro 无法解析该 PDF");
    let cache = RenderCache::new();
    let settings = InterpreterSettings::default();
    let scale = dpi / 72.0;
    let rs = RenderSettings {
        x_scale: scale,
        y_scale: scale,
        bg_color: WHITE,
        ..Default::default()
    };
    pdf.pages()
        .iter()
        .map(|page| {
            let pix = render(page, &cache, &settings, &rs);
            let (w, h) = (pix.width() as usize, pix.height() as usize);
            // 白底不透明，预乘与否结果相同。
            let px = pix
                .data_as_u8_slice()
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| {
                    (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32).round() as u8
                })
                .collect();
            Gray { w, h, px }
        })
        .collect()
}

/// 墨迹掩码：比阈值暗的像素算「有墨」。
pub struct Mask {
    pub w: usize,
    pub h: usize,
    pub on: Vec<bool>,
}

impl Mask {
    pub fn from_gray(g: &Gray, threshold: u8) -> Self {
        Self {
            w: g.w,
            h: g.h,
            on: g.px.iter().map(|&v| v < threshold).collect(),
        }
    }

    pub fn count(&self) -> usize {
        self.on.iter().filter(|b| **b).count()
    }

    /// 3×3 膨胀一次。两个渲染结果之间亚像素级的偏移不该算作差异。
    pub fn dilate(&self) -> Self {
        let mut on = vec![false; self.on.len()];
        for y in 0..self.h {
            for x in 0..self.w {
                if !self.on[y * self.w + x] {
                    continue;
                }
                for dy in -1i64..=1 {
                    for dx in -1i64..=1 {
                        let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                        if nx >= 0 && ny >= 0 && (nx as usize) < self.w && (ny as usize) < self.h {
                            on[ny as usize * self.w + nx as usize] = true;
                        }
                    }
                }
            }
        }
        Self {
            w: self.w,
            h: self.h,
            on,
        }
    }

    /// 有墨区域的外框 (x0, y0, x1, y1)，像素坐标，原点在左上角。
    pub fn bbox(&self) -> Option<(usize, usize, usize, usize)> {
        let mut b: Option<(usize, usize, usize, usize)> = None;
        for y in 0..self.h {
            for x in 0..self.w {
                if self.on[y * self.w + x] {
                    b = Some(match b {
                        None => (x, y, x, y),
                        Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                    });
                }
            }
        }
        b
    }
}

/// 交并比。尺寸不同时只比公共区域（页面尺寸应当一致，不一致本身会在页数/尺寸上报出来）。
/// 两边都是空白页时返回 1。
pub fn iou(a: &Mask, b: &Mask) -> f64 {
    let (w, h) = (a.w.min(b.w), a.h.min(b.h));
    let (mut inter, mut union) = (0usize, 0usize);
    for y in 0..h {
        for x in 0..w {
            let (p, q) = (a.on[y * a.w + x], b.on[y * b.w + x]);
            inter += (p && q) as usize;
            union += (p || q) as usize;
        }
    }
    if union == 0 {
        1.0
    } else {
        inter as f64 / union as f64
    }
}

/// 比对用的掩码：72dpi、亮度 < 160 算墨迹、膨胀一次。
pub fn masks(pdf: &[u8]) -> Vec<Mask> {
    render_gray(pdf, 72.0)
        .iter()
        .map(|g| Mask::from_gray(g, 160).dilate())
        .collect()
}
