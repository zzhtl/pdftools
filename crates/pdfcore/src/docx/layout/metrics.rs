//! 行框：一行多高、基线在行框里的哪个位置。纯函数，只依赖字体度量与段落设置。

use crate::docx::ir::{Grid, LineSpacing};

/// 一行的竖向度量，单位点。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct LineBox {
    /// 行框高度：本行占掉的竖向空间。
    pub height: f32,
    /// 基线到行框顶部的距离。
    pub baseline: f32,
    /// 页底要容得下的高度，见 `rules` 模块「页底」一节。
    pub fit_height: f32,
}

/// 一行里的字与行内对象（图片）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct LineContent {
    /// 文字的自然行高（吸附前，取最大者）与最大上伸。只有对象的行是段落标记的，
    /// 只用来算行距倍数多出来的部分。
    pub unsnapped: f32,
    pub ascent: f32,
    /// 最高的对象有多高（底边在基线上）。没有对象是 0。
    pub object: f32,
    /// 行里有文字；没有文字的行只由对象撑起来，没有下伸。
    pub has_text: bool,
}

impl LineContent {
    /// 只有文字的行。
    pub fn text(unsnapped: f32, ascent: f32) -> Self {
        Self {
            unsnapped,
            ascent,
            object: 0.0,
            has_text: true,
        }
    }
}

