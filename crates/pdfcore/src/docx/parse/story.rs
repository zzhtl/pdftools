//! 块级与行内内容：段落、run、表格。正文、页眉页脚、单元格、文本框共用这一套。
//!
//! 不认识的容器元素一律「钻进去」而不是跳过：`w:customXml`、`w:smartTag`、
//! 各种新版包装元素里装的仍然是正文，跳过就会悄悄丢字。只有明确不该显示的
//! （删除的修订、域代码、`mc:Choice` 分支）才整棵跳过。

use quick_xml::events::{BytesStart, Event};

use super::props::{parse_ppr, parse_rpr, parse_sect_pr};
use super::table::{parse_grid, parse_tbl_pr, parse_tc_pr, parse_tr_pr};
use super::{attr, resolve_entity, skip, xml_err, Rd};
use crate::docx::model::{
    Block, BreakKind, Cell, Drawing, FieldChar, LinkRef, Para, Picture, Row, Run, RunItem, SectPr,
    Story, Table,
};
use crate::error::Result;

/// 读 `w:body`。返回正文，以及 body 末尾那个 `w:sectPr`（最后一节的页面设置）。
pub(super) fn parse_body(r: &mut Rd) -> Result<(Story, Option<SectPr>)> {
    let mut out = Vec::new();
    let mut last = None;
    parse_blocks(r, "body", &mut out, &mut last)?;
    Ok((out, last))
}

/// 读一个页眉或页脚部件（根元素 `w:hdr` / `w:ftr`）。
pub(super) fn parse_part(r: &mut Rd) -> Result<Story> {
    let mut out = Vec::new();
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) if matches!(e.local_name().as_ref(), "hdr" | "ftr") => {
                let root = e.local_name().as_ref().to_string();
                let mut no_section = None;
                parse_blocks(r, &root, &mut out, &mut no_section)?;
                break;
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

/// 读块级内容直到 `end` 结束。`section` 收 body 级的 `w:sectPr`。
fn parse_blocks(
    r: &mut Rd,
    end: &str,
    out: &mut Story,
    section: &mut Option<SectPr>,
) -> Result<()> {
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) => match e.local_name().as_ref() {
                "p" => out.push(Block::Para(parse_paragraph(r)?)),
                "tbl" => out.push(Block::Table(parse_table(r, 1)?)),
                "sectPr" => *section = Some(parse_sect_pr(r)?),
                name if skips_subtree(name) => skip(r, name)?,
                _ => {}
            },
            // Word 把没有任何属性的空段落写成 `<w:p/>`。它照样占一行。
            Event::Empty(e) if e.local_name().as_ref() == "p" => {
                out.push(Block::Para(Para::default()))
            }
            Event::End(e) if e.local_name().as_ref() == end => break,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(())
}

/// 整棵跳过、内容不该显示的元素。
fn skips_subtree(name: &str) -> bool {
    matches!(
        name,
        // 删除的修订、移走的原文。
        "del" | "moveFrom"
            // `mc:Choice` 是新版特性的表示；我们还不认识任何一种，一律取 `mc:Fallback`。
            | "Choice"
            // 内容控件的属性（占位格式、下拉项……），不是正文。
            | "sdtPr" | "sdtEndPr"
            // 表格、行、单元格的属性由各自的解析函数先读走，出现在别处就不是正文。
            | "tblPr" | "tblGrid" | "tblPrEx" | "trPr" | "tcPr"
    )
}

