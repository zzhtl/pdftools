//! 分页：把测量好的块按顺序放进页面。

use super::calib::{Calib, PageBreakBefore};
use super::para::{Line, ParaBody, ParaBox, ParaDecor};
use super::table::TableBox;
use super::{Page, PageKind, PaintOp};
use crate::docx::ir::{self, BorderStyle, PageGeom, SectionStart};

/// 一页的版面：纸张，以及其中的正文区。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Frame {
    pub page: PageGeom,
    /// 正文区的起点离版心顶端多远。有行网格时网格在版心里居中，起点就不在顶端。
    pub origin: f32,
    /// 正文区的高度。
    pub capacity: f32,
}

/// 一节里各类页面的版面：首页、偶数页的页眉页脚可以不同，正文区也就跟着不同。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Frames {
    pub default: Frame,
    pub first: Frame,
    pub even: Frame,
    /// 本节首页用单独的页眉页脚（`w:titlePg`）。
    pub title_page: bool,
    /// 偶数页用单独的页眉页脚（`w:evenAndOddHeaders`）。
    pub even_odd: bool,
}

impl Frames {
    pub fn uniform(frame: Frame) -> Self {
        Self {
            default: frame,
            first: frame,
            even: frame,
            title_page: false,
            even_odd: false,
        }
    }

    /// 本节第 `first` 页（是否首页）、页码为 `number` 的页面用哪类。
    fn pick(&self, first: bool, number: i32) -> (Frame, PageKind) {
        if first && self.title_page {
            (self.first, PageKind::First)
        } else if self.even_odd && number.rem_euclid(2) == 0 {
            (self.even, PageKind::Even)
        } else {
            (self.default, PageKind::Default)
        }
    }
}

pub(super) struct Paginator {
    /// 当前页的版面。
    frame: Frame,
    /// 之后新开的页用的版面。连续分节换了设置时，当前页仍用旧的。
    frames: Frames,
    /// 当前是第几节。
    section: usize,
    /// 下一个新开的页是本节的第一页。
    section_start: bool,
    /// 下一页的页码从这里重新起头（`w:pgNumType/@w:start`）。
    restart: Option<i32>,
    pages: Vec<Page>,
    /// 当前页已用掉的垂直空间（从正文区顶部往下量）。
    used: f32,
    /// 段后距与下一段的段前距取较大值，而不是相加。
    collapse_spacing: bool,
    /// 当前页上刚加过的段后距。取较大值时，下一段的段前距只补差额。
    last_after: f32,
    page_break_before: PageBreakBefore,
    /// 正在画的段落框。同一组的段落共用一个框；换页时在旧页收口，新页上重新开。
    open: Option<OpenBox>,
}

struct OpenBox {
    decor: ParaDecor,
    /// 框顶在哪（与 `used` 同一个量法）。
    top: f32,
    /// 开框时本页已有几个绘制操作。底纹插在这个位置，画在文字下面。
    ops_at: usize,
}

impl Paginator {
    /// `first_number`：第一页的页码，没写是 1。
    pub fn new(
        frames: Frames,
        first_number: Option<i32>,
        collapse_spacing: bool,
        calib: &Calib,
    ) -> Self {
        let number = first_number.unwrap_or(1);
        let (frame, kind) = frames.pick(true, number);
        Self {
            frame,
            frames,
            section: 0,
            section_start: false,
            restart: None,
            pages: vec![Page::new(&frame.page, number, 0, kind)],
            used: 0.0,
            collapse_spacing,
            last_after: 0.0,
            page_break_before: calib.page_break_before,
            open: None,
        }
    }

    pub fn page_index(&self) -> usize {
        self.pages.len() - 1
    }

    pub fn finish(mut self) -> Vec<Page> {
        self.close_box();
        self.pages
    }

