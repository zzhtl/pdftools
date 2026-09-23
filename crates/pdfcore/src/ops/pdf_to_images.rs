//! PDF 转图片：选中的每一页画成一张 PNG 或 JPEG，写进目标文件夹。
//!
//! 几页同时画（最多 4 个线程），每页画完立刻编码写盘：几百页的文档也不会把几百张
//! 位图同时留在内存里。输出名是 `原名_p001.png`，照 [`OutputNamer`] 的规矩什么都不覆盖。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::ImageEncoder;
use rayon::prelude::*;

use crate::error::{CoreError, Report, Result, Warning, WarningKind};
use crate::fsio::{write_atomic, OutputNamer};
use crate::imaging::{encode_jpeg_image, worker_pool};
use crate::pdf::render::{fit_scale, Document};
use crate::progress::{Progress, ProgressSink};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// 无损。文字页首选：JPEG 会在笔画边缘留下噪点。
    Png,
    /// 体积小，适合照片、扫描件。
    Jpeg,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Jpeg => "jpg",
        }
    }
}

pub struct Options {
    pub dpi: f32,
    pub format: Format,
    /// 要哪几页，写法见 [`crate::pdf::ranges::parse`]；空表示全部。按每份 PDF
    /// 自己的页数解释：批量转换时同一个范围套用到每一份上。
    pub pages: String,
}

pub struct Outcome {
    /// 写出的文件，按页码顺序。
    pub files: Vec<PathBuf>,
    /// 这份 PDF 一共几页。
    pub page_count: usize,
}

/// 分辨率的范围。再低看不清字，再高一页就是上亿像素。
pub const DPI_RANGE: std::ops::RangeInclusive<f32> = 36.0..=1200.0;
const JPEG_QUALITY: u8 = 90;

pub fn run(
    pdf: &Path,
    out_dir: &Path,
    opts: &Options,
    namer: &mut OutputNamer,
    sink: &dyn ProgressSink,
) -> Result<Report<Outcome>> {
    if !DPI_RANGE.contains(&opts.dpi) {
        return Err(CoreError::Unsupported(format!(
            "分辨率 {} DPI 超出范围（{}–{}）",
            opts.dpi,
            DPI_RANGE.start(),
            DPI_RANGE.end()
        )));
    }
    let data = std::fs::read(pdf).map_err(|e| CoreError::io(pdf, e))?;
    let doc = Document::open(data)?;
    let count = doc.page_count();
    if count == 0 {
        return Err(CoreError::Pdf("这份 PDF 没有页面".into()));
    }
    let pages = crate::pdf::ranges::parse(&opts.pages, count).map_err(CoreError::Unsupported)?;
    std::fs::create_dir_all(out_dir).map_err(|e| CoreError::io(out_dir, e))?;

    // 名字在开画之前按页码顺序定好：并行画完的先后不影响叫什么。
    let stem = pdf.file_stem().unwrap_or_default().to_string_lossy();
    let digits = count.to_string().len().max(3);
    let ext = opts.format.extension();
    let targets: Vec<(usize, PathBuf)> = pages
        .iter()
        .map(|&p| {
            (
                p,
                namer.name(out_dir, &format!("{stem}_p{p:0digits$}"), ext),
            )
        })
        .collect();

    let total = targets.len();
    sink.emit(Progress::Started { total });
    let scale = opts.dpi / 72.0;
    let done = AtomicUsize::new(0);
    worker_pool()?.install(|| {
        targets.par_iter().try_for_each_init(
            || doc.cache(),
            |cache, (page, out)| {
                if sink.is_cancelled() {
                    return Err(CoreError::Cancelled);
                }
                let img = doc.render(page - 1, scale, cache);
                write_atomic(out, &encode(img, opts.format)?)?;
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                sink.emit(Progress::Item {
                    done: n,
                    total,
                    label: format!("第 {page} 页"),
                });
                Ok(())
            },
        )
    })?;

    let mut warnings = Vec::new();
    let reduced: Vec<String> = pages
        .iter()
        .filter(|&&p| fit_scale(doc.page_size(p - 1), scale) < scale)
        .map(|p| p.to_string())
        .collect();
    if !reduced.is_empty() {
        warnings.push(Warning::new(
            WarningKind::UnsupportedElement,
            format!(
                "第 {} 页幅面太大，按 {} DPI 画会超过一页 6400 万像素的上限，已降低分辨率",
                reduced.join("、"),
                opts.dpi
            ),
        ));
    }
    let notes = doc.notes();
    for (wanted, used) in &notes.substituted {
        warnings.push(Warning::new(
            WarningKind::FontSubstituted,
            format!("字体「{wanted}」没有嵌入 PDF，用系统里的「{used}」显示"),
        ));
    }
    if !notes.missing.is_empty() {
        warnings.push(Warning::new(
            WarningKind::UnsupportedElement,
            format!(
                "字体「{}」没有嵌入 PDF，本机也没有可替代的中文字体，这些文字画不出来",
                notes.missing.join("」「")
            ),
        ));
    }
    if notes.unsupported_font {
        warnings.push(Warning::new(
            WarningKind::UnsupportedElement,
            "有的字体用了暂不支持的编码，其中的文字可能缺失",
        ));
    }
    if notes.broken_image {
        warnings.push(Warning::new(
            WarningKind::UnsupportedElement,
            "有的图片解不开，画成了空白",
        ));
    }

    Ok(Report::with(
        Outcome {
            files: targets.into_iter().map(|(_, p)| p).collect(),
            page_count: count,
        },
        warnings,
    ))
}

fn encode(img: image::RgbImage, format: Format) -> Result<Vec<u8>> {
    // 文字页几乎都是纯灰阶：存成单通道仍然是无损的，PNG 体积却减半
    //（331 页的文字文档 150 DPI：75 MB → 36 MB），编码也更快。
    let gray = img.pixels().all(|p| p.0[0] == p.0[1] && p.0[1] == p.0[2]);
    match format {
        Format::Png => {
            let mut buf = Vec::new();
            let (w, h) = img.dimensions();
            let enc = PngEncoder::new_with_quality(
                &mut buf,
                CompressionType::Default,
                FilterType::Adaptive,
            );
            if gray {
                let luma: Vec<u8> = img
                    .as_raw()
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .map(|p| p[0])
                    .collect();
                enc.write_image(&luma, w, h, image::ExtendedColorType::L8)
            } else {
                enc.write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgb8)
            }
            .map_err(|e| CoreError::Image(format!("PNG 编码失败：{e}")))?;
            Ok(buf)
        }
        Format::Jpeg => encode_jpeg_image(&image::DynamicImage::ImageRgb8(img), JPEG_QUALITY, gray),
    }
}