fn parse_paragraph(r: &mut Rd) -> Result<Para> {
    let mut para = Para::default();
    // 段落里再套段落不合规范，但文本框之类的结构偶尔会这样；
    // 内层的字并入本段，至少不丢。
    let mut depth = 1usize;
    // 正在 `w:hyperlink` 里面时，指向哪里。
    let mut link: Option<LinkRef> = None;
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) => match e.local_name().as_ref() {
                "p" => depth += 1,
                "pPr" => {
                    let (ppr, section) = parse_ppr(r)?;
                    para.ppr = ppr;
                    para.section = section;
                }
                "hyperlink" => {
                    link = attr(&e, "id")
                        .map(LinkRef::Rel)
                        .or_else(|| attr(&e, "anchor").map(LinkRef::Anchor));
                }
                // 公式里的 `m:r` 也按普通 run 读：公式按线性文字输出，至少内容还在。
                "r" => {
                    let mut run = parse_run(r)?;
                    run.link = link.clone();
                    para.runs.push(run);
                }
                // 简单域：展开成与复杂域一样的开始、代码、分隔……结束，里面的 run 是结果。
                "fldSimple" => {
                    let code = attr(&e, "instr").unwrap_or_default();
                    para.runs.push(field_run(&[
                        RunItem::FieldChar(FieldChar::Begin),
                        RunItem::FieldCode(code),
                        RunItem::FieldChar(FieldChar::Separate),
                    ]));
                }
                name if skips_subtree(name) => skip(r, name)?,
                // 超链接、`w:ins`、`w:smartTag`、公式、内容控件……里面都是正常显示的 run。
                _ => {}
            },
            Event::Empty(e) if e.local_name().as_ref() == "fldSimple" => {
                let code = attr(&e, "instr").unwrap_or_default();
                para.runs.push(field_run(&[
                    RunItem::FieldChar(FieldChar::Begin),
                    RunItem::FieldCode(code),
                    RunItem::FieldChar(FieldChar::Separate),
                    RunItem::FieldChar(FieldChar::End),
                ]));
            }
            Event::End(e) if e.local_name().as_ref() == "fldSimple" => {
                para.runs
                    .push(field_run(&[RunItem::FieldChar(FieldChar::End)]));
            }
            Event::End(e) if e.local_name().as_ref() == "hyperlink" => link = None,
            Event::End(e) if e.local_name().as_ref() == "p" => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(para)
}

/// 只有域标记、没有文字的 run。
fn field_run(items: &[RunItem]) -> Run {
    Run {
        items: items.to_vec(),
        ..Run::default()
    }
}

fn field_char(e: &BytesStart) -> Option<FieldChar> {
    match attr(e, "fldCharType").as_deref() {
        Some("begin") => Some(FieldChar::Begin),
        Some("separate") => Some(FieldChar::Separate),
        Some("end") => Some(FieldChar::End),
        _ => None,
    }
}

fn parse_run(r: &mut Rd) -> Result<Run> {
    let mut run = Run::default();
    // 注音（`w:ruby`）的底文里还套着 run。
    let mut depth = 1usize;
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) => match e.local_name().as_ref() {
                "r" => depth += 1,
                "rPr" => run.rpr = parse_rpr(r)?,
                "t" => {
                    let text = read_text(r, &e)?;
                    if !text.is_empty() {
                        run.items.push(RunItem::Text(text));
                    }
                }
                "drawing" => run.items.push(RunItem::Drawing(parse_drawing(r)?)),
                // VML 与嵌入对象：本版本只取替代文字。
                name @ ("pict" | "object") => {
                    let name = name.to_string();
                    run.items.push(RunItem::Drawing(Drawing {
                        alt: find_alt_text(r, &name)?,
                        ..Default::default()
                    }));
                }
                // 域代码（`PAGE`、`TOC \o "1-3"`）是给 Word 看的指令，不是正文，
                // 记下来给排版认页码域用。
                "instrText" => {
                    let code = read_text(r, &e)?;
                    run.items.push(RunItem::FieldCode(code));
                }
                // 窗体域的 `w:fldChar` 里还有 `w:ffData`。
                "fldChar" => {
                    run.items.extend(field_char(&e).map(RunItem::FieldChar));
                    skip(r, "fldChar")?;
                }
                // 删除的域代码、删除的文字、注音的读音标注都不进正文。
                name @ ("delInstrText" | "delText" | "rt") => skip(r, name)?,
                name if skips_subtree(name) => skip(r, name)?,
                _ => {}
            },
            Event::Empty(e) => match e.local_name().as_ref() {
                "tab" => run.items.push(RunItem::Tab),
                "br" => run
                    .items
                    .push(RunItem::Break(match attr(&e, "type").as_deref() {
                        Some("page") => BreakKind::Page,
                        Some("column") => BreakKind::Column,
                        _ => BreakKind::Line,
                    })),
                "cr" => run.items.push(RunItem::Break(BreakKind::Line)),
                "noBreakHyphen" => run.items.push(RunItem::NoBreakHyphen),
                "fldChar" => run.items.extend(field_char(&e).map(RunItem::FieldChar)),
                "sym" => {
                    if let Some(code) =
                        attr(&e, "char").and_then(|c| u32::from_str_radix(&c, 16).ok())
                    {
                        run.items.push(RunItem::Sym {
                            font: attr(&e, "font"),
                            code,
                        });
                    }
                }
                _ => {}
            },
            Event::End(e) if e.local_name().as_ref() == "r" => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(run)
}