    fn new_page(&mut self) {
        self.close_box();
        let number = self
            .restart
            .take()
            .unwrap_or_else(|| self.pages.last().map_or(1, |p| p.number + 1));
        let (frame, kind) = self.frames.pick(self.section_start, number);
        self.section_start = false;
        self.frame = frame;
        self.pages
            .push(Page::new(&frame.page, number, self.section, kind));
        self.used = 0.0;
        self.last_after = 0.0;
    }

    /// 当前页已用掉的高度。
    pub fn used(&self) -> f32 {
        self.used
    }

    /// 开始新的一节。见 [`Sections::Each`](super::calib::Sections::Each)。
    pub fn start_section(
        &mut self,
        frames: Frames,
        start: SectionStart,
        number: Option<i32>,
        index: usize,
    ) {
        let same_paper = (frames.default.page.w_pt - self.frame.page.w_pt).abs() < 0.01
            && (frames.default.page.h_pt - self.frame.page.h_pt).abs() < 0.01;
        self.frames = frames;
        self.section = index;
        if start == SectionStart::Continuous && same_paper {
            return;
        }
        self.restart = number;
        self.section_start = true;
        self.new_page();
        let odd = self
            .pages
            .last()
            .is_some_and(|p| p.number.rem_euclid(2) == 1);
        let blank = match start {
            SectionStart::OddPage => !odd,
            SectionStart::EvenPage => odd,
            _ => false,
        };
        if blank {
            self.new_page();
        }
    }

    fn at_page_top(&self) -> bool {
        self.used <= f32::EPSILON
    }

    /// 段前距实际要加多少：与上一段的段后距取较大值时，只补差额。
    fn gap_before(&self, para: &ParaBox, last_after: f32) -> f32 {
        if self.collapse_spacing {
            (para.space_before - last_after).max(0.0)
        } else {
            para.space_before
        }
    }

    /// 与下段同页：`chain` 里的各段整段、再加 `next` 的第一行，当前页放不下、
    /// 又不在页首时，先换页。在页首就照排 —— 比一页还长的串只能被断开。
    pub fn keep_together(&mut self, chain: &[&ParaBox], next: Option<&ParaBox>) {
        if self.at_page_top() || chain.first().is_some_and(|p| p.page_break_before) {
            return;
        }
        let mut last_after = self.last_after;
        let mut need = 0.0;
        for p in chain {
            need += self.gap_before(p, last_after) + p.body_height() + p.decor_height();
            need += p.space_after;
            last_after = p.space_after;
        }
        if let Some(n) = next {
            if !n.page_break_before {
                let top = n.decor.as_ref().map_or(0.0, ParaDecor::top);
                need += self.gap_before(n, last_after) + top + n.first_line_height();
            }
        }
        if self.used + need > self.frame.capacity + FIT_TOLERANCE {
            self.new_page();
        }
    }

