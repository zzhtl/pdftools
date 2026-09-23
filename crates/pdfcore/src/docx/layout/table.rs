//! 表格：先量出各行各格，再一行一行放进页面，放不下的行跨页拆开。
//! 规则见 [`Tables::Drawn`](super::calib::Tables)。
//!
//! 竖直方向依次是上框线、第一行、行间框线……下框线，框线各占自己的线宽；
//! 文字排在两条框线之间，再让出上下边距。横向上，竖框线骑在列边界上，文字从
//! 列边界让出左右边距。

use std::collections::HashMap;

use super::paginate::{edge, flow, Side, StoryPage, ENDLESS};
use super::para::{Env, ParaBox, ParaDecor};
use super::{Measured, PaintOp};
use crate::docx::ir::{self, Border, Edge};
use crate::docx::model::{HeightRule, VAlign};

/// 量好的表格。x 是绝对坐标；y 在放进页面时才定。
pub(super) struct TableBox {
    /// 列边界（竖框线的中线）的 x，比列数多一个。
    col_x: Vec<f32>,
    rows: Vec<RowBox>,
    /// 开头几行是标题行（`w:tblHeader`），跨页时在新的一页上重复。
    header_rows: usize,
    /// 叠放单元格内容时段距取较大值。
    collapse: bool,
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
    /// 固定行高（`w:hRule="exact"`）。
    fixed: bool,
    /// 能跨页拆开：没写 `w:cantSplit`，也不是固定行高。
    splittable: bool,
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
    /// 量好的内容。跨页拆开时按各页能给的高度重新排。
    blocks: Vec<Measured>,
    /// 整格不拆时的内容（y 以上边距之下为 0）与高度。
    ops: Vec<PaintOp>,
    height: f32,
}

impl CellBox {
    /// 内容连同上下边距要多高。
    fn needs(&self) -> f32 {
        self.padding() + self.height
    }

    /// 上下边距之和。
    fn padding(&self) -> f32 {
        self.margins[0] + self.margins[2]
    }

    /// 跨在几行上的格（纵向合并）。
    fn spans_rows(&self) -> bool {
        self.continued || self.last_row > self.first_row
    }
}

/// 一页上的一截表格里的一行，或者拆开的一行在这一页上的一段。
pub(super) struct Frag {
    pub ri: usize,
    /// 这一截的第一行（或重复的标题行之后的第一行）：上框线用本行自己的上边。
    pub at_top: bool,
    /// 上下两条框线之间的高度。
    pub height: f32,
    /// 拆开的行这一段各格的内容，按格在行里的顺序（纵向合并的格另算，是 None）。
    /// 整行放下时是 None。
    pieces: Option<Vec<Option<StoryPage>>>,
}

impl Frag {
    pub fn whole(ri: usize, at_top: bool, height: f32) -> Self {
        Self {
            ri,
            at_top,
            height,
            pieces: None,
        }
    }

    pub fn piece(ri: usize, at_top: bool, p: Piece) -> Self {
        Self {
            ri,
            at_top,
            height: p.height,
            pieces: Some(p.cells),
        }
    }
}

