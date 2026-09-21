//! 多张图片合成一个 PDF。

use std::path::{Path, PathBuf};

use crate::bail_if_cancelled;
use crate::error::{CoreError, Report, Result, Warning, WarningKind};
use crate::imaging::{prepare_for_pdf, Fidelity, Tier};
use crate::pdf::writer::{image::write_image, DocBuilder, PageSpec};
use crate::progress::{Progress, ProgressSink};

pub struct Outcome {
    pub pdf: Vec<u8>,
    /// 每张图实际达到的保真度，顺序与输入一致。界面上按行显示徽章。
    pub fidelity: Vec<(PathBuf, Fidelity)>,
}

impl std::fmt::Debug for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Outcome")
            .field("pdf_bytes", &self.pdf.len())
            .field("fidelity", &self.fidelity)
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

        let image_ref = {
            let (pdf, alloc) = doc.parts();
            write_image(pdf, alloc, &prepared)
        };

        let mut page = PageSpec::new(prepared.page_w_pt, prepared.page_h_pt);
        page.images.push(("Im0".into(), image_ref));
        // image XObject 的坐标系是 1×1 的单位方块，靠 cm 矩阵拉伸到整页。
        page.content.save_state();
        page.content
            .transform([prepared.page_w_pt, 0.0, 0.0, prepared.page_h_pt, 0.0, 0.0]);
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

    let pdf = doc.finish()?;
    Ok(Report::with(Outcome { pdf, fidelity }, warnings))
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}