/// 读一个 `w:t` 的文字（刚读过它的 Start 事件）。
///
/// 没有 `xml:space="preserve"` 时，OOXML 规定忽略首尾空白 —— 格式化过的 XML
/// 元素之间有缩进换行，不遵守就会凭空多出一堆空格。去空白要对整个元素做一次：
/// 实体引用会把文字切成好几段，逐段去会把「A &amp; B」读成「A&B」。
fn read_text(r: &mut Rd, start: &BytesStart) -> Result<String> {
    let preserve = attr(start, "space").as_deref() == Some("preserve");
    let end = start.local_name().as_ref().to_string();
    let mut text = String::new();
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Text(t) => text.push_str(&t),
            Event::GeneralRef(rf) => {
                if let Some(c) = resolve_entity(&rf) {
                    text.push(c);
                }
            }
            Event::CData(c) => text.push_str(&c),
            Event::End(e) if e.local_name().as_ref() == end => break,
            Event::Eof => break,
            _ => {}
        }
    }
    if preserve {
        Ok(text)
    } else {
        Ok(text.trim().to_string())
    }
}

/// 跳过一棵图片子树，顺便把 `wp:docPr/@descr`（替代文字）捞出来。
/// 有替代文字的话，占位提示就能说清楚「这里原本是什么图」。
/// `w:drawing`：行内还是浮动、显示大小、替代文字，是图片的话取图片与裁剪。
/// 组合、形状、图表都不当图片。
fn parse_drawing(r: &mut Rd) -> Result<Drawing> {
    let mut d = Drawing::default();
    let mut depth = 1usize;
    // 组合、图表里也可能有 `pic:pic`，但那不是一张单独的图。
    let mut composite = false;
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) if e.local_name().as_ref() == "drawing" => depth += 1,
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                "inline" => d.inline = true,
                "anchor" => d.inline = false,
                "extent" if d.extent.is_none() => {
                    let n = |k| attr(&e, k).and_then(|v| v.trim().parse::<i64>().ok());
                    d.extent = n("cx").zip(n("cy"));
                }
                "docPr" => {
                    d.alt = d
                        .alt
                        .take()
                        .or_else(|| attr(&e, "descr").filter(|s| !s.trim().is_empty()));
                }
                "wgp" | "grpSp" | "chart" | "relIds" | "wsp" => composite = true,
                "pic" if !composite && d.picture.is_none() => d.picture = Some(Picture::default()),
                "blip" => {
                    if let Some(p) = d.picture.as_mut().filter(|p| p.target.is_none()) {
                        p.target = attr(&e, "embed");
                    }
                }
                "srcRect" => {
                    if let Some(p) = d.picture.as_mut() {
                        let n = |k| attr(&e, k).and_then(|v| v.trim().parse().ok()).unwrap_or(0);
                        p.crop = [n("l"), n("t"), n("r"), n("b")];
                    }
                }
                _ => {}
            },
            Event::End(e) if e.local_name().as_ref() == "drawing" => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if composite {
        d.picture = None;
    }
    Ok(d)
}

