//! 分页：把测量好的块按顺序放进页面。

use super::para::{Line, ParaBody, ParaBox};
use super::Page;
use crate::docx::ir::PageGeom;

pub(super) struct Paginator<'a> {
    page: &'a PageGeom,
    pages: Vec<Page>,
    /// 当前页已用掉的垂直空间（从内容区顶部往下量）。
    used: f32,
}

impl<'a> Paginator<'a> {
    pub fn new(page: &'a PageGeom) -> Self {
        Self {
            page,
            pages: vec![Page::default()],
            used: 0.0,
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
        self.used += para.space_before;

        match &para.body {
            ParaBody::Empty { height } => {
                self.used += height;
            }
            ParaBody::Lines(lines) => {
                let mut next = 0;
                while next < lines.len() {
                    let n = fit_lines(&lines[next..], self.used, self.page.content_height());
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
    }

    fn commit(&mut self, line: &Line) {
        let base = self.page.h_pt - self.page.margin_top - self.used - line.baseline;
        let page = self.pages.last_mut().expect("至少有一页");
        page.ops.extend(line.ops.iter().map(|op| op.shifted(base)));
        self.used += line.height;
    }
}

/// 从已用高度 `used` 开始，当前页还放得下前几行。
///
/// 页首（什么都还没放）的那一行无论多高都放得下 —— 否则一行比整页还高时永远排不出去。
fn fit_lines(lines: &[Line], mut used: f32, content_height: f32) -> usize {
    let mut n = 0;
    for line in lines {
        if used + line.height > content_height && used > f32::EPSILON {
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

    #[test]
    fn a_line_taller_than_the_page_still_goes_on_an_empty_page() {
        let l = lines(&[500.0, 20.0]);
        assert_eq!(fit_lines(&l, 0.0, 100.0), 1);
        assert_eq!(fit_lines(&l, 10.0, 100.0), 0);
    }
}
