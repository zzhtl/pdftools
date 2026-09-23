//! 表格：先量出各行各格，再按行放进页面。规则见 [`Tables::Drawn`](super::calib::Tables)。
//!
//! 竖直方向依次是上框线、第一行、行间框线……下框线，框线各占自己的线宽；
//! 文字排在两条框线之间，再让出上下边距。横向上，竖框线骑在列边界上，文字从
//! 列边界让出左右边距。

use super::paginate::{edge, Side};
use super::para::Env;
use super::PaintOp;
use crate::docx::ir::{self, Border, Edge};
use crate::docx::model::{HeightRule, VAlign};

/// 量好的表格。x 是绝对坐标；y 在放进页面时才定。
pub(super) struct TableBox {
    /// 列边界（竖框线的中线）的 x，比列数多一个。
    col_x: Vec<f32>,
    rows: Vec<RowBox>,
}

struct RowBox {
    /// 与上一行在同一页时，两行之间那条框线的粗细：每一列上两边的线争出一条，再取最粗的。
    band: f32,
    /// 本行在一页的开头时上框线的粗细：只看本行自己的上边。
    band_at_top: f32,
    /// 在本行之后收口（表格结束，或者换页）时下框线的粗细。
    closing: f32,
    /// 上下两条框线之间的高度。
    content: f32,
    /// 纵向合并连在一起、要放在同一页的这一组行，最后一行是第几行。
    group_end: usize,
    cells: Vec<CellBox>,
}

struct CellBox {
    col: usize,
    span: usize,
    /// 纵向合并从第几行开始、到第几行（含）。不合并时都是本行。
    first_row: usize,
    last_row: usize,
    /// 接在上一格下面的续格：不单独画。
    continued: bool,
    borders: ir::CellBorders,
    shading: Option<[u8; 3]>,
    /// 上、左、下、右边距。
    margins: [f32; 4],
    v_align: VAlign,
    /// 内容的绘制操作，y 以上边距之下为 0。
    ops: Vec<PaintOp>,
    height: f32,
}

impl CellBox {
    /// 内容连同上下边距要多高。
    fn needs(&self) -> f32 {
        self.margins[0] + self.height + self.margins[2]
    }
}

fn thickness(b: Option<Border>) -> f32 {
    b.map_or(0.0, |b| b.thickness())
}

/// 相邻两格共用的一条边上画哪条线：单元格自己写的胜过从表格继承的；再比粗细，
/// 一样粗时颜色深的胜出（亮度按 R + B + 2G 比）。
fn resolve(a: Edge, b: Edge) -> Option<Border> {
    if a.explicit != b.explicit {
        return if a.explicit { a.border } else { b.border };
    }
    match (a.border, b.border) {
        (Some(x), Some(y)) => {
            let light = |c: [u8; 3]| c[0] as u32 + c[2] as u32 + 2 * c[1] as u32;
            let heavier = y.thickness() > x.thickness()
                || (y.thickness() == x.thickness() && light(y.color) < light(x.color));
            Some(if heavier { y } else { x })
        }
        (x, y) => x.or(y),
    }
}

/// 把一格里的块在给定的栏里量好、从上往下叠起来：返回绘制操作（y 以顶端为 0）与总高度。
pub(super) type Stack<'a> = dyn FnMut(&[ir::Block], &Env) -> (Vec<PaintOp>, f32) + 'a;