/// `snap` 表示段落参与行网格吸附（且文档有网格）。行内对象的规则见 `rules` 模块
/// 「图片、形状与文本框」一节。
pub(super) fn line_box(
    content: LineContent,
    grid: Option<Grid>,
    snap: bool,
    spacing: LineSpacing,
    is_last_line: bool,
) -> LineBox {
    let unsnapped = content.unsnapped;
    // 对象的底边在基线上：它比文字高出的部分加在行的上伸里，下伸只看文字。
    let (text_ascent, text_descent) = if content.has_text {
        (content.ascent, unsnapped - content.ascent)
    } else {
        (0.0, 0.0)
    };
    let ascent = text_ascent.max(content.object);
    let filled = ascent + text_descent;
    let snapped = |h: f32| match grid {
        Some(g) if snap => g.snap(h),
        _ => h,
    };
    // 行网格：单倍行高先向上吸附到网格整数倍，倍数再乘在这之上。
    // 漏掉这一步，中文文档的行密度会比 Word 高出近一倍。
    let natural = snapped(filled);

    let height = match spacing {
        // 段落的**最后一行**，倍数带来的额外行距按**吸附前**的自然行高算，
        // 而不是吸附后的。这条实测自 LibreOffice，1.0 / 1.3 / 1.5 / 2.0
        // 四个倍数全部吻合；没有网格时 natural == unsnapped，公式自然退化
        // 成 natural × m。
        //
        // 不区分这一条，每个段落边界都会多出 (m-1) × 吸附增量，
        // 段落密集的文档累积下来会平白多出一整页。
        //
        // 倍数多出来的部分只按文字算：一行里有张图，行不会跟着图成倍地变高。
        LineSpacing::Multiple(m) if is_last_line => natural + (m - 1.0).max(0.0) * unsnapped,
        LineSpacing::Multiple(m) => natural + (m - 1.0) * snapped(unsnapped),
        LineSpacing::Exact(pt) => pt,
        LineSpacing::AtLeast(pt) => natural.max(pt),
    };

    // 基线在行框里的位置。
    //
    // **倍数**带来的额外行距加在基线下方 —— 实测参照的首基线位置在 1.0 倍和 1.3 倍
    // 行距下完全相同，说明倍数不影响基线在行框内的位置；按 height/natural 等比缩放
    // ascent 的话，基线比参照高 8.6pt，整页文字随之上移。
    //
    // **吸附**多出来的空间：文字在所占的整格里上下居中，见 `rules` 模块「行网格」一节。
    // 图比字高时，图顶在行顶，吸附多出来的都在下面（LibreOffice 实测）。
    let snap_extra = (natural - filled).max(0.0);
    let above = if content.object > text_ascent {
        0.0
    } else {
        snap_extra / 2.0
    };
    // 见 `rules` 模块「固定行距与最小行距的基线」一节。
    let baseline = match spacing {
        LineSpacing::Exact(pt) => 0.8 * pt,
        LineSpacing::AtLeast(pt) if pt > natural => ascent + above + (pt - natural),
        _ => ascent + above,
    };
    // 倍数多出来的空白都在文字下方（见上），可以越过页底，见 `rules` 模块「页底」一节。
    let fit_height = match spacing {
        LineSpacing::Multiple(_) => height.min(natural),
        _ => height,
    };
    LineBox {
        height,
        baseline,
        fit_height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRID: Option<Grid> = Some(Grid { pitch_pt: 15.6 });

    /// 31.2 / 40.56 / 段落边界约 36.4 是对着 LibreOffice 标定过的值
    /// （行网格 15.6pt、12pt 中文正文）。输入取 12pt 中文字体量级的度量。
    /// 吸附到两格时多出 13.812，上下各一半，基线在 13.812 + 6.906。
    #[test]
    fn calibrated_values() {
        let (nat, asc) = (17.388, 13.812);
        let cases = [
            // (网格, 吸附, 行距, 末行, 高度, 基线)
            (GRID, true, LineSpacing::Multiple(1.0), false, 31.2, 20.718),
            (GRID, true, LineSpacing::Multiple(1.3), false, 40.56, 20.718),
            // 段落最后一行：倍数的额外部分按吸附前的自然行高算。
            (
                GRID,
                true,
                LineSpacing::Multiple(1.3),
                true,
                36.4164,
                20.718,
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
            (GRID, true, LineSpacing::Exact(20.0), false, 20.0, 16.0),
            (GRID, true, LineSpacing::AtLeast(40.0), false, 40.0, 29.518),
            (GRID, true, LineSpacing::AtLeast(10.0), false, 31.2, 20.718),
        ];
        for (grid, snap, spacing, last, height, baseline) in cases {
            let b = line_box(LineContent::text(nat, asc), grid, snap, spacing, last);
            assert!(
                (b.height - height).abs() < 1e-3 && (b.baseline - baseline).abs() < 1e-3,
                "{grid:?} snap={snap} {spacing:?} last={last}: 得到 {b:?}，应为 ({height}, {baseline})"
            );
        }
    }

    /// 文字在所占的整格里上下居中（LibreOffice 实测）。
    #[test]
    fn text_is_centered_in_its_grid_cells() {
        // 12pt：自然行高 17.388 占 2 格；16pt 的 23.184 也占 2 格；24pt 的 34.776 占 3 格。
        for (nat, asc, height, baseline) in [
            (17.388, 13.812, 31.2, 13.812 + (31.2 - 17.388) / 2.0),
            (23.184, 18.416, 31.2, 18.416 + (31.2 - 23.184) / 2.0),
            (34.776, 27.624, 46.8, 27.624 + (46.8 - 34.776) / 2.0),
        ] {
            let b = line_box(
                LineContent::text(nat, asc),
                GRID,
                true,
                LineSpacing::Multiple(1.0),
                false,
            );
            assert!(
                (b.height - height).abs() < 1e-3 && (b.baseline - baseline).abs() < 1e-3,
                "{nat}: {b:?}"
            );
        }
        // 不吸附时没有多出来的空间可分。
        let b = line_box(
            LineContent::text(17.388, 13.812),
            GRID,
            false,
            LineSpacing::Multiple(1.0),
            false,
        );
        assert!((b.baseline - 13.812).abs() < 1e-4, "{b:?}");
    }

    /// 固定行距：基线在行高的 80% 处；最小行距撑高时多出的高度在文字上方。
    #[test]
    fn fixed_and_at_least_spacing_place_the_baseline_like_the_reference() {
        let (nat, asc) = (17.244, 13.812);
        for (spacing, height, baseline) in [
            (LineSpacing::Exact(30.0), 30.0, 24.0),
            (LineSpacing::Exact(10.0), 10.0, 8.0),
            (LineSpacing::AtLeast(30.0), 30.0, 13.812 + (30.0 - 17.244)),
            // 自然行高已经够高：与单倍行距一样。
            (LineSpacing::AtLeast(10.0), 17.244, 13.812),
        ] {
            let b = line_box(LineContent::text(nat, asc), None, false, spacing, false);
            assert!(
                (b.height - height).abs() < 1e-3 && (b.baseline - baseline).abs() < 1e-3,
                "{spacing:?}: {b:?}"
            );
        }
    }

    /// 自然行高正好落在网格整数倍上时不再多占一格。
    #[test]
    fn exact_multiple_of_the_grid_is_not_rounded_up() {
        let b = line_box(
            LineContent::text(15.6, 12.0),
            GRID,
            true,
            LineSpacing::Multiple(1.0),
            false,
        );
        assert!((b.height - 15.6).abs() < 1e-4, "{b:?}");
        assert!((b.baseline - 12.0).abs() < 1e-4, "{b:?}");
    }
}
