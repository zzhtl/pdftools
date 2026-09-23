//! 分页：把测量好的块按顺序放进页面。

use super::para::{Line, ParaBody, ParaBox};
use super::Page;
use crate::docx::ir::PageGeom;

pub(super) struct Paginator<'a> {
    page: &'a PageGeom,
    /// 正文区的起点离版心顶端多远。有行网格时网格在版心里居中，起点就不在顶端。
    origin: f32,
    /// 正文区的高度。
    capacity: f32,
    pages: Vec<Page>,
    /// 当前页已用掉的垂直空间（从正文区顶部往下量）。
    used: f32,
    /// 段后距与下一段的段前距取较大值，而不是相加。
    collapse_spacing: bool,
    /// 当前页上刚加过的段后距。取较大值时，下一段的段前距只补差额。
    last_after: f32,
}

impl<'a> Paginator<'a> {
    /// `area` 是正文区：(离版心顶端的偏移, 高度)。
    pub fn new(page: &'a PageGeom, (origin, capacity): (f32, f32), collapse_spacing: bool) -> Self {
        Self {
            page,
            origin,
            capacity,
            pages: vec![Page::default()],
            used: 0.0,
            collapse_spacing,
            last_after: 0.0,
        }
    }

    pub fn page_index(&self) -> usize {
        self.pages.len() - 1
    }

    pub fn finish(self) -> Vec<Page> {
        self.pages
    }

    fn new_page(&mut self) {
        self.pages.push(Page::default());
        self.used = 0.0;
        self.last_after = 0.0;
    }

    fn at_page_top(&self) -> bool {
        self.used <= f32::EPSILON
    }

    pub fn place_para(&mut self, para: &ParaBox) {
        // 只有文档第一页的页首不另起新页。与重写前一致。
        if para.page_break_before && !(self.at_page_top() && self.pages.len() == 1) {
            self.new_page();
        }
        // 段前距在页首也照常生效。
        //
        // 在页首吃掉段前距是 HTML 的习惯，LibreOffice 并不这么做：同一份文档，
        // 参照的首行基线距正文顶 40.3pt，而吃掉段前距只有 24.1pt，整页内容整体上移一截。
        if self.collapse_spacing {
            self.used += (para.space_before - self.last_after).max(0.0);
        } else {
            self.used += para.space_before;
        }

        match &para.body {
            ParaBody::Empty { height } => {
                self.used += height;
            }
            ParaBody::Lines(lines) => {
                let mut next = 0;
                while next < lines.len() {
                    let n = fit_lines(&lines[next..], self.used, self.capacity);
                    if n == 0 {
                        self.new_page();
                        continue;
                    }
                    for line in &lines[next..next + n] {
                        self.commit(line);
                    }
                    next += n;
                }
            }
        }
        self.used += para.space_after;
        self.last_after = para.space_after;
    }

    fn commit(&mut self, line: &Line) {
        self.last_after = 0.0;
        let base = self.page.h_pt - self.page.margin_top - self.origin - self.used - line.baseline;
        let page = self.pages.last_mut().expect("至少有一页");
        page.ops.extend(line.ops.iter().map(|op| op.shifted(base)));
        self.used += line.height;
    }
}

/// 放不放得下的判断允许的误差。行网格下行高都是格高的整数倍，22 行 31.2pt
/// 在 f32 里累加出来会比网格区的 686.4pt 多一点点，不留余量就会少排一行。
const FIT_TOLERANCE: f32 = 1e-3;

/// 从已用高度 `used` 开始，当前页还放得下前几行。
///
/// 页首（什么都还没放）的那一行无论多高都放得下 —— 否则一行比整页还高时永远排不出去。
fn fit_lines(lines: &[Line], mut used: f32, content_height: f32) -> usize {
    let mut n = 0;
    for line in lines {
        if used + line.fit_height > content_height + FIT_TOLERANCE && used > f32::EPSILON {
            break;
        }
        used += line.height;
        n += 1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(heights: &[f32]) -> Vec<Line> {
        heights
            .iter()
            .map(|&height| Line {
                height,
                baseline: height * 0.8,
                fit_height: height,
                ops: Vec::new(),
            })
            .collect()
    }

    #[test]
    fn fits_until_the_next_line_would_cross_the_bottom() {
        let l = lines(&[20.0, 20.0, 20.0]);
        assert_eq!(fit_lines(&l, 0.0, 60.0), 3);
        assert_eq!(fit_lines(&l, 0.0, 59.9), 2);
        assert_eq!(fit_lines(&l, 45.0, 60.0), 0);
    }

    /// 网格区正好放满 22 行：浮点累加误差不能让最后一行被挤到下一页。
    /// 行高与网格区都按排版时的同一算法求（A4、上下边距 72pt、格高 15.6pt）。
    #[test]
    fn lines_that_exactly_fill_the_grid_area_all_fit() {
        let grid = crate::docx::ir::Grid { pitch_pt: 15.6 };
        let area = (697.9f32 / grid.pitch_pt).floor() * grid.pitch_pt;
        let l = lines(&[grid.snap(17.388); 23]);
        assert_eq!(fit_lines(&l, 0.0, area), 22);
    }

    /// 行距倍数在文字下方多出来的空白可以越过页底。
    #[test]
    fn only_the_text_part_of_the_last_line_has_to_fit() {
        let line = |height, fit_height| Line {
            height,
            baseline: 20.0,
            fit_height,
            ops: Vec::new(),
        };
        let l = [line(40.56, 31.2), line(40.56, 31.2)];
        assert_eq!(fit_lines(&l, 0.0, 72.0), 2);
        assert_eq!(fit_lines(&l, 0.0, 71.0), 1);
    }

    #[test]
    fn a_line_taller_than_the_page_still_goes_on_an_empty_page() {
        let l = lines(&[500.0, 20.0]);
        assert_eq!(fit_lines(&l, 0.0, 100.0), 1);
        assert_eq!(fit_lines(&l, 10.0, 100.0), 0);
    }
}