fn find_alt_text(r: &mut Rd, name: &str) -> Result<Option<String>> {
    let mut depth = 1usize;
    let mut alt = None;
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "docPr" => {
                alt = alt.or_else(|| attr(&e, "descr").filter(|s| !s.trim().is_empty()));
            }
            Event::Start(e) if e.local_name().as_ref() == name => depth += 1,
            Event::End(e) if e.local_name().as_ref() == name => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(alt)
}

/// 表格最多嵌套几层。再往里的只留下文字：正常的文书不会嵌这么深，一层层递归下去
/// 却可能把栈用完。
const MAX_TABLE_DEPTH: usize = 16;

/// 读一张表格。`depth`：它是第几层（正文里的是 1）。
fn parse_table(r: &mut Rd, depth: usize) -> Result<Table> {
    let mut table = Table::default();
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) => match e.local_name().as_ref() {
                "tblPr" => table.props = parse_tbl_pr(r, "tblPr")?,
                "tblGrid" => table.grid = parse_grid(r)?,
                "tr" => table.rows.push(parse_row(r, depth)?),
                name if skips_subtree(name) => skip(r, name)?,
                // 包在内容控件、customXml 里的行。
                _ => {}
            },
            Event::End(e) if e.local_name().as_ref() == "tbl" => break,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(table)
}

fn parse_row(r: &mut Rd, depth: usize) -> Result<Row> {
    let mut row = Row::default();
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) => match e.local_name().as_ref() {
                "trPr" => row.props = parse_tr_pr(r)?,
                "tblPrEx" => row.exceptions = parse_tbl_pr(r, "tblPrEx")?,
                "tc" => row.cells.push(parse_cell(r, depth)?),
                name if skips_subtree(name) => skip(r, name)?,
                _ => {}
            },
            Event::End(e) if e.local_name().as_ref() == "tr" => break,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(row)
}

/// 一个单元格：属性，以及与正文同样的块级内容。
fn parse_cell(r: &mut Rd, depth: usize) -> Result<Cell> {
    let mut cell = Cell::default();
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) => match e.local_name().as_ref() {
                "tcPr" => cell.props = parse_tc_pr(r)?,
                "p" => cell.content.push(Block::Para(parse_paragraph(r)?)),
                "tbl" if depth >= MAX_TABLE_DEPTH => cell.content.push(Block::Para(table_text(r)?)),
                "tbl" => cell.content.push(Block::Table(parse_table(r, depth + 1)?)),
                name if skips_subtree(name) => skip(r, name)?,
                // 内容控件、customXml 里的段落与表格。
                _ => {}
            },
            Event::Empty(e) if e.local_name().as_ref() == "p" => {
                cell.content.push(Block::Para(Para::default()))
            }
            Event::End(e) if e.local_name().as_ref() == "tc" => break,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(cell)
}

/// 嵌得太深的表格：不再解析结构，把里面的字收成一段，段与段之间隔一个空格。
fn table_text(r: &mut Rd) -> Result<Para> {
    let mut depth = 1usize;
    let mut text = String::new();
    let mut in_text = false;
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) => match e.local_name().as_ref() {
                "tbl" => depth += 1,
                "t" => in_text = true,
                _ => {}
            },
            Event::Text(t) if in_text => text.push_str(&t),
            Event::GeneralRef(rf) if in_text => {
                if let Some(c) = resolve_entity(&rf) {
                    text.push(c);
                }
            }
            Event::End(e) => match e.local_name().as_ref() {
                "t" => in_text = false,
                "p" if !text.is_empty() && !text.ends_with(' ') => text.push(' '),
                "tbl" => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    let run = Run {
        items: vec![RunItem::Text(text.trim_end().to_string())],
        ..Default::default()
    };
    Ok(Para {
        runs: vec![run],
        ..Default::default()
    })
}
