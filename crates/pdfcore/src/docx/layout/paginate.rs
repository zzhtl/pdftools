//! 分页：把测量好的块按顺序放进页面。

use std::collections::HashMap;

use super::calib::{Calib, PageBreakBefore};
use super::para::{Line, ParaBody, ParaBox, ParaDecor};
use super::table::{Frag, TableBox};
use super::{Measured, Page, PageKind, PaintOp};
use crate::docx::ir::{self, BorderStyle, PageGeom, SectionStart};

/// 排得下任何内容的高度：不分页地叠放时用。
pub(super) const ENDLESS: f32 = f32::MAX / 4.0;

/// 单元格、页眉页脚这类「故事」排到一页上的结果。
pub(super) struct StoryPage {
    /// y 以这一页的顶端为 0（往下为负）。
    pub ops: Vec<PaintOp>,
    pub height: f32,
    /// 放了几行。
    pub lines: usize,
}

/// 把一串块按给定的各页高度排下去（超出的页沿用最后一个高度）。`soft_top`：第一页的
/// 顶端不算页首，一行都放不下时不硬放，整个挪到第二页。
pub(super) fn flow(
    blocks: &[Measured],
    caps: &[f32],
    soft_top: bool,
    collapse: bool,
) -> Vec<StoryPage> {
    let mut pages = Paginator::story(caps, soft_top, collapse);
    for m in blocks {
        pages.place(m);
    }
    pages.finish_story()
}

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
    /// 排「故事」时第 i 页的高度（超出的页沿用最后一个）。正文是空的，按各页的版面。
    schedule: Vec<f32>,
    /// 第一页的顶端不算页首，见 [`flow`]。
    soft_top: bool,
    /// 已经排完的各页用掉的高度。
    heights: Vec<f32>,
    /// 各页放了几行。
    lines: Vec<usize>,
    /// 当前页上上下型环绕的图挡住的横条（与 `used` 同一个量法），碰到的行挪到下面。
    bands: Vec<(f32, f32)>,
}

