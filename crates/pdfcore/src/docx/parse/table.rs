//! 表格的属性：`w:tblPr`（与 `w:tblPrEx`）、`w:tblGrid`、`w:trPr`、`w:tcPr`。
//!
//! 边框与边距的子元素同名（`w:top`、`w:left`……），只能按所在的容器区分，
//! 所以两种容器各读各的。

use quick_xml::events::{BytesStart, Event};

use super::props::{parse_border, parse_shd};
use super::{attr, attr_i32, on_off, skip, xml_err, Rd};
use crate::docx::model::{
    Align, CellMargins, HeightRule, TableBorders, TblPr, TcPr, TrPr, VAlign, VMerge, Width,
};
use crate::error::Result;

/// `w:tblW`、`w:tcW`。百分比在过渡格式里是五十分之一个百分点（5000 = 100%），
/// 严格格式里直接写「100%」。
fn parse_width(e: &BytesStart) -> Option<Width> {
    let w = attr(e, "w").unwrap_or_default();
    match attr(e, "type").as_deref() {
        Some("auto") | Some("nil") => Some(Width::Auto),
        Some("pct") => {
            let pct = match w.strip_suffix('%') {
                Some(p) => p.trim().parse::<f32>().ok()?,
                None => w.trim().parse::<f32>().ok()? / 50.0,
            };
            Some(Width::Percent(pct))
        }
        // dxa，或者没写 type。
        _ => w.trim().parse::<i32>().ok().map(Width::Twips),
    }
}

fn parse_borders(r: &mut Rd, end: &str) -> Result<TableBorders> {
    let mut b = TableBorders::default();
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) | Event::Empty(e) => {
                let border = parse_border(&e);
                match e.local_name().as_ref() {
                    "top" => b.top = Some(border),
                    "left" | "start" => b.left = Some(border),
                    "bottom" => b.bottom = Some(border),
                    "right" | "end" => b.right = Some(border),
                    "insideH" => b.inside_h = Some(border),
                    "insideV" => b.inside_v = Some(border),
                    // 斜线（tl2br、tr2bl）本版本不画。
                    _ => {}
                }
            }
            Event::End(e) if e.local_name().as_ref() == end => break,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(b)
}

fn parse_margins(r: &mut Rd, end: &str) -> Result<CellMargins> {
    let mut m = CellMargins::default();
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) | Event::Empty(e) => {
                let w = match parse_width(&e) {
                    Some(Width::Twips(t)) => Some(t),
                    _ => None,
                };
                match e.local_name().as_ref() {
                    "top" => m.top = w,
                    "left" | "start" => m.left = w,
                    "bottom" => m.bottom = w,
                    "right" | "end" => m.right = w,
                    _ => {}
                }
            }
            Event::End(e) if e.local_name().as_ref() == end => break,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(m)
}

/// 读 `w:tblPr` 或 `w:tblPrEx`，到 `end` 的结束标签为止。
pub(super) fn parse_tbl_pr(r: &mut Rd, end: &str) -> Result<TblPr> {
    let mut p = TblPr::default();
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) => match e.local_name().as_ref() {
                "tblBorders" => p.borders = parse_borders(r, "tblBorders")?,
                "tblCellMar" => p.cell_margins = parse_margins(r, "tblCellMar")?,
                // 修订前的旧属性、浮动表格的定位都不用。
                name @ ("tblPrChange" | "tblpPr") => skip(r, name)?,
                _ => {}
            },
            Event::Empty(e) => match e.local_name().as_ref() {
                "tblStyle" => p.style_id = attr(&e, "val"),
                "tblW" => p.width = parse_width(&e),
                "jc" => {
                    p.align = match attr(&e, "val").as_deref() {
                        Some("center") => Some(Align::Center),
                        Some("right" | "end") => Some(Align::Right),
                        Some("left" | "start") => Some(Align::Left),
                        _ => None,
                    }
                }
                "tblInd" => {
                    if let Some(Width::Twips(t)) = parse_width(&e) {
                        p.indent = Some(t);
                    }
                }
                "tblLayout" => p.fixed_layout = attr(&e, "type").as_deref() == Some("fixed"),
                "shd" => p.shading = Some(parse_shd(&e)),
                _ => {}
            },
            Event::End(e) if e.local_name().as_ref() == end => break,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(p)
}

/// `w:tblGrid` 里各列的宽度。
pub(super) fn parse_grid(r: &mut Rd) -> Result<Vec<i32>> {
    let mut cols = Vec::new();
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "gridCol" => {
                cols.push(attr_i32(&e, "w").unwrap_or(0).max(0));
            }
            Event::Start(e) if e.local_name().as_ref() == "tblGridChange" => {
                skip(r, "tblGridChange")?
            }
            Event::End(e) if e.local_name().as_ref() == "tblGrid" => break,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(cols)
}

pub(super) fn parse_tr_pr(r: &mut Rd) -> Result<TrPr> {
    let mut p = TrPr::default();
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) if e.local_name().as_ref() == "trPrChange" => skip(r, "trPrChange")?,
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                "trHeight" => {
                    let rule = match attr(&e, "hRule").as_deref() {
                        Some("exact") => HeightRule::Exact,
                        Some("auto") => HeightRule::Auto,
                        // 缺省是 atLeast。
                        _ => HeightRule::AtLeast,
                    };
                    p.height = attr_i32(&e, "val").map(|v| (v.max(0), rule));
                }
                "cantSplit" => p.cant_split = on_off(&e),
                "tblHeader" => p.header = on_off(&e),
                "gridBefore" => p.grid_before = attr_i32(&e, "val").unwrap_or(0).max(0) as u32,
                "gridAfter" => p.grid_after = attr_i32(&e, "val").unwrap_or(0).max(0) as u32,
                _ => {}
            },
            Event::End(e) if e.local_name().as_ref() == "trPr" => break,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(p)
}

pub(super) fn parse_tc_pr(r: &mut Rd) -> Result<TcPr> {
    let mut p = TcPr::default();
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) => match e.local_name().as_ref() {
                "tcBorders" => p.borders = parse_borders(r, "tcBorders")?,
                "tcMar" => p.margins = parse_margins(r, "tcMar")?,
                "tcPrChange" => skip(r, "tcPrChange")?,
                _ => {}
            },
            Event::Empty(e) => match e.local_name().as_ref() {
                "tcW" => p.width = parse_width(&e),
                "gridSpan" => p.grid_span = attr_i32(&e, "val").map(|v| v.max(1) as u32),
                "vMerge" => {
                    p.v_merge = Some(match attr(&e, "val").as_deref() {
                        Some("restart") => VMerge::Restart,
                        _ => VMerge::Continue,
                    })
                }
                "shd" => p.shading = Some(parse_shd(&e)),
                "vAlign" => {
                    p.v_align = match attr(&e, "val").as_deref() {
                        Some("center") => Some(VAlign::Center),
                        Some("bottom") => Some(VAlign::Bottom),
                        Some("top") => Some(VAlign::Top),
                        _ => None,
                    }
                }
                _ => {}
            },
            Event::End(e) if e.local_name().as_ref() == "tcPr" => break,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(p)
}