/// 量一张表格。
pub(super) fn measure(t: &ir::Table, env: &Env, stack: &mut Stack) -> TableBox {
    let width: f32 = t.columns.iter().sum();
    let x0 = env.left
        + match t.align {
            ir::Align::Center => (env.width - width) / 2.0,
            ir::Align::Right => env.width - width,
            _ => t.indent,
        };
    let col_x: Vec<f32> = std::iter::once(x0)
        .chain(t.columns.iter().scan(x0, |x, w| {
            *x += w;
            Some(*x)
        }))
        .collect();

    // 每一列上正在延续的纵向合并从第几行开始。
    let mut merge_from = vec![0; t.columns.len()];
    let rows = t
        .rows
        .iter()
        .enumerate()
        .map(|(ri, row)| RowBox {
            band: 0.0,
            band_at_top: 0.0,
            closing: 0.0,
            content: 0.0,
            group_end: ri,
            cells: row
                .cells
                .iter()
                .map(|c| {
                    let (ops, height) = if c.continued {
                        (Vec::new(), 0.0)
                    } else {
                        let left = col_x[c.col] + c.margins[1];
                        let right = col_x[c.col + c.span] - c.margins[3];
                        let cell_env = Env {
                            left,
                            width: (right - left).max(1.0),
                            punct_hangs: false,
                            ..*env
                        };
                        stack(&c.blocks, &cell_env)
                    };
                    if !c.continued {
                        merge_from[c.col] = ri;
                    }
                    CellBox {
                        col: c.col,
                        span: c.span,
                        first_row: merge_from[c.col],
                        last_row: ri + c.rows - 1,
                        continued: c.continued,
                        borders: c.borders,
                        shading: c.shading,
                        margins: c.margins,
                        v_align: c.v_align,
                        ops,
                        height,
                    }
                })
                .collect(),
        })
        .collect();
    let mut table = TableBox { col_x, rows };

    // 框线要看相邻的行，先都算出来再写回去。
    let bands: Vec<(f32, f32, f32)> = (0..table.rows.len())
        .map(|ri| {
            let tb = &table;
            let own = || tb.rows[ri].cells.iter().filter(|c| !c.continued);
            let band = own()
                .flat_map(|c| (c.col..c.col + c.span).map(move |col| tb.top_edge(ri, c, col)))
                .map(thickness)
                .fold(0.0, f32::max);
            let band_at_top = own()
                .map(|c| thickness(c.borders.top.border))
                .fold(0.0, f32::max);
            let closing = (0..tb.col_x.len() - 1)
                .filter_map(|col| tb.owner(ri, col))
                .map(|c| thickness(c.borders.bottom.border))
                .fold(0.0, f32::max);
            (band, band_at_top, closing)
        })
        .collect();
    let rows = &mut table.rows;
    for (ri, (band, band_at_top, closing)) in bands.into_iter().enumerate() {
        let row = &mut rows[ri];
        let content = row
            .cells
            .iter()
            .filter(|c| !c.continued && c.last_row == ri)
            .map(CellBox::needs)
            .fold(0.0, f32::max);
        row.content = match t.rows[ri].height {
            Some((h, HeightRule::Exact)) => (h - band).max(0.0),
            Some((h, HeightRule::AtLeast)) => content.max(h - band),
            _ => content,
        };
        row.band = band;
        row.band_at_top = band_at_top;
        row.closing = closing;
    }

    // 纵向合并的格比它占的几行加起来还高时，撑高最后一行。
    for ri in 0..rows.len() {
        let merged: Vec<(usize, f32)> = rows[ri]
            .cells
            .iter()
            .filter(|c| !c.continued && c.last_row > ri)
            .map(|c| (c.last_row, c.needs()))
            .collect();
        for (end, needs) in merged {
            let have = rows[ri..=end].iter().map(|r| r.content).sum::<f32>()
                + rows[ri + 1..=end].iter().map(|r| r.band).sum::<f32>();
            if needs > have {
                rows[end].content += needs - have;
            }
        }
    }

    // 连在一起的行：合并的格跨到哪一行，这一组就延续到哪一行。
    let mut from = 0;
    while from < rows.len() {
        let mut end = from;
        let mut ri = from;
        while ri <= end {
            end = rows[ri]
                .cells
                .iter()
                .map(|c| c.last_row)
                .fold(end, usize::max);
            ri += 1;
        }
        for row in &mut rows[from..=end] {
            row.group_end = end;
        }
        from = end + 1;
    }
    table
}

impl TableBox {
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// 从第 `ri` 行起、要放在同一页的这一组行的最后一行。
    pub fn group_end(&self, ri: usize) -> usize {
        self.rows[ri].group_end
    }

    /// 第 `ri` 行上面那条框线的粗细。`at_top`：它是本页上这张表格的第一行。
    fn band(&self, ri: usize, at_top: bool) -> f32 {
        let row = &self.rows[ri];
        if at_top {
            row.band_at_top
        } else {
            row.band
        }
    }

    /// 第 `from..=to` 行连同各自上面的框线有多高（不含收口的下框线）。
    pub fn height(&self, from: usize, to: usize, at_top: bool) -> f32 {
        (from..=to)
            .map(|ri| self.band(ri, at_top && ri == from) + self.rows[ri].content)
            .sum()
    }

    /// 在第 `ri` 行之后收口时，下框线有多粗。
    pub fn closing(&self, ri: usize) -> f32 {
        self.rows[ri].closing
    }

    /// 第 `ri` 行里盖住第 `col` 列的格（续格换成开头的那一格）。
    fn owner(&self, ri: usize, col: usize) -> Option<&CellBox> {
        let c = self.rows[ri]
            .cells
            .iter()
            .find(|c| c.col <= col && col < c.col + c.span)?;
        if !c.continued {
            return Some(c);
        }
        self.rows[c.first_row]
            .cells
            .iter()
            .find(|o| o.col == c.col && !o.continued)
    }

    /// 格 `c`（在第 `ri` 行）在第 `col` 列上方画的线：与上一行那一格的下边争。
    fn top_edge(&self, ri: usize, c: &CellBox, col: usize) -> Option<Border> {
        match ri.checked_sub(1) {
            Some(above) => resolve(
                self.owner(above, col)
                    .map_or(Edge::default(), |a| a.borders.bottom),
                c.borders.top,
            ),
            None => c.borders.top.border,
        }
    }