/// 一段的浮动对象放在了哪一页、放之前各层有多少东西：段落挪到下一页时撤回。
struct FloatMark {
    page: usize,
    under: usize,
    over: usize,
    bands: usize,
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
            schedule: Vec::new(),
            soft_top: false,
            heights: Vec::new(),
            lines: vec![0],
            bands: Vec::new(),
        }
    }

    /// 排「故事」用：没有纸张，第 i 页高 `caps[i]`，y 以各页顶端为 0。
    fn story(caps: &[f32], soft_top: bool, collapse_spacing: bool) -> Self {
        let page = PageGeom {
            w_pt: 0.0,
            h_pt: 0.0,
            margin_top: 0.0,
            margin_bottom: 0.0,
            margin_left: 0.0,
            margin_right: 0.0,
            header_dist: 0.0,
            footer_dist: 0.0,
        };
        let frame = Frame {
            page,
            origin: 0.0,
            capacity: caps.first().copied().unwrap_or(ENDLESS),
        };
        Self {
            frame,
            frames: Frames::uniform(frame),
            section: 0,
            section_start: false,
            restart: None,
            pages: vec![Page::new(&page, 1, 0, PageKind::Default)],
            used: 0.0,
            collapse_spacing,
            last_after: 0.0,
            // 故事里的分页符在量的时候已经去掉了。
            page_break_before: PageBreakBefore::AnyPageTop,
            open: None,
            schedule: caps.to_vec(),
            soft_top,
            heights: Vec::new(),
            lines: vec![0],
            bands: Vec::new(),
        }
    }

    fn finish_story(mut self) -> Vec<StoryPage> {
        self.close_box();
        self.heights.push(self.used);
        self.pages
            .into_iter()
            .zip(self.heights)
            .zip(self.lines)
            .map(|((p, height), lines)| StoryPage {
                ops: p.under.into_iter().chain(p.ops).chain(p.over).collect(),
                height,
                lines,
            })
            .collect()
    }

    /// 放一个量好的块。
    pub fn place(&mut self, m: &Measured) {
        match m {
            Measured::Para(p) => self.place_para(p),
            Measured::Placeholder(paras) => paras.iter().for_each(|p| self.place_para(p)),
            Measured::Table(t) => self.place_table(t),
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
        let (mut frame, kind) = self.frames.pick(self.section_start, number);
        if let Some(&cap) = self.schedule.get(self.pages.len()).or(self.schedule.last()) {
            frame.capacity = cap;
        }
        self.section_start = false;
        self.frame = frame;
        self.pages
            .push(Page::new(&frame.page, number, self.section, kind));
        self.heights.push(self.used);
        self.lines.push(0);
        self.bands.clear();
        self.used = 0.0;
        self.last_after = 0.0;
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
        self.used <= f32::EPSILON && !(self.soft_top && self.pages.len() == 1)
    }

    /// 段前距实际要加多少：与上一段的段后距取较大值时，只补差额。
    fn gap_before(&self, para: &ParaBox, last_after: f32) -> f32 {
        if self.collapse_spacing {
            (para.space_before - last_after).max(0.0)
        } else {
            para.space_before
        }
    }

    /// 与下段同页：`chain` 里的各段整段、再加 `next` 的第一行（表格是第一行放得下的
    /// 第一段），当前页放不下、又不在页首时，先换页。在页首就照排 —— 比一页还长的串
    /// 只能被断开。
    pub fn keep_together(&mut self, chain: &[&ParaBox], next: Option<&Measured>) {
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
        match next {
            Some(Measured::Para(n)) if !n.page_break_before => {
                let top = n.decor.as_ref().map_or(0.0, ParaDecor::top);
                need += self.gap_before(n, last_after) + top + n.first_line_height();
            }
            Some(Measured::Table(t)) => need += t.first_fit(),
            _ => {}
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
                self.count_line();
            }
            ParaBody::Lines(lines) => {
                // 框的下边框也要放得下：跨页时前一页照样收口。
                let reserve = para.decor.as_ref().map_or(0.0, ParaDecor::bottom);
                let mut next = 0;
                // 浮动对象跟着段落的第一行定在哪一页；第一行挪走时撤回重放。
                let mut floats: Option<FloatMark> = None;
                while next < lines.len() {
                    if next == 0 && floats.is_none() && !para.floats.is_empty() {
                        floats = Some(self.place_floats(para));
                    }
                    self.skip_bands(lines[next].height);
                    let lead = para.decor.as_ref().map_or(0.0, |d| self.lead(d));
                    let mut n = fit_lines(
                        &lines[next..],
                        self.used + lead,
                        self.frame.capacity - reserve,
                        self.at_page_top(),
                    );
                    // 一次放得下的几行里有碰到图的：放到它前面为止，下一轮再绕过去。
                    let clear = self.clear_of_bands(&lines[next..next + n], self.used + lead);
                    if clear < n {
                        n = clear;
                    } else if para.widow_control {
                        n = widow_orphan(lines.len(), next, n, &lines[..], self.at_page_top());
                    }
                    if n == 0 {
                        if next == 0 {
                            if let Some(mark) = floats.take() {
                                self.undo_floats(mark);
                            }
                        }
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

    /// 放一段的浮动对象：按页面（或者所在的栏、段落）定位，画进正文下面或上面的一层；
    /// 上下型的记下它挡住的横条。单元格、页眉页脚这类故事没有纸张，都相对栏与段落。
    fn place_floats(&mut self, para: &ParaBox) -> FloatMark {
        use ir::{At, RelativeFrom as F};
        let index = self.page_index();
        let page = self.pages.last().expect("至少有一页");
        let mark = FloatMark {
            page: index,
            under: page.under.len(),
            over: page.over.len(),
            bands: self.bands.len(),
        };
        let story = !self.schedule.is_empty();
        let geom = self.frame.page;
        // 离纸张上边多远（故事里是离故事顶端多远）。
        let from_top = |used: f32| geom.h_pt - self.y(used);
        let body_top = from_top(0.0);
        let para_top = from_top(self.used);
        let place = |(start, len): (f32, f32), size: f32, at: At| match at {
            At::Offset(o) => start + o,
            At::Start => start,
            At::Center => start + (len - size) / 2.0,
            At::End => start + len - size,
        };
        let mut placed = Vec::new();
        for f in &para.floats {
            let h_area = match f.h.from {
                _ if story => para.column,
                F::Page => (0.0, geom.w_pt),
                F::Margin => (geom.margin_left, geom.content_width()),
                F::LeftMargin => (0.0, geom.margin_left),
                F::RightMargin => (geom.w_pt - geom.margin_right, geom.margin_right),
                _ => para.column,
            };
            let v_area = match f.v.from {
                _ if story => (para_top, 0.0),
                F::Page => (0.0, geom.h_pt),
                F::Margin => (geom.margin_top, geom.content_height()),
                F::TopMargin => (0.0, geom.margin_top),
                F::BottomMargin => (geom.h_pt - geom.margin_bottom, geom.margin_bottom),
                _ => (para_top, 0.0),
            };
            let x = place(h_area, f.width, f.h.at);
            let top = place(v_area, f.height, f.v.at);
            let rect = [x, geom.h_pt - top - f.height, f.width, f.height];
            if f.wrap == ir::Wrap::TopAndBottom {
                self.bands.push((
                    top - f.dist[0] - body_top,
                    top + f.height + f.dist[1] - body_top,
                ));
            }
            placed.push((f.behind, object_ops(&f.content, rect)));
        }
        let page = self.pages.last_mut().expect("至少有一页");
        for (behind, ops) in placed {
            if behind {
                page.under.extend(ops);
            } else {
                page.over.extend(ops);
            }
        }
        mark
    }

    fn undo_floats(&mut self, mark: FloatMark) {
        if let Some(page) = self.pages.get_mut(mark.page) {
            page.under.truncate(mark.under);
            page.over.truncate(mark.over);
        }
        if mark.page == self.page_index() {
            self.bands.truncate(mark.bands);
        }
    }

    /// 高 `h` 的下一样东西碰到图挡住的横条，就挪到横条下面。
    fn skip_bands(&mut self, h: f32) {
        while let Some(&(_, bottom)) = self
            .bands
            .iter()
            .find(|(top, bottom)| *top < self.used + h - FIT_TOLERANCE && *bottom > self.used)
        {
            self.used = bottom;
        }
    }

    /// 从 `from` 起往下接着放这几行，碰到横条之前放得下几行。
    fn clear_of_bands(&self, lines: &[Line], mut from: f32) -> usize {
        for (i, line) in lines.iter().enumerate() {
            if self
                .bands
                .iter()
                .any(|(top, bottom)| *top < from + line.height - FIT_TOLERANCE && *bottom > from)
            {
                return i;
            }
            from += line.height;
        }
        lines.len()
    }

    /// 放一张表格：一行一行地放，放不下的行在页底拆开（写了 `w:cantSplit`、固定行高的
    /// 整行挪到下一页），续页先重复标题行。一页上的这一截凑齐了才画：纵向合并的格要
    /// 知道自己在这一页上占多高。
    pub fn place_table(&mut self, t: &TableBox) {
        self.close_box();
        self.skip_bands(t.first_fit());
        let mut part = TablePart::default();
        // 纵向合并的格跨页时，前几页各给了它多高。
        let mut merged = HashMap::new();
        // 本页这一截从页首开始、还没放正文行：这一行无论如何都要放下（拆开或者硬放），
        // 不然永远排不出去。
        let mut fresh = self.at_page_top();
        let mut ri = 0;
        // 正在拆的行：前几段各能用多高；第一段是不是在页中间开始的。
        let mut caps: Vec<f32> = Vec::new();
        let mut soft = false;
        while ri < t.row_count() {
            let must = fresh && !part.body;
            let at_top = !part.body;
            let avail =
                self.frame.capacity - self.used - part.height - t.band(ri, at_top) - t.closing(ri);
            if caps.is_empty() {
                let whole = t.content(ri) <= avail + FIT_TOLERANCE
                    || (must && !t.splittable(ri) && t.fixed(ri));
                if whole {
                    part.push(t, Frag::whole(ri, at_top, t.content(ri)));
                    ri += 1;
                    continue;
                }
                // 不能拆的行挪到下一页；已经在页首还放不下，只好拆开。
                if !t.splittable(ri) && !must {
                    self.break_table(t, &mut part, &mut merged, ri);
                    fresh = true;
                    continue;
                }
                soft = !must;
            }
            caps.push(avail.max(0.0));
            let piece = t.piece(ri, &caps, soft);
            if caps.len() == 1 && !piece.placed && !must {
                // 一行都放不下：整行挪到下一页。
                caps.clear();
                self.break_table(t, &mut part, &mut merged, ri);
                fresh = true;
                continue;
            }
            let more = piece.more;
            part.push(t, Frag::piece(ri, at_top, piece));
            if more {
                self.break_table(t, &mut part, &mut merged, ri);
                fresh = true;
                continue;
            }
            caps.clear();
            ri += 1;
        }
        self.flush_table(t, &mut part, &mut merged);
        self.last_after = 0.0;
    }

    /// 在第 `ri` 行之前换页：画完本页的这一截，新的一页先重复标题行。
    fn break_table(
        &mut self,
        t: &TableBox,
        part: &mut TablePart,
        merged: &mut HashMap<(usize, usize), Vec<f32>>,
        ri: usize,
    ) {
        self.flush_table(t, part, merged);
        self.new_page();
        let heads = t.header_rows();
        // 标题行比一页还高时不重复，否则每页只剩标题行。
        if ri >= heads && t.header_height() < self.frame.capacity {
            for hi in 0..heads {
                part.push(t, Frag::whole(hi, hi == 0, t.content(hi)));
            }
            part.body = false;
        }
    }

    /// 画出本页的这一截表格，在最后一行下面收口。
    fn flush_table(
        &mut self,
        t: &TableBox,
        part: &mut TablePart,
        merged: &mut HashMap<(usize, usize), Vec<f32>>,
    ) {
        let Some(last) = part.frags.last().map(|f| f.ri) else {
            return;
        };
        let y = self.y(self.used);
        let page = self.pages.last_mut().expect("至少有一页");
        self.used += t.draw(&part.frags, y, merged, &mut page.ops);
        let y = self.y(self.used);
        let page = self.pages.last_mut().expect("至少有一页");
        self.used += t.close(last, y, &mut page.ops);
        *part = TablePart::default();
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
        self.count_line();
    }

    fn count_line(&mut self) {
        if let Some(n) = self.lines.last_mut() {
            *n += 1;
        }
    }
}

/// 一页上已经凑好、还没画的一截表格。
#[derive(Default)]
struct TablePart {
    frags: Vec<Frag>,
    /// 各行连同上框线的高度之和。
    height: f32,
    /// 放了正文行（不只是重复的标题行）。
    body: bool,
}

impl TablePart {
    fn push(&mut self, t: &TableBox, f: Frag) {
        self.height += t.band(f.ri, f.at_top) + f.height;
        self.body = true;
        self.frags.push(f);
    }
}

/// 一个浮动对象画出来的操作：`rect` 是左下角与宽高（PDF 坐标）。
fn object_ops(content: &ir::ObjectContent, [x, y, w, h]: [f32; 4]) -> Vec<PaintOp> {
    match content {
        ir::ObjectContent::Image { part, crop } => vec![PaintOp::Image {
            part: part.clone(),
            x,
            y,
            w,
            h,
            crop: *crop,
        }],
        ir::ObjectContent::Missing { .. } => super::para::missing_box(x, y, w, h),
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