    pub fn place_para(&mut self, para: &ParaBox) {
        let at_top = match self.page_break_before {
            PageBreakBefore::FirstPageTopOnly => self.at_page_top() && self.pages.len() == 1,
            PageBreakBefore::AnyPageTop => self.at_page_top(),
        };
        if para.page_break_before && !at_top {
            self.new_page();
        }
        // 段中不分页：整段放不下、又不在页首，就整段挪到下一页。
        if para.keep_lines
            && !self.at_page_top()
            && self.used
                + self.gap_before(para, self.last_after)
                + para.body_height()
                + para.decor_height()
                > self.frame.capacity + FIT_TOLERANCE
        {
            self.new_page();
        }
        // 上一段留着的框：同一组就接着用（段距算在框里），否则先收口。
        let continuing = matches!((&self.open, &para.decor), (Some(o), Some(d)) if o.decor == *d);
        if !continuing {
            self.close_box();
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
                if let Some(d) = &para.decor {
                    let lead = self.lead(d);
                    self.start_lines(d, lead);
                }
                self.used += height;
            }
            ParaBody::Lines(lines) => {
                // 框的下边框也要放得下：跨页时前一页照样收口。
                let reserve = para.decor.as_ref().map_or(0.0, ParaDecor::bottom);
                let mut next = 0;
                while next < lines.len() {
                    let lead = para.decor.as_ref().map_or(0.0, |d| self.lead(d));
                    let mut n = fit_lines(
                        &lines[next..],
                        self.used + lead,
                        self.frame.capacity - reserve,
                        self.at_page_top(),
                    );
                    if para.widow_control {
                        n = widow_orphan(lines.len(), next, n, &lines[..], self.at_page_top());
                    }
                    if n == 0 {
                        self.new_page();
                        continue;
                    }
                    if let Some(d) = &para.decor {
                        self.start_lines(d, lead);
                    }
                    for line in &lines[next..next + n] {
                        self.commit(line);
                    }
                    next += n;
                    if lines[next - 1].page_break_after {
                        self.new_page();
                        // 段落以分页符结束：段后距不带到新页上。
                        if next == lines.len() {
                            return;
                        }
                    }
                }
            }
        }
        if !para.joins_next {
            self.close_box();
        }
        self.used += para.space_after;
        self.last_after = para.space_after;
    }

    /// 放一张表格。行不拆开，纵向合并连在一起的几行放在同一页；放不下又不在页首
    /// 就换页：旧页按最后一行的下边收口，新页上下一行按它自己的上边开头。
    pub fn place_table(&mut self, t: &TableBox) {
        self.close_box();
        let mut from = 0;
        // 本页上已经放了这张表格的行。
        let mut placed = false;
        while from < t.row_count() {
            let to = t.group_end(from);
            let need = t.height(from, to, !placed) + t.closing(to);
            if self.used + need > self.frame.capacity + FIT_TOLERANCE && !self.at_page_top() {
                if placed {
                    self.close_table(t, from - 1);
                }
                self.new_page();
                placed = false;
                continue;
            }
            let y = self.y(self.used);
            let page = self.pages.last_mut().expect("至少有一页");
            self.used += t.draw(from, to, !placed, y, &mut page.ops);
            placed = true;
            from = to + 1;
        }
        if placed {
            self.close_table(t, t.row_count() - 1);
        }
        self.last_after = 0.0;
    }

    fn close_table(&mut self, t: &TableBox, ri: usize) {
        let y = self.y(self.used);
        let page = self.pages.last_mut().expect("至少有一页");
        self.used += t.close(ri, y, &mut page.ops);
    }

    /// 本页接下来要放 `d` 框里的行，第一行之前还要占多高：框还没开就是上边框，
    /// 接着上一段的框就是分隔线（有的话）。
    fn lead(&self, d: &ParaDecor) -> f32 {
        if self.open.is_some() {
            d.between()
        } else {
            d.top()
        }
    }

    /// 放第一行之前：开框，或者在同一个框里画上一段与本段之间的分隔线。
    fn start_lines(&mut self, d: &ParaDecor, lead: f32) {
        if self.open.is_some() {
            if let Some(b) = d.borders.between {
                let y = self.y(self.used + b.space);
                let page = self.pages.last_mut().expect("至少有一页");
                edge(&mut page.ops, &b, Side::Top, (d.left, d.right), y);
            }
        } else {
            let ops_at = self.pages.last().map_or(0, |p| p.ops.len());
            self.open = Some(OpenBox {
                decor: d.clone(),
                top: self.used,
                ops_at,
            });
        }
        self.used += lead;
    }

    /// 收口：加上下边框占的高度，画出底纹与四边。
    fn close_box(&mut self) {
        let Some(b) = self.open.take() else {
            return;
        };
        let d = &b.decor;
        self.used += d.bottom();
        let (top, bottom) = (self.y(b.top), self.y(self.used));
        let page = self.pages.last_mut().expect("至少有一页");
        if let Some(color) = d.fill {
            let fill = PaintOp::Rect {
                x: d.left,
                y: bottom,
                w: d.right - d.left,
                h: top - bottom,
                color,
            };
            page.ops.insert(b.ops_at, fill);
        }
        let ops = &mut page.ops;
        let (across, up) = ((d.left, d.right), (bottom, top));
        if let Some(e) = &d.borders.top {
            edge(ops, e, Side::Top, across, top);
        }
        if let Some(e) = &d.borders.bottom {
            edge(ops, e, Side::Bottom, across, bottom);
        }
        if let Some(e) = &d.borders.left {
            edge(ops, e, Side::Left, up, d.left);
        }
        if let Some(e) = &d.borders.right {
            edge(ops, e, Side::Right, up, d.right);
        }
    }

    /// 从正文区顶部往下 `used` 处的 y（PDF 坐标）。
    fn y(&self, used: f32) -> f32 {
        self.frame.page.h_pt - self.frame.page.margin_top - self.frame.origin - used
    }

    fn commit(&mut self, line: &Line) {
        self.last_after = 0.0;
        let base = self.y(self.used) - line.baseline;
        let page = self.pages.last_mut().expect("至少有一页");
        page.ops.extend(line.ops.iter().map(|op| op.shifted(base)));
        self.used += line.height;
    }
}