    /// 画第 `from..=to` 行（同一组，放在同一页）。`y` 是第一行上框线的外沿（PDF 坐标），
    /// `at_top`：第一行是本页上这张表格的第一行。返回占掉的高度（不含收口的下框线）。
    pub fn draw(
        &self,
        from: usize,
        to: usize,
        at_top: bool,
        y: f32,
        ops: &mut Vec<PaintOp>,
    ) -> f32 {
        // 各行上框线外沿的 y，以及上框线的粗细。
        let mut tops = Vec::with_capacity(to + 1 - from);
        let mut cur = y;
        for ri in from..=to {
            let band = self.band(ri, at_top && ri == from);
            tops.push((cur, band));
            cur -= band + self.rows[ri].content;
        }
        let bottom = |ri: usize| {
            let (top, band) = tops[ri - from];
            top - band - self.rows[ri].content
        };

        // 底纹在最下，其上是文字，框线最后画：合并的格跨行时，后画的底纹不会盖住先画的框线。
        let (mut fills, mut text, mut lines) = (Vec::new(), Vec::new(), Vec::new());
        for ri in from..=to {
            let (top, band) = tops[ri - from];
            for c in self.rows[ri].cells.iter().filter(|c| !c.continued) {
                let low = bottom(c.last_row.min(to));
                let (x1, x2) = (self.col_x[c.col], self.col_x[c.col + c.span]);
                // 左框线与左边那一格的右边争；行里最右的格再画自己的右框线。
                let left = match c.col.checked_sub(1).and_then(|l| self.owner(ri, l)) {
                    Some(l) => resolve(l.borders.right, c.borders.left),
                    None => c.borders.left.border,
                };
                let first = c.col == 0 || self.owner(ri, c.col - 1).is_none();
                let last = self.owner(ri, c.col + c.span).is_none();
                let right = c.borders.right.border.filter(|_| last);

                // 底纹从左框线的中线铺到右框线的中线。
                if let Some(color) = c.shading {
                    fills.push(PaintOp::Rect {
                        x: x1,
                        y: low,
                        w: x2 - x1,
                        h: top - low,
                        color,
                    });
                }

                let area_top = top - band - c.margins[0];
                let room = (area_top - c.margins[2] - low - c.height).max(0.0);
                let dy = match c.v_align {
                    VAlign::Top => 0.0,
                    VAlign::Center => room / 2.0,
                    VAlign::Bottom => room,
                };
                text.extend(c.ops.iter().map(|op| op.shifted(area_top - dy)));

                // 行首、行尾的横线伸到竖框线的外沿，补上角。
                let ends = (
                    if first { thickness(left) / 2.0 } else { 0.0 },
                    thickness(right) / 2.0,
                );
                for col in c.col..c.col + c.span {
                    let b = if at_top && ri == from {
                        c.borders.top.border
                    } else {
                        self.top_edge(ri, c, col)
                    };
                    if let Some(b) = b {
                        let span =
                            self.segment(col, (col == c.col, col + 1 == c.col + c.span), ends);
                        lines.push((b, Side::Top, span, top));
                    }
                }
                for (b, x) in [(left, x1), (right, x2)] {
                    if let Some(b) = b {
                        lines.push((b, Side::Left, (low, top), x - b.thickness() / 2.0));
                    }
                }
            }
        }
        ops.append(&mut fills);
        ops.append(&mut text);
        for (b, side, span, at) in lines {
            edge(ops, &b, side, span, at);
        }
        y - cur
    }

    /// 在第 `ri` 行之后收口：画下框线，`y` 是它的上沿。返回它的粗细。
    pub fn close(&self, ri: usize, y: f32, ops: &mut Vec<PaintOp>) -> f32 {
        for col in 0..self.col_x.len() - 1 {
            let Some(c) = self.owner(ri, col) else {
                continue;
            };
            let Some(b) = c.borders.bottom.border else {
                continue;
            };
            // 行首、行尾那一段伸到竖框线的外沿，补上下面的两个角。
            let first = col == 0 || self.owner(ri, col - 1).is_none();
            let last = self.owner(ri, col + 1).is_none();
            let ends = (
                thickness(c.borders.left.border) / 2.0,
                thickness(c.borders.right.border) / 2.0,
            );
            let span = self.segment(col, (first, last), ends);
            edge(ops, &b, Side::Top, span, y);
        }
        self.rows[ri].closing
    }

    /// 第 `col` 列上一段横线的范围。`(first, last)`：它是行首、行尾那一段，两头各伸出
    /// `ends` 那么多。
    fn segment(&self, col: usize, (first, last): (bool, bool), ends: (f32, f32)) -> (f32, f32) {
        let lo = self.col_x[col] - if first { ends.0 } else { 0.0 };
        let hi = self.col_x[col + 1] + if last { ends.1 } else { 0.0 };
        (lo, hi)
    }
}
