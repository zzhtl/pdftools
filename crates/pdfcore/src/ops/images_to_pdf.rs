//! 多张图片合成一个 PDF。

use std::path::{Path, PathBuf};

use crate::bail_if_cancelled;
use crate::error::{CoreError, Report, Result, Warning, WarningKind};
use crate::imaging::{prepare_for_pdf, read_time, Fidelity, Tier};
use crate::pdf::writer::{image::write_image, DocBuilder, DocInfo, PageSpec};
use crate::progress::{Progress, ProgressSink};
use crate::timestamp::{DatedFile, TimeSource, Timestamp};

pub struct Outcome {
    pub pdf: Vec<u8>,
    /// 每张图实际达到的保真度，顺序与输入一致。界面上按行显示徽章。
    pub fidelity: Vec<(PathBuf, Fidelity)>,
    /// 写进 PDF 的创建时间（最早一张照片的拍摄时间）。
    pub creation: Option<Timestamp>,
    /// 参与合成的图片里，时间的最早与最晚值。
    pub time_span: Option<(Timestamp, Timestamp)>,
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
pub fn run(paths: &[PathBuf], tier: Tier, sink: &dyn ProgressSink) -> Result<Report<Outcome>> {
    if paths.is_empty() {
        return Err(CoreError::Image("没有选择任何图片".into()));
    }

    let quality = tier.for_images();
    let mut doc = DocBuilder::new();
    let mut warnings = Vec::new();
    let mut fidelity = Vec::new();
    // 时间对取证场景很关键：拍摄时间要一路带进 PDF 的文档属性。
    let mut times: Vec<DatedFile> = Vec::new();
    let mut only_file_times = true;

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
        if let Some(t) = read_time(path) {
            if t.source == TimeSource::Captured {
                only_file_times = false;
            }
            times.push(t);
        }

        let image_ref = {
            let (pdf, alloc) = doc.parts();
            write_image(pdf, alloc, &prepared)
        };

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
        return Err(CoreError::Image("所有图片都处理失败了".into()));
    }

    times.sort_by_key(|t| t.when);
    let time_span = times
        .first()
        .zip(times.last())
        .map(|(a, b)| (a.when, b.when));
    let creation = time_span.map(|(first, _)| first);

    if !times.is_empty() && only_file_times {
        // 说明所有图片都没有 EXIF 拍摄时间。用户以为记下的是拍摄时刻，
        // 实际只是文件修改时间，这个差别在取证时是要命的，必须说出来。
        warnings.push(Warning::new(
            WarningKind::CaptureTimeMissing,
            "所有图片都没有 EXIF 拍摄时间，PDF 里记录的是文件修改时间（复制、导出都会改变它）",
        ));
    }

    doc.set_info(DocInfo {
        title: None,
        subject: time_span.map(|(a, b)| {
            // 措辞必须跟着时间来源走：把文件修改时间说成「拍摄于」是在误导，
            // 而这份 PDF 可能是要拿去举证的。
            let kind = if only_file_times {
                "文件时间"
            } else {
                "拍摄时间"
            };
            if a == b {
                format!("共 {} 张图片，{kind} {}", fidelity.len(), a.display())
            } else {
                format!(
                    "共 {} 张图片，{kind} {} 至 {}",
                    fidelity.len(),
                    a.display(),
                    b.display()
                )
            }
        }),
        creation,
        modified: Some(Timestamp::now()),
    });

    let pdf = doc.finish()?;
    Ok(Report::with(
        Outcome {
            pdf,
            fidelity,
            creation,
            time_span,
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