/// 孤行控制：段落从第 `next` 行起、当前页放得下 `n` 行时，实际该放几行。
///
/// - 最后一行不单独落到下一页：只剩一行放不下时，再往下一页多挪一行；
/// - 第一行不单独留在页底：段落的头一行放得下、第二行放不下时，整段挪走。
///
/// 分页符造成的拆分不算。在页首时至少放一行，否则永远排不出去。
fn widow_orphan(total: usize, next: usize, n: usize, lines: &[Line], at_top: bool) -> usize {
    let split_by_overflow = next + n < total && n > 0 && !lines[next + n - 1].page_break_after;
    if !split_by_overflow {
        return n;
    }
    let mut m = n;
    if total - (next + m) == 1 && m >= 2 {
        m -= 1;
    }
    if next == 0 && m == 1 && total >= 2 {
        m = 0;
    }
    if at_top {
        m.max(1)
    } else {
        m
    }
}

/// 放不放得下的判断允许的误差。行网格下行高都是格高的整数倍，22 行 31.2pt
/// 在 f32 里累加出来会比网格区的 686.4pt 多一点点，不留余量就会少排一行。
const FIT_TOLERANCE: f32 = 1e-3;

/// 框的哪一边。
#[derive(Clone, Copy)]
pub(super) enum Side {
    Top,
    Bottom,
    Left,
    Right,
}

/// 画一条边框线。`outer` 是它的外沿（上边框的上沿、左边框的左沿），线往框里长；
/// `span` 是它沿线方向的范围。
///
/// 实线也按描边画，不画成细长的矩形：阅读器给描边保底一个像素宽，0.5pt 的表格线
/// 缩小看时不会变淡、时有时无。平头线端，盖住的范围与矩形相同。
pub(super) fn edge(
    ops: &mut Vec<PaintOp>,
    b: &ir::Border,
    side: Side,
    (lo, hi): (f32, f32),
    outer: f32,
) {
    let inward = match side {
        Side::Top | Side::Right => -1.0,
        Side::Bottom | Side::Left => 1.0,
    };
    let horizontal = matches!(side, Side::Top | Side::Bottom);
    let w = b.width;
    // 离外沿 `from` 处起、一条线宽的线。
    let line = |from: f32, dash: Vec<f32>| {
        let mid = outer + inward * (from + w / 2.0);
        let (from, to) = if horizontal {
            ((lo, mid), (hi, mid))
        } else {
            ((mid, lo), (mid, hi))
        };
        PaintOp::Line {
            from,
            to,
            width: w,
            color: b.color,
            dash,
        }
    };
    let unit = w.max(0.5);
    match b.style {
        BorderStyle::Double => {
            ops.push(line(0.0, Vec::new()));
            ops.push(line(2.0 * w, Vec::new()));
        }
        BorderStyle::Dotted => ops.push(line(0.0, vec![unit, unit])),
        BorderStyle::Dashed => ops.push(line(0.0, vec![unit * 6.0, unit * 3.0])),
        _ => ops.push(line(0.0, Vec::new())),
    }
}

