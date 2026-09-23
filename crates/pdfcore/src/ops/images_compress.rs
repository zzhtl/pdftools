//! 图片批量压缩。复用 `imaging` 的同一条流水线，输出到目标目录，
//! **什么都不覆盖**：原文件不动，同名输出自动改名（见 [`crate::fsio::OutputNamer`]）。

use std::path::{Path, PathBuf};

use crate::bail_if_cancelled;
use crate::error::{CoreError, Report, Result, Warning, WarningKind};
use crate::fsio::{write_atomic, OutputNamer};
use crate::imaging::Tier;
use crate::progress::{Progress, ProgressSink};

pub struct Item {
    pub source: PathBuf,
    pub output: PathBuf,
    pub before: u64,
    pub after: u64,
}

impl Item {
    pub fn saved_ratio(&self) -> f32 {
        if self.before == 0 {
            return 0.0;
        }
        1.0 - self.after as f32 / self.before as f32
    }
}

#[derive(Default)]
pub struct Outcome {
    pub items: Vec<Item>,
}

impl Outcome {
    pub fn total_before(&self) -> u64 {
        self.items.iter().map(|i| i.before).sum()
    }
    pub fn total_after(&self) -> u64 {
        self.items.iter().map(|i| i.after).sum()
    }
}

pub fn run(
    paths: &[PathBuf],
    out_dir: &Path,
    tier: Tier,
    sink: &dyn ProgressSink,
) -> Result<Report<Outcome>> {
    if paths.is_empty() {
        return Err(CoreError::Image("没有选择任何图片".into()));
    }
    std::fs::create_dir_all(out_dir).map_err(|e| CoreError::io(out_dir, e))?;

    let quality = tier.for_images();
    let mut outcome = Outcome::default();
    let mut warnings = Vec::new();
    let mut namer = OutputNamer::new(paths);

    sink.emit(Progress::Started { total: paths.len() });

    for (i, path) in paths.iter().enumerate() {
        bail_if_cancelled!(sink);
        let label = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();

        match compress_file(path, out_dir, &quality, &mut namer) {
            Ok((item, notes)) => {
                warnings.extend(notes);
                outcome.items.push(item);
            }
            Err(e) => warnings.push(Warning::new(
                WarningKind::ItemFailed,
                format!("{label}：{e}"),
            )),
        }

        sink.emit(Progress::Item {
            done: i + 1,
            total: paths.len(),
            label,
        });
    }

    if outcome.items.is_empty() {
        let reason = warnings
            .first()
            .map(|w| format!("：{}", w.detail))
            .unwrap_or_default();
        return Err(CoreError::Image(format!("所有图片都处理失败了{reason}")));
    }
    Ok(Report::with(outcome, warnings))
}

/// 压缩一张图，输出到 `out_dir`（名字由 `namer` 定，什么都不覆盖）。返回结果与要告诉
/// 用户的事：多页 TIFF 只处理了第一页、压完反而更大所以原样复制。
pub fn compress_file(
    path: &Path,
    out_dir: &Path,
    quality: &crate::imaging::ImageQuality,
    namer: &mut OutputNamer,
) -> Result<(Item, Vec<Warning>)> {
    let label = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let (item, dropped_pages) = compress_one(path, out_dir, quality, namer)?;
    let mut warnings = Vec::new();
    if dropped_pages > 0 {
        warnings.push(Warning::new(
            WarningKind::UnsupportedElement,
            format!(
                "{label} 是多页 TIFF（共 {} 页），本版本只处理第一页",
                dropped_pages + 1
            ),
        ));
    }
    if item.after >= item.before {
        warnings.push(Warning::new(
            WarningKind::ImageKeptOriginal,
            format!("{label}：压缩后反而更大，已原样复制"),
        ));
    }
    Ok((item, warnings))
}

/// 压缩一张图，返回结果与被丢掉的页数（多页 TIFF 只处理第一页）。
fn compress_one(
    path: &Path,
    out_dir: &Path,
    quality: &crate::imaging::ImageQuality,
    namer: &mut OutputNamer,
) -> Result<(Item, usize)> {
    let before = std::fs::metadata(path)
        .map_err(|e| CoreError::io(path, e))?
        .len();
    let original = std::fs::read(path).map_err(|e| CoreError::io(path, e))?;
    let source_ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin")
        .to_ascii_lowercase();
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let dropped_pages =
        if crate::imaging::probe::sniff(&original) == crate::imaging::probe::Container::Tiff {
            crate::imaging::probe::tiff_page_count(&original)
                .unwrap_or(1)
                .saturating_sub(1)
        } else {
            0
        };

    let img = crate::imaging::decode_oriented(&original)?;
    let (w, h) = (img.width(), img.height());

    // 批量压缩没有「页面」的概念，用 A4 幅面作参照换算目标像素，
    // 这样「上限 300 DPI」这类档位说明对用户才是有意义的。
    let (page_w, page_h) = crate::imaging::page_size_pt(w, h, None);
    let target = quality.downscale_target(w, h, page_w, page_h);

    // 无损档且不需要缩放 —— 没有任何可做的，直接复制。
    // 重新编码一遍只会白费 CPU，对 JPEG 源还会因为改存 PNG 而暴涨。
    if quality.lossless_only() && target.is_none() {
        let output = namer.name(out_dir, &stem, &source_ext);
        write_atomic(&output, &original)?;
        return Ok((
            Item {
                source: path.to_path_buf(),
                output,
                before,
                after: before,
            },
            dropped_pages,
        ));
    }

    let out_img = match target {
        Some((tw, th)) => crate::imaging::resize_image(&img, tw, th)?,
        None => img,
    };

    // 带透明通道的图不能转 JPEG —— 透明区域会变成黑块。这类一律走 PNG。
    let keeps_alpha = out_img.color().has_alpha();
    let (bytes, ext) = if keeps_alpha || quality.lossless_only() {
        let mut buf = std::io::Cursor::new(Vec::new());
        out_img
            .write_to(&mut buf, image::ImageFormat::Png)
            .map_err(|e| CoreError::Image(format!("PNG 编码失败：{e}")))?;
        (buf.into_inner(), "png".to_string())
    } else {
        (
            crate::imaging::encode_jpeg_image(
                &out_img,
                quality.jpeg_quality,
                quality.grayscale || crate::imaging::is_gray_source(&out_img),
            )?,
            "jpg".to_string(),
        )
    };

    // 压完更大就写回原始字节。用户要的是「更小」，不是「被我们处理过」。
    let (final_bytes, after, ext) = if bytes.len() as u64 >= before {
        (original, before, source_ext)
    } else {
        let n = bytes.len() as u64;
        (bytes, n, ext)
    };

    let output = namer.name(out_dir, &stem, &ext);
    write_atomic(&output, &final_bytes)?;

    Ok((
        Item {
            source: path.to_path_buf(),
            output,
            before,
            after,
        },
        dropped_pages,
    ))
}
