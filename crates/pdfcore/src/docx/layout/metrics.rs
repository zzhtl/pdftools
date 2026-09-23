//! 行框：一行多高、基线在行框里的哪个位置。纯函数，只依赖字体度量与段落设置。

use super::calib::{Calib, GridLayout, PageBottom};
use crate::docx::ir::{Grid, LineSpacing};

/// 一行的竖向度量，单位点。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct LineBox {
    /// 行框高度：本行占掉的竖向空间。
    pub height: f32,
    /// 基线到行框顶部的距离。
    pub baseline: f32,
    /// 页底要容得下的高度。见 [`PageBottom`]。
    pub fit_height: f32,
}

/// `unsnapped` 是行内字体的自然行高（吸附前，取最大者），`ascent` 是最大上伸。
/// `snap` 表示段落参与行网格吸附（且文档有网格）。
pub(super) fn line_box(
    unsnapped: f32,
    ascent: f32,
    grid: Option<Grid>,
    snap: bool,
    spacing: LineSpacing,
    is_last_line: bool,
    calib: &Calib,
) -> LineBox {
    // 行网格：单倍行高先向上吸附到网格整数倍，倍数再乘在这之上。
    // 漏掉这一步，中文文档的行密度会比 Word 高出近一倍。
    let natural = match grid {
        Some(g) if snap => g.snap(unsnapped),
        _ => unsnapped,
    };

    let height = match spacing {
        // 段落的**最后一行**，倍数带来的额外行距按**吸附前**的自然行高算，
        // 而不是吸附后的。这条实测自 LibreOffice，1.0 / 1.3 / 1.5 / 2.0
        // 四个倍数全部吻合；没有网格时 natural == unsnapped，公式自然退化
        // 成 natural × m。
        //
        // 不区分这一条，每个段落边界都会多出 (m-1) × 吸附增量，
        // 段落密集的文档累积下来会平白多出一整页。
        LineSpacing::Multiple(m) if is_last_line => natural + (m - 1.0).max(0.0) * unsnapped,
        LineSpacing::Multiple(m) => natural * m,
        LineSpacing::Exact(pt) => pt,
        LineSpacing::AtLeast(pt) => natural.max(pt),
    };

    // 基线在行框里的位置。
    //
    // **倍数**带来的额外行距加在基线下方 —— 实测参照的首基线位置在 1.0 倍和 1.3 倍
    // 行距下完全相同，说明倍数不影响基线在行框内的位置；按 height/natural 等比缩放
    // ascent 的话，基线比参照高 8.6pt，整页文字随之上移。
    //
    // **吸附**多出来的空间怎么分，见 [`GridLayout`]。
    let snap_extra = (natural - unsnapped).max(0.0);
    let above = match calib.grid {
        GridLayout::Legacy => snap_extra,
        GridLayout::Centered => snap_extra / 2.0,
    };
    // 倍数多出来的空白都在文字下方（见上），它越不越过页底由规则决定。
    let fit_height = match (calib.page_bottom, spacing) {
        (PageBottom::TextOnly, LineSpacing::Multiple(_)) => height.min(natural),
        _ => height,
    };
    LineBox {
        height,
        baseline: ascent + above,
        fit_height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRID: Option<Grid> = Some(Grid { pitch_pt: 15.6 });

    /// 31.2 / 40.56 / 段落边界约 36.4 是对着 LibreOffice 标定过的值
    /// （行网格 15.6pt、12pt 中文正文）。输入取 12pt 中文字体量级的度量。
    #[test]
    fn calibrated_values() {
        let (nat, asc) = (17.388, 13.812);
        let cases = [
            // (网格, 吸附, 行距, 末行, 高度, 基线)
            (GRID, true, LineSpacing::Multiple(1.0), false, 31.2, 27.624),
            (GRID, true, LineSpacing::Multiple(1.3), false, 40.56, 27.624),
            // 段落最后一行：倍数的额外部分按吸附前的自然行高算。
            (
                GRID,
                true,
                LineSpacing::Multiple(1.3),
                true,
                36.4164,
                27.624,
            ),
            // 段落不参与吸附。
            (
                GRID,
                false,
                LineSpacing::Multiple(1.0),
                false,
                17.388,
                13.812,
            ),
            (None, true, LineSpacing::Multiple(1.5), true, 26.082, 13.812),
            (GRID, true, LineSpacing::Exact(20.0), false, 20.0, 27.624),
            (GRID, true, LineSpacing::AtLeast(40.0), false, 40.0, 27.624),
            (GRID, true, LineSpacing::AtLeast(10.0), false, 31.2, 27.624),
        ];
        for (grid, snap, spacing, last, height, baseline) in cases {
            let b = line_box(nat, asc, grid, snap, spacing, last, &Calib::legacy());
            assert!(
                (b.height - height).abs() < 1e-3 && (b.baseline - baseline).abs() < 1e-3,
                "{grid:?} snap={snap} {spacing:?} last={last}: 得到 {b:?}，应为 ({height}, {baseline})"
            );
        }
    }

    /// 文字在所占的整格里上下居中（LibreOffice 实测，见 `GridLayout::Centered`）。
    #[test]
    fn text_is_centered_in_its_grid_cells() {
        let calib = Calib {
            grid: GridLayout::Centered,
            ..Calib::legacy()
        };
        // 12pt：自然行高 17.388 占 2 格；16pt 的 23.184 也占 2 格；24pt 的 34.776 占 3 格。
        for (nat, asc, height, baseline) in [
            (17.388, 13.812, 31.2, 13.812 + (31.2 - 17.388) / 2.0),
            (23.184, 18.416, 31.2, 18.416 + (31.2 - 23.184) / 2.0),
            (34.776, 27.624, 46.8, 27.624 + (46.8 - 34.776) / 2.0),
        ] {
            let b = line_box(
                nat,
                asc,
                GRID,
                true,
                LineSpacing::Multiple(1.0),
                false,
                &calib,
            );
            assert!(
                (b.height - height).abs() < 1e-3 && (b.baseline - baseline).abs() < 1e-3,
                "{nat}: {b:?}"
            );
        }
        // 不吸附时没有多出来的空间可分。
        let b = line_box(
            17.388,
            13.812,
            GRID,
            false,
            LineSpacing::Multiple(1.0),
            false,
            &calib,
        );
        assert!((b.baseline - 13.812).abs() < 1e-4, "{b:?}");
    }

    /// 自然行高正好落在网格整数倍上时不再多占一格。
    #[test]
    fn exact_multiple_of_the_grid_is_not_rounded_up() {
        let b = line_box(
            15.6,
            12.0,
            GRID,
            true,
            LineSpacing::Multiple(1.0),
            false,
            &Calib::legacy(),
        );
        assert!((b.height - 15.6).abs() < 1e-4, "{b:?}");
        assert!((b.baseline - 12.0).abs() < 1e-4, "{b:?}");
    }
}
