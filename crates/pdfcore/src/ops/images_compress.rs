//! 图片批量压缩。复用 `imaging` 的同一条流水线，输出到目标目录，不覆盖原文件。

use std::path::{Path, PathBuf};

use crate::bail_if_cancelled;
use crate::error::{CoreError, Report, Result, Warning, WarningKind};
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

    sink.emit(Progress::Started { total: paths.len() });

    for (i, path) in paths.iter().enumerate() {
        bail_if_cancelled!(sink);
        let label = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();

        match compress_one(path, out_dir, &quality) {
            Ok(item) => {
                if item.after >= item.before {
                    warnings.push(Warning::new(
                        WarningKind::ImageKeptOriginal,
                        format!("{label}：压缩后反而更大，已原样复制"),
                    ));
                }
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
        return Err(CoreError::Image("所有图片都处理失败了".into()));
    }
    Ok(Report::with(outcome, warnings))
}

fn compress_one(
    path: &Path,
    out_dir: &Path,
    quality: &crate::imaging::ImageQuality,
) -> Result<Item> {
    let before = std::fs::metadata(path)
        .map_err(|e| CoreError::io(path, e))?
        .len();
    let original = std::fs::read(path).map_err(|e| CoreError::io(path, e))?;
    let source_ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin")
        .to_ascii_lowercase();

    let img = image::load_from_memory(&original)
        .map_err(|e| CoreError::Image(format!("解码失败：{e}")))?;
    let (w, h) = (img.width(), img.height());

    // 批量压缩没有「页面」的概念，用 A4 幅面作参照换算目标像素，
    // 这样「上限 300 DPI」这类档位说明对用户才是有意义的。
    let (page_w, page_h) = crate::imaging::page_size_pt(w, h, None);
    let target = quality.downscale_target(w, h, page_w, page_h);

    // 无损档且不需要缩放 —— 没有任何可做的，直接复制。
    // 重新编码一遍只会白费 CPU，对 JPEG 源还会因为改存 PNG 而暴涨。
    if quality.lossless_only() && target.is_none() {
        let output = out_dir.join(format!(
            "{}.{source_ext}",
            path.file_stem().unwrap_or_default().to_string_lossy()
        ));
        std::fs::write(&output, &original).map_err(|e| CoreError::io(&output, e))?;
        return Ok(Item {
            source: path.to_path_buf(),
            output,
            before,
            after: before,
        });
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
            crate::imaging::encode_jpeg_image(&out_img, quality.jpeg_quality, quality.grayscale)?,
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

    let output = out_dir.join(format!(
        "{}.{ext}",
        path.file_stem().unwrap_or_default().to_string_lossy()
    ));
    std::fs::write(&output, &final_bytes).map_err(|e| CoreError::io(&output, e))?;

    Ok(Item {
        source: path.to_path_buf(),
        output,
        before,
        after,
    })
}