/// 拆开的一行在一页上的一段。
pub(super) struct Piece {
    cells: Vec<Option<StoryPage>>,
    /// 上下两条框线之间的高度。
    height: f32,
    /// 这一段里至少放下了一行字。
    pub placed: bool,
    /// 还有内容要放到下一页。
    pub more: bool,
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

/// 一串块在一页上起头至少要多高：第一个块的第一行（表格是第一行放得下的第一段）。
fn first_fit(blocks: &[Measured]) -> f32 {
    let para = |p: &ParaBox| {
        p.space_before + p.decor.as_ref().map_or(0.0, ParaDecor::top) + p.first_line_height()
    };
    match blocks.first() {
        Some(Measured::Para(p)) => para(p),
        Some(Measured::Placeholder(paras)) => paras.first().map_or(0.0, para),
        Some(Measured::Table(t)) => t.first_fit(),
        None => 0.0,
    }
}

/// 把一格里的块在给定的栏里量好。
pub(super) type MeasureCell<'a> = dyn FnMut(&[ir::Block], &Env) -> Vec<Measured> + 'a;

/// 量一张表格。`collapse`：段距取较大值。
pub(super) fn measure(
    t: &ir::Table,
    env: &Env,
    collapse: bool,
    measure_cell: &mut MeasureCell,
) -> TableBox {
    // 不知道宽度的列平分版心剩下的宽度，至少留 18pt（LibreOffice 没写宽度时也是平分）。
    let unknown = t.columns.iter().filter(|w| **w <= 0.0).count();
    let known: f32 = t.columns.iter().filter(|w| **w > 0.0).sum();
    let share = if unknown > 0 {
        ((env.width - known) / unknown as f32).max(18.0)
    } else {
        0.0
    };
    let columns: Vec<f32> = t
        .columns
        .iter()
        .map(|&w| if w > 0.0 { w } else { share })
        .collect();
    let width: f32 = columns.iter().sum();
    let x0 = env.left
        + match t.align {
            ir::Align::Center => (env.width - width) / 2.0,
            ir::Align::Right => env.width - width,
            _ => t.indent,
        };
    let col_x: Vec<f32> = std::iter::once(x0)
        .chain(columns.iter().scan(x0, |x, w| {
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
        .map(|(ri, row)| {
            let fixed = matches!(row.height, Some((_, HeightRule::Exact)));
            RowBox {
                band: 0.0,
                band_at_top: 0.0,
                closing: 0.0,
                content: 0.0,
                fixed,
                splittable: !row.cant_split && !fixed,
                cells: row
                    .cells
                    .iter()
                    .map(|c| {
                        if !c.continued {
                            merge_from[c.col] = ri;
                        }
                        let blocks = if c.continued {
                            Vec::new()
                        } else {
                            let left = col_x[c.col] + c.margins[1];
                            let right = col_x[c.col + c.span] - c.margins[3];
                            let cell_env = Env {
                                left,
                                width: (right - left).max(1.0),
                                punct_hangs: false,
                                ..*env
                            };
                            measure_cell(&c.blocks, &cell_env)
                        };
                        let whole = flow(&blocks, &[ENDLESS], false, collapse).swap_remove(0);
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
                            blocks,
                            ops: whole.ops,
                            height: whole.height,
                        }
                    })
                    .collect(),
            }
        })
        .collect();
    let heads = t.rows.iter().take_while(|r| r.header).count();
    let mut table = TableBox {
        col_x,
        rows,
        // 全是标题行时没有什么可重复的。
        header_rows: if heads == t.rows.len() { 0 } else { heads },
        collapse,
    };

    // 框线要看相邻的行，先都算出来再写回去。
    let bands: Vec<(f32, f32, f32)> = (0..table.rows.len())
        .map(|ri| {
            let tb = &table;
            let widest = |at_top: bool| {
                (0..tb.col_x.len() - 1)
                    .map(|col| thickness(tb.top_line(ri, col, at_top)))
                    .fold(0.0, f32::max)
            };
            let (band, band_at_top) = (widest(false), widest(true));
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
            .filter(|c| !c.spans_rows())
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
    table
}

impl TableBox {
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// 开头几行是跨页时要重复的标题行。
    pub fn header_rows(&self) -> usize {
        self.header_rows
    }

    /// 标题行连同它们的框线有多高。
    pub fn header_height(&self) -> f32 {
        (0..self.header_rows)
            .map(|ri| self.band(ri, ri == 0) + self.rows[ri].content)
            .sum()
    }

    /// 第 `ri` 行上面那条框线的粗细。`at_top`：它是本页上这张表格的第一行。
    pub fn band(&self, ri: usize, at_top: bool) -> f32 {
        let row = &self.rows[ri];
        if at_top {
            row.band_at_top
        } else {
            row.band
        }
    }

    /// 第 `ri` 行整行不拆时，上下两条框线之间有多高。
    pub fn content(&self, ri: usize) -> f32 {
        self.rows[ri].content
    }

    /// 在第 `ri` 行之后收口时，下框线有多粗。
    pub fn closing(&self, ri: usize) -> f32 {
        self.rows[ri].closing
    }

    pub fn splittable(&self, ri: usize) -> bool {
        self.rows[ri].splittable
    }

    /// 固定行高：拆不开，内容多出来也不撑高。
    pub fn fixed(&self, ri: usize) -> bool {
        self.rows[ri].fixed
    }

    /// 表格在一页上起头至少要多高：第一行上框线、第一行放得下的第一段（拆不开的行
    /// 是整行）、收口的下框线。
    pub fn first_fit(&self) -> f32 {
        let Some(row) = self.rows.first() else {
            return 0.0;
        };
        let body = if row.splittable {
            row.cells
                .iter()
                .filter(|c| !c.spans_rows())
                .map(|c| c.padding() + first_fit(&c.blocks))
                .fold(0.0, f32::max)
        } else {
            row.content
        };
        row.band_at_top + body + row.closing
    }

    /// 拆开的第 `ri` 行在这一页上的一段。`caps`：前几段各能用多高，最后一个是这一段
    /// 能用的；`soft`：第一段在页中间开始，一行都放不下的格不硬放。
    pub fn piece(&self, ri: usize, caps: &[f32], soft: bool) -> Piece {
        let k = caps.len() - 1;
        let mut piece = Piece {
            cells: Vec::new(),
            height: 0.0,
            placed: false,
            more: false,
        };
        for c in &self.rows[ri].cells {
            if c.spans_rows() {
                piece.cells.push(None);
                continue;
            }
            let cell_caps: Vec<f32> = caps
                .iter()
                .map(|cap| (cap - c.padding()).max(0.0))
                .chain(std::iter::once(ENDLESS))
                .collect();
            let mut pages = flow(&c.blocks, &cell_caps, soft, self.collapse);
            piece.more |= pages.iter().skip(k + 1).any(|p| p.lines > 0);
            let page = if k < pages.len() {
                pages.swap_remove(k)
            } else {
                StoryPage {
                    ops: Vec::new(),
                    height: 0.0,
                    lines: 0,
                }
            };
            piece.placed |= page.lines > 0;
            piece.height = piece.height.max(c.padding() + page.height);
            piece.cells.push(Some(page));
        }
        piece
    }

    /// 第 `ri` 行里盖住第 `col` 列的格（续格换成开头的那一格）。
    fn owner(&self, ri: usize, col: usize) -> Option<&CellBox> {
        let c = self.rows[ri]
            .cells
            .iter()
            .find(|c| c.col <= col && col < c.col + c.span)?;
        if c.continued {
            self.origin(c)
        } else {
            Some(c)
        }
    }

    /// 续格所在的纵向合并开头的那一格。
    fn origin(&self, c: &CellBox) -> Option<&CellBox> {
        self.rows[c.first_row]
            .cells
            .iter()
            .find(|o| o.col == c.col && !o.continued)
    }

    /// 第 `ri` 行里盖住第 `col` 列的格（续格照原样返回）。
    fn cell_at(&self, ri: usize, col: usize) -> Option<&CellBox> {
        self.rows[ri]
            .cells
            .iter()
            .find(|c| c.col <= col && col < c.col + c.span)
    }

    /// 第 `ri` 行上面、第 `col` 列这一段画什么线。`at_top`：这一行在本页上开头，只看
    /// 本行（续格用开头那一格）的上边。否则与上一行的下边争；纵向合并的格里面没有线；
    /// 本行这一列空着（`w:gridBefore`、`w:gridAfter`）时画上一行的下边。
    fn top_line(&self, ri: usize, col: usize, at_top: bool) -> Option<Border> {
        if at_top || ri == 0 {
            return self.owner(ri, col)?.borders.top.border;
        }
        let above = self.owner(ri - 1, col);
        match self.cell_at(ri, col) {
            Some(c) if c.continued => None,
            Some(c) => resolve(
                above.map_or(Edge::default(), |a| a.borders.bottom),
                c.borders.top,
            ),
            None => above?.borders.bottom.border,
        }
    }

    /// 画一页上的这一截：`y` 是第一行上框线的外沿（PDF 坐标）。`merged` 记着跨页的
    /// 纵向合并格前几页各给了多高。返回占掉的高度（不含收口的下框线）。
    pub fn draw(
        &self,
        frags: &[Frag],
        y: f32,
        merged: &mut HashMap<(usize, usize), Vec<f32>>,
        ops: &mut Vec<PaintOp>,
    ) -> f32 {
        // 各段上框线外沿的 y，以及上框线的粗细。
        let mut tops = Vec::with_capacity(frags.len());
        let mut cur = y;
        for f in frags {
            let band = self.band(f.ri, f.at_top);
            tops.push((cur, band));
            cur -= band + f.height;
        }
        let bottom = |i: usize| tops[i].0 - tops[i].1 - frags[i].height;
        // 第 `i` 段下面那条横线的粗细：下一段的上框线，最后一段是收口的下框线。竖线
        // 一直画到它的下沿，把角补上。
        let below = |i: usize| match tops.get(i + 1) {
            Some(&(_, band)) => band,
            None => self.rows[frags[i].ri].closing,
        };

        // 底纹在最下，其上是文字，框线最后画：合并的格跨行时，后画的底纹不会盖住先画的框线。
        let (mut fills, mut text, mut lines) = (Vec::new(), Vec::new(), Vec::new());
        for (i, f) in frags.iter().enumerate() {
            let (top, band) = tops[i];
            for (k, c) in self.rows[f.ri].cells.iter().enumerate() {
                // 续格由开头那一格画；只有在这一截的第一行（接着上一页）时，由它替开头那一格画。
                if c.continued && !f.at_top {
                    continue;
                }
                let Some(owner) = (if c.continued { self.origin(c) } else { Some(c) }) else {
                    continue;
                };
                // 这一格在这一截里一直占到第几段。
                let j = (i..frags.len())
                    .take_while(|&j| frags[j].ri <= owner.last_row)
                    .last()
                    .unwrap_or(i);
                let low = bottom(j);
                let (x1, x2) = (self.col_x[owner.col], self.col_x[owner.col + owner.span]);
                let last = self.owner(f.ri, owner.col + owner.span).is_none();
                // 左框线与左边那一格的右边争；行里最右的格再画自己的右框线。
                let left = match owner.col.checked_sub(1).and_then(|l| self.owner(f.ri, l)) {
                    Some(l) => resolve(l.borders.right, owner.borders.left),
                    None => owner.borders.left.border,
                };
                let right = owner.borders.right.border.filter(|_| last);

                // 底纹从左框线的中线铺到右框线的中线。
                if let Some(color) = owner.shading {
                    fills.push(PaintOp::Rect {
                        x: x1,
                        y: low,
                        w: x2 - x1,
                        h: top - low,
                        color,
                    });
                }

                let area_top = top - band - owner.margins[0];
                let room = area_top - owner.margins[2] - low;
                let content = if owner.spans_rows() {
                    // 纵向合并的格：整组都在这一截里、又是头一回画时按竖直对齐摆整格内容；
                    // 否则按它在这一页上占的高度接着往下排。
                    let key = (owner.first_row, owner.col);
                    let ends = frags[j].ri == owner.last_row;
                    let whole = ends
                        && !merged.contains_key(&key)
                        && !c.continued
                        && frags[i..=j].iter().all(|f| f.pieces.is_none());
                    if whole {
                        Content::Whole
                    } else {
                        let history = merged.entry(key).or_default();
                        // 这一组在这一截里结束：剩下的都放在这里。
                        history.push(if ends { ENDLESS } else { room.max(0.0) });
                        let caps: Vec<f32> = history
                            .iter()
                            .copied()
                            .chain(std::iter::once(ENDLESS))
                            .collect();
                        let n = history.len() - 1;
                        let pages = flow(&owner.blocks, &caps, true, self.collapse);
                        Content::Owned(pages.into_iter().nth(n).map(|p| p.ops).unwrap_or_default())
                    }
                } else {
                    match f.pieces.as_ref().and_then(|p| p[k].as_ref()) {
                        Some(page) => Content::Piece(&page.ops),
                        None => Content::Whole,
                    }
                };
                match content {
                    Content::Whole => {
                        let slack = (room - owner.height).max(0.0);
                        let dy = match owner.v_align {
                            VAlign::Top => 0.0,
                            VAlign::Center => slack / 2.0,
                            VAlign::Bottom => slack,
                        };
                        text.extend(owner.ops.iter().map(|op| op.shifted(area_top - dy)));
                    }
                    // 拆开的行不做竖直对齐，从上往下排。
                    Content::Piece(piece) => {
                        text.extend(piece.iter().map(|op| op.shifted(area_top)))
                    }
                    Content::Owned(piece) => {
                        text.extend(piece.iter().map(|op| op.shifted(area_top)))
                    }
                }

                for (b, x) in [(left, x1), (right, x2)] {
                    if let Some(b) = b {
                        let span = (low - below(j), top);
                        lines.push((b, Side::Left, span, x - b.thickness() / 2.0));
                    }
                }
            }
            for col in 0..self.col_x.len() - 1 {
                if let Some(b) = self.top_line(f.ri, col, f.at_top) {
                    let span = (self.col_x[col], self.col_x[col + 1]);
                    lines.push((b, Side::Top, span, top));
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
            if let Some(b) = self.owner(ri, col).and_then(|c| c.borders.bottom.border) {
                let span = (self.col_x[col], self.col_x[col + 1]);
                edge(ops, &b, Side::Top, span, y);
            }
        }
        self.rows[ri].closing
    }
}

/// 一格在一页上画什么。
enum Content<'a> {
    /// 量好的整格内容，按竖直对齐摆。
    Whole,
    /// 拆开的行在这一页上的一段。
    Piece(&'a [PaintOp]),
    /// 跨页的纵向合并格在这一页上的一段。
    Owned(Vec<PaintOp>),
}
