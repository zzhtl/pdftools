//! 多张图片合成一个 PDF。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::bail_if_cancelled;
use crate::error::{CoreError, Report, Result, Warning, WarningKind};
use crate::imaging::{prepare_for_pdf, read_time, ColorData, Fidelity, Tier};
use crate::pdf::writer::{DocBuilder, DocInfo, ImageData, ImageEncoding, PageSpec};
use crate::progress::{Progress, ProgressSink};
use crate::timestamp::{DatedFile, TimeSource, Timestamp};

pub struct Outcome {
    pub pdf: Vec<u8>,
    /// 每张图实际达到的保真度，顺序与输入一致。界面上按行显示徽章。
    pub fidelity: Vec<(PathBuf, Fidelity)>,
    /// 写进 PDF 的创建时间。
    pub creation: Option<Timestamp>,
    /// `creation` 是否来自真实的拍摄时间。为 false 时它只是导出时刻。
    pub creation_is_capture_time: bool,
    /// 有真实拍摄时间的那些图片里，最早与最晚的拍摄时刻。
    pub capture_span: Option<(Timestamp, Timestamp)>,
    /// 没有任何拍摄时间信息的图片数量。
    pub without_capture_time: usize,
}

impl std::fmt::Debug for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Outcome")
            .field("pdf_bytes", &self.pdf.len())
            .field("fidelity", &self.fidelity)
            .field("creation", &self.creation)
            .finish()
    }
}

/// 按给定顺序把图片合成 PDF。页面尺寸逐张跟随图片。
/// 按给定顺序把图片合成 PDF。
///
/// `manual_times` 是用户手动指定的拍摄时间。文件里的拍摄时间被剥掉时，
/// 这是唯一能让 PDF 带上正确时间的途径。
pub fn run(
    paths: &[PathBuf],
    tier: Tier,
    manual_times: &HashMap<PathBuf, Timestamp>,
    sink: &dyn ProgressSink,
) -> Result<Report<Outcome>> {
    if paths.is_empty() {
        return Err(CoreError::Image("没有选择任何图片".into()));
    }

    let quality = tier.for_images();
    let mut doc = DocBuilder::new();
    let mut warnings = Vec::new();
    let mut fidelity = Vec::new();
    // 时间对取证场景很关键：拍摄时间要一路带进 PDF 的文档属性。
    let mut times: Vec<DatedFile> = Vec::new();

    sink.emit(Progress::Started { total: paths.len() });

    for (i, path) in paths.iter().enumerate() {
        bail_if_cancelled!(sink);

        let label = file_label(path);
        // 单张图失败不能拖垮整批 —— 用户选了 200 张，不该因为其中一张损坏就全废。
        let prepared = match prepare_for_pdf(path, &quality) {
            Ok(p) => p,
            Err(e) => {
                warnings.push(Warning::new(
                    WarningKind::ItemFailed,
                    format!("{label}：{e}"),
                ));
                sink.emit(Progress::Item {
                    done: i + 1,
                    total: paths.len(),
                    label,
                });
                continue;
            }
        };

        fidelity.push((path.clone(), prepared.fidelity));
        if prepared.dropped_pages > 0 {
            warnings.push(Warning::new(
                WarningKind::UnsupportedElement,
                format!(
                    "{label} 是多页 TIFF（共 {} 页），本版本只转换第一页",
                    prepared.dropped_pages + 1
                ),
            ));
        }
        // 手动指定优先于文件里读到的。
        let dated = match manual_times.get(path) {
            Some(when) => Some(DatedFile {
                when: *when,
                source: TimeSource::Manual,
            }),
            None => read_time(path),
        };
        if let Some(t) = dated {
            times.push(t);
        }

        let (encoding, gray) = match &prepared.color {
            ColorData::Jpeg { bytes, gray } => (ImageEncoding::Jpeg(bytes), *gray),
            ColorData::Raw { bytes, gray } => (ImageEncoding::Raw(bytes), *gray),
        };
        let image_ref = doc.add_image(&ImageData {
            width: prepared.width,
            height: prepared.height,
            gray,
            encoding,
            alpha: prepared.alpha.as_deref(),
        });

        let mut page = PageSpec::new(prepared.page_w_pt, prepared.page_h_pt);
        page.images.push(("Im0".into(), image_ref));
        // image XObject 的坐标系是 1×1 的单位方块，靠 cm 矩阵拉伸到整页。
        page.content.save_state();
        page.content.transform(prepared.placement_matrix());
        page.content.x_object(pdf_writer::Name(b"Im0"));
        page.content.restore_state();
        doc.add_page(page);

        sink.emit(Progress::Item {
            done: i + 1,
            total: paths.len(),
            label,
        });
    }

    if doc.page_count() == 0 {
        // 把第一条具体原因带上。只说「都失败了」等于让用户自己猜是格式不对、
        // 文件损坏还是权限问题。
        let reason = warnings
            .first()
            .map(|w| format!("：{}", w.detail))
            .unwrap_or_default();
        return Err(CoreError::Image(format!("所有图片都处理失败了{reason}")));
    }

    // 只有真正的拍摄时间才有资格填 /CreationDate。文件系统时间一复制就被刷新，
    // 拿它冒充拍摄时刻等于往证据材料里塞一个伪造的日期。
    let mut captures: Vec<Timestamp> = times
        .iter()
        .filter(|t| t.source.is_capture_time())
        .map(|t| t.when)
        .collect();
    captures.sort();
    let capture_span = captures.first().zip(captures.last()).map(|(a, b)| (*a, *b));
    let without_capture_time = times.len().saturating_sub(captures.len());

    let creation_is_capture_time = capture_span.is_some();
    let creation = match capture_span {
        Some((first, _)) => first,
        // 一个拍摄时间都没有：按 PDF 规范的本义，/CreationDate 就记这份文件的生成时刻。
        None => Timestamp::now(),
    };

    if without_capture_time > 0 {
        warnings.push(Warning::new(
            WarningKind::CaptureTimeMissing,
            if creation_is_capture_time {
                format!(
                    "{without_capture_time} 张图片没有拍摄时间信息（EXIF / XMP / IPTC 里都没有）",
                )
            } else {
                "所有图片都没有拍摄时间信息。PDF 的创建时间记的是本次导出时刻，\
                 不是拍摄时刻 —— 需要真实拍摄时间的话，请用手机相册里的原图，\
                 或在列表里手动填写。"
                    .to_string()
            },
        ));
    }

    doc.set_info(DocInfo {
        title: None,
        subject: Some(match capture_span {
            Some((a, b)) if a == b => {
                format!("共 {} 张图片，拍摄于 {}", fidelity.len(), a.display())
            }
            Some((a, b)) => format!(
                "共 {} 张图片，拍摄时间 {} 至 {}",
                fidelity.len(),
                a.display(),
                b.display()
            ),
            None => format!("共 {} 张图片；原始文件中没有拍摄时间信息", fidelity.len()),
        }),
        creation: Some(creation),
        modified: Some(Timestamp::now()),
    });

    let pdf = doc.finish()?;
    Ok(Report::with(
        Outcome {
            pdf,
            fidelity,
            creation: Some(creation),
            creation_is_capture_time,
            capture_span,
            without_capture_time,
        },
        warnings,
    ))
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}