/// 从已用高度 `used` 开始，当前页还放得下前几行。
///
/// 页首（`at_top`，本页什么都还没放）的那一行无论多高都放得下 —— 否则一行比整页
/// 还高时永远排不出去。
fn fit_lines(lines: &[Line], mut used: f32, content_height: f32, at_top: bool) -> usize {
    let mut n = 0;
    for line in lines {
        if used + line.fit_height > content_height + FIT_TOLERANCE && !(at_top && n == 0) {
            break;
        }
        used += line.height;
        n += 1;
        // 分页符之后的行放到下一页。
        if line.page_break_after {
            break;
        }
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
                page_break_after: false,
                ops: Vec::new(),
            })
            .collect()
    }

    #[test]
    fn fits_until_the_next_line_would_cross_the_bottom() {
        let l = lines(&[20.0, 20.0, 20.0]);
        assert_eq!(fit_lines(&l, 0.0, 60.0, true), 3);
        assert_eq!(fit_lines(&l, 0.0, 59.9, true), 2);
        assert_eq!(fit_lines(&l, 45.0, 60.0, false), 0);
    }

    /// 网格区正好放满 22 行：浮点累加误差不能让最后一行被挤到下一页。
    /// 行高与网格区都按排版时的同一算法求（A4、上下边距 72pt、格高 15.6pt）。
    #[test]
    fn lines_that_exactly_fill_the_grid_area_all_fit() {
        let grid = crate::docx::ir::Grid { pitch_pt: 15.6 };
        let area = (697.9f32 / grid.pitch_pt).floor() * grid.pitch_pt;
        let l = lines(&[grid.snap(17.388); 23]);
        assert_eq!(fit_lines(&l, 0.0, area, true), 22);
    }

    /// 行距倍数在文字下方多出来的空白可以越过页底。
    #[test]
    fn only_the_text_part_of_the_last_line_has_to_fit() {
        let line = |height, fit_height| Line {
            height,
            baseline: 20.0,
            fit_height,
            page_break_after: false,
            ops: Vec::new(),
        };
        let l = [line(40.56, 31.2), line(40.56, 31.2)];
        assert_eq!(fit_lines(&l, 0.0, 72.0, true), 2);
        assert_eq!(fit_lines(&l, 0.0, 71.0, true), 1);
    }

    #[test]
    fn widows_and_orphans_are_avoided() {
        let l = lines(&[10.0; 6]);
        // 6 行里放得下 5 行：最后一行不单独落下去，改放 4 行。
        assert_eq!(widow_orphan(6, 0, 5, &l, false), 4);
        // 只放得下第一行：整段挪走。
        assert_eq!(widow_orphan(6, 0, 1, &l, false), 0);
        // 在页首时至少放一行。
        assert_eq!(widow_orphan(6, 0, 1, &l, true), 1);
        // 整段放得下：不动。
        assert_eq!(widow_orphan(6, 0, 6, &l, false), 6);
        // 两行的段落只放得下一行：两条规则一起，整段挪走。
        assert_eq!(widow_orphan(2, 0, 1, &l, false), 0);
    }

    #[test]
    fn a_line_taller_than_the_page_still_goes_on_an_empty_page() {
        let l = lines(&[500.0, 20.0]);
        assert_eq!(fit_lines(&l, 0.0, 100.0, true), 1);
        assert_eq!(fit_lines(&l, 10.0, 100.0, false), 0);
    }
}
