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
    Anchor, AnchorPos, Block, BreakKind, Cell, Drawing, FieldChar, Geometry, LinkRef, Para,
    Picture, Row, Run, RunItem, SectPr, Shape, Story, Table, VAlign, WrapKind,
};
use crate::error::Result;

/// 读 `w:body`。返回正文，以及 body 末尾那个 `w:sectPr`（最后一节的页面设置）。
pub(super) fn parse_body(r: &mut Rd) -> Result<(Story, Option<SectPr>)> {
    let mut out = Vec::new();
    let mut last = None;
    parse_blocks(r, "body", &mut out, &mut last, 0)?;
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
                parse_blocks(r, &root, &mut out, &mut no_section, 0)?;
                break;
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

/// 读块级内容直到 `end` 结束。`section` 收 body 级的 `w:sectPr`。`level`：外面套着
/// 几层容器（表格、文本框），正文与页眉页脚是 0。
fn parse_blocks(
    r: &mut Rd,
    end: &str,
    out: &mut Story,
    section: &mut Option<SectPr>,
    level: usize,
) -> Result<()> {
    let mut alts = Alternates::default();
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) if alts.skips(&e) => {
                let name = e.local_name().as_ref().to_string();
                skip(r, &name)?
            }
            Event::End(e) if alts.ends(&e) => {}
            Event::Start(e) => match e.local_name().as_ref() {
                "p" => out.push(Block::Para(parse_paragraph(r, level)?)),
                "tbl" if level >= MAX_DEPTH => out.push(Block::Para(flat_text(r, "tbl")?)),
                "tbl" => out.push(Block::Table(parse_table(r, level + 1)?)),
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
            // 内容控件的属性（占位格式、下拉项……），不是正文。
            | "sdtPr" | "sdtEndPr"
            // 表格、行、单元格的属性由各自的解析函数先读走，出现在别处就不是正文。
            | "tblPr" | "tblGrid" | "tblPrEx" | "trPr" | "tcPr"
    )
}

/// `mc:AlternateContent`：`mc:Choice` 要求的特性我们都认识就取它，否则取 `mc:Fallback`。
/// 目前只认识 `wps`（形状、文本框）；其余的新版特性一律取 Fallback。
#[derive(Default)]
struct Alternates {
    /// 各层 AlternateContent 是不是已经取了一个 Choice。
    taken: Vec<bool>,
}

impl Alternates {
    /// 开始标签：返回 true 表示这一棵整个跳过。
    fn skips(&mut self, e: &BytesStart) -> bool {
        match e.local_name().as_ref() {
            "AlternateContent" => {
                self.taken.push(false);
                false
            }
            "Choice" => {
                let known =
                    attr(e, "Requires").is_some_and(|r| r.split_whitespace().all(|p| p == "wps"));
                match self.taken.last_mut() {
                    Some(taken) if !*taken && known => {
                        *taken = true;
                        false
                    }
                    _ => true,
                }
            }
            "Fallback" => self.taken.last().copied().unwrap_or(false),
            _ => false,
        }
    }

    /// 结束标签：是 AlternateContent 的结束就收起这一层。
    fn ends(&mut self, e: &quick_xml::events::BytesEnd) -> bool {
        let closes = e.local_name().as_ref() == "AlternateContent";
        if closes {
            self.taken.pop();
        }
        closes
    }
}

/// `level`：段落外面套着几层容器，见 [`parse_blocks`]。
fn parse_paragraph(r: &mut Rd, level: usize) -> Result<Para> {
    let mut para = Para::default();
    // 段落里再套段落不合规范，但文本框之类的结构偶尔会这样；
    // 内层的字并入本段，至少不丢。
    let mut depth = 1usize;
    // 正在 `w:hyperlink` 里面时，指向哪里。
    let mut link: Option<LinkRef> = None;
    let mut alts = Alternates::default();
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) if alts.skips(&e) => {
                let name = e.local_name().as_ref().to_string();
                skip(r, &name)?
            }
            Event::End(e) if alts.ends(&e) => {}
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
                    let mut run = parse_run(r, level)?;
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

fn parse_run(r: &mut Rd, level: usize) -> Result<Run> {
    let mut run = Run::default();
    // 注音（`w:ruby`）的底文里还套着 run。
    let mut depth = 1usize;
    let mut alts = Alternates::default();
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) if alts.skips(&e) => {
                let name = e.local_name().as_ref().to_string();
                skip(r, &name)?
            }
            Event::End(e) if alts.ends(&e) => {}
            Event::Start(e) => match e.local_name().as_ref() {
                "r" => depth += 1,
                "rPr" => run.rpr = parse_rpr(r)?,
                "t" => {
                    let text = read_text(r, &e)?;
                    if !text.is_empty() {
                        run.items.push(RunItem::Text(text));
                    }
                }
                "drawing" => run
                    .items
                    .push(RunItem::Drawing(Box::new(parse_drawing(r, level)?))),
                // VML 的图、文本框、形状，嵌入对象的预览图。
                name @ ("pict" | "object") => {
                    let name = name.to_string();
                    run.items
                        .push(RunItem::Drawing(Box::new(parse_vml(r, &name, level)?)));
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
/// `w:drawing`：行内还是浮动、显示大小、替代文字；图片取图片与裁剪，形状（`wps:wsp`）
/// 取填充、轮廓与文本框里的内容；浮动的再取位置与环绕。组合、图表不当图片也不当形状。
fn parse_drawing(r: &mut Rd, level: usize) -> Result<Drawing> {
    let mut d = Drawing::default();
    // 从 `w:drawing` 往里开着的元素，判断颜色属于填充、轮廓还是别的（阴影之类）。
    let mut path: Vec<String> = Vec::new();
    // 组合、图表里也可能有 `pic:pic`，但那不是一张单独的图。
    let mut composite = false;
    // 正在读哪个方向的位置（true 是横向），里面的文字是偏移还是对齐。
    let mut axis: Option<bool> = None;
    let mut value: Option<&'static str> = None;
    // `@simplePos="1"`：位置写在 `wp:simplePos` 里，相对纸张左上角。
    let mut simple = false;
    loop {
        let ev = r.read_event().map_err(xml_err)?;
        let (e, empty) = match &ev {
            Event::Start(e) => (e, false),
            Event::Empty(e) => (e, true),
            Event::Text(t) => {
                if let (Some(h), Some(kind), Some(a)) = (axis, value, d.anchor.as_mut()) {
                    let pos = if h { &mut a.h } else { &mut a.v };
                    let text = t.trim().to_string();
                    match kind {
                        "offset" => pos.offset = text.parse().ok(),
                        _ => pos.align = Some(text),
                    }
                }
                continue;
            }
            Event::End(e) => {
                match e.local_name().as_ref() {
                    "drawing" if path.is_empty() => break,
                    "positionH" | "positionV" => axis = None,
                    "posOffset" | "align" => value = None,
                    _ => {}
                }
                path.pop();
                continue;
            }
            Event::Eof => break,
            _ => continue,
        };
        let name = e.local_name().as_ref().to_string();
        // 文本框里的内容，读完它自己的结束标签。
        if name == "txbxContent" && !empty {
            let story = text_box(r, level)?;
            if let Some(shape) = d.shape.as_mut() {
                shape.text = Some(story);
            }
            continue;
        }
        let parent = path.last().map(String::as_str);
        let grand = path
            .len()
            .checked_sub(2)
            .and_then(|i| path.get(i))
            .map(String::as_str);
        match name.as_str() {
            "inline" => d.inline = true,
            "anchor" => {
                d.inline = false;
                let n = |k| {
                    attr(e, k)
                        .and_then(|v| v.trim().parse::<i64>().ok())
                        .unwrap_or(0)
                };
                simple = attr(e, "simplePos").is_some_and(|v| v == "1" || v == "true");
                d.anchor = Some(Anchor {
                    behind: attr(e, "behindDoc").is_some_and(|v| v == "1" || v == "true"),
                    dist: [n("distT"), n("distB"), n("distL"), n("distR")],
                    ..Default::default()
                });
            }
            "simplePos" if simple => {
                if let Some(a) = d.anchor.as_mut() {
                    let n = |k| attr(e, k).and_then(|v| v.trim().parse::<i64>().ok());
                    a.h = AnchorPos {
                        from: Some("page".into()),
                        offset: n("x"),
                        align: None,
                    };
                    a.v = AnchorPos {
                        from: Some("page".into()),
                        offset: n("y"),
                        align: None,
                    };
                }
            }
            "positionH" | "positionV" if !simple => {
                axis = Some(name == "positionH");
                if let Some(a) = d.anchor.as_mut() {
                    let pos = if name == "positionH" {
                        &mut a.h
                    } else {
                        &mut a.v
                    };
                    pos.from = attr(e, "relativeFrom");
                }
            }
            "posOffset" => value = Some("offset"),
            "align" if axis.is_some() => value = Some("align"),
            "wrapNone" | "wrapTopAndBottom" | "wrapSquare" | "wrapTight" | "wrapThrough" => {
                if let Some(a) = d.anchor.as_mut() {
                    a.wrap = match name.as_str() {
                        "wrapTopAndBottom" => WrapKind::TopAndBottom,
                        "wrapSquare" => WrapKind::Square,
                        "wrapTight" => WrapKind::Tight,
                        "wrapThrough" => WrapKind::Through,
                        _ => WrapKind::None,
                    };
                }
            }
            "extent" if d.extent.is_none() => {
                let n = |k| attr(e, k).and_then(|v| v.trim().parse::<i64>().ok());
                d.extent = n("cx").zip(n("cy"));
            }
            "docPr" => {
                d.alt = d
                    .alt
                    .take()
                    .or_else(|| attr(e, "descr").filter(|s| !s.trim().is_empty()));
            }
            "wgp" | "grpSp" | "chart" | "relIds" => composite = true,
            "wsp" if !composite && d.shape.is_none() => {
                d.shape = Some(Shape {
                    insets: TEXT_INSETS,
                    ..Default::default()
                })
            }
            "pic" if !composite && d.picture.is_none() => d.picture = Some(Picture::default()),
            "blip" => {
                if let Some(p) = d.picture.as_mut().filter(|p| p.target.is_none()) {
                    p.target = attr(e, "embed");
                }
            }
            "srcRect" => {
                if let Some(p) = d.picture.as_mut() {
                    let n = |k| attr(e, k).and_then(|v| v.trim().parse().ok()).unwrap_or(0);
                    p.crop = [n("l"), n("t"), n("r"), n("b")];
                }
            }
            "prstGeom" => {
                if let Some(shape) = d.shape.as_mut() {
                    shape.geometry = match attr(e, "prst").as_deref() {
                        Some("rect") => Geometry::Rect,
                        Some("roundRect") => Geometry::RoundRect,
                        Some("line" | "straightConnector1") => Geometry::Line,
                        _ => Geometry::Other,
                    };
                }
            }
            "custGeom" => {
                if let Some(shape) = d.shape.as_mut() {
                    shape.geometry = Geometry::Other;
                }
            }
            "xfrm" if parent == Some("spPr") => {
                if let Some(shape) = d.shape.as_mut() {
                    let on = |k| attr(e, k).is_some_and(|v| v == "1" || v == "true");
                    shape.flip = [on("flipH"), on("flipV")];
                    shape.rotation = attr(e, "rot")
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);
                }
            }
            "ln" if parent == Some("spPr") => {
                if let Some(shape) = d.shape.as_mut() {
                    let w = attr(e, "w")
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(9525);
                    shape.line = Some((w, shape.line.map_or([0, 0, 0], |l| l.1)));
                }
            }
            "noFill" => {
                if let Some(shape) = d.shape.as_mut() {
                    match parent {
                        Some("spPr") => shape.fill = None,
                        Some("ln") if grand == Some("spPr") => shape.line = None,
                        _ => {}
                    }
                }
            }
            "srgbClr" | "prstClr" | "sysClr" | "schemeClr" if parent == Some("solidFill") => {
                if let (Some(shape), Some(color)) = (d.shape.as_mut(), drawing_color(e)) {
                    match grand {
                        Some("spPr") => shape.fill = Some(color),
                        Some("ln") => {
                            let w = shape.line.map_or(9525, |l| l.0);
                            shape.line = Some((w, color));
                        }
                        _ => {}
                    }
                }
            }
            "bodyPr" => {
                if let Some(shape) = d.shape.as_mut() {
                    for (slot, k) in shape
                        .insets
                        .iter_mut()
                        .zip(["lIns", "tIns", "rIns", "bIns"])
                    {
                        if let Some(v) = attr(e, k).and_then(|v| v.trim().parse().ok()) {
                            *slot = v;
                        }
                    }
                    shape.text_align = match attr(e, "anchor").as_deref() {
                        Some("ctr") => VAlign::Center,
                        Some("b") => VAlign::Bottom,
                        _ => VAlign::Top,
                    };
                }
            }
            _ => {}
        }
        if !empty {
            path.push(name);
        }
    }
    if composite {
        d.picture = None;
        d.shape = None;
    }
    Ok(d)
}

/// 文本框里的字离框的缺省距离：左、上、右、下各 0.1、0.05、0.1、0.05 英寸（EMU），
/// DrawingML 与 VML 都是这个数。
const TEXT_INSETS: [i64; 4] = [91_440, 45_720, 91_440, 45_720];

/// DrawingML 的颜色：`a:srgbClr`、`a:prstClr`、`a:sysClr`；主题色只认白与黑
/// （lt1/bg1、dk1/tx1），其余的配色本版本不读主题，当没有颜色。
fn drawing_color(e: &BytesStart) -> Option<[u8; 3]> {
    let hex = |v: &str| {
        let v = v.trim().trim_start_matches('#');
        (v.len() == 6).then(|| {
            let c = |i| u8::from_str_radix(&v[i..i + 2], 16).ok();
            Some([c(0)?, c(2)?, c(4)?])
        })?
    };
    match e.local_name().as_ref() {
        "srgbClr" => attr(e, "val").and_then(|v| hex(&v)),
        "sysClr" => attr(e, "lastClr").and_then(|v| hex(&v)),
        "prstClr" => named_color(attr(e, "val").as_deref()?),
        "schemeClr" => match attr(e, "val").as_deref() {
            Some("lt1" | "bg1") => Some([255, 255, 255]),
            Some("dk1" | "tx1") => Some([0, 0, 0]),
            _ => None,
        },
        _ => None,
    }
}

/// 常见的颜色名（DrawingML 的 prstClr、VML 的颜色写法）。
fn named_color(name: &str) -> Option<[u8; 3]> {
    Some(match name.trim().to_ascii_lowercase().as_str() {
        "black" => [0, 0, 0],
        "white" => [255, 255, 255],
        "red" => [255, 0, 0],
        "green" => [0, 128, 0],
        "blue" => [0, 0, 255],
        "yellow" => [255, 255, 0],
        "gray" | "grey" => [128, 128, 128],
        _ => return None,
    })
}

/// VML 的颜色：`#RRGGBB`、`#RGB` 或颜色名，后面可能跟着 ` [n]` 之类的索引。
fn vml_color(v: &str) -> Option<[u8; 3]> {
    let v = v.split_whitespace().next()?;
    match v.strip_prefix('#') {
        Some(h) if h.len() == 6 => {
            let c = |i| u8::from_str_radix(&h[i..i + 2], 16).ok();
            Some([c(0)?, c(2)?, c(4)?])
        }
        Some(h) if h.len() == 3 => {
            let c = |i| u8::from_str_radix(&h[i..i + 1], 16).ok().map(|x| x * 17);
            Some([c(0)?, c(1)?, c(2)?])
        }
        _ => named_color(v),
    }
}

/// VML 的长度（`12pt`、`1in`、`2.5cm`、`10mm`、`96px`，不带单位按 px）换成 EMU。
fn vml_length(v: &str) -> Option<i64> {
    let v = v.trim();
    let split = v.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(v.len());
    let (n, unit) = v.split_at(split);
    let n: f64 = n.trim().parse().ok()?;
    let pt = match unit {
        "pt" => n,
        "in" => n * 72.0,
        "cm" => n * 72.0 / 2.54,
        "mm" => n * 72.0 / 25.4,
        "pc" => n * 12.0,
        "px" | "" => n * 0.75,
        "em" => n * 12.0,
        _ => return None,
    };
    Some((pt * 12700.0).round() as i64)
}

/// VML 的 `w:pict` / `w:object`：第一个形状（`v:shape`、`v:rect`……）的位置、大小、
/// 填充轮廓，里面的图（`v:imagedata`）或文本框（`v:textbox`）。组合（`v:group`）
/// 只取位置、大小与替代文字。
fn parse_vml(r: &mut Rd, root: &str, level: usize) -> Result<Drawing> {
    let mut d = Drawing {
        inline: true,
        ..Default::default()
    };
    let mut depth = 1usize;
    let mut shape_seen = false;
    let mut composite = false;
    loop {
        let ev = r.read_event().map_err(xml_err)?;
        let (e, empty) = match &ev {
            Event::Start(e) => (e, false),
            Event::Empty(e) => (e, true),
            Event::End(e) if e.local_name().as_ref() == root => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
                continue;
            }
            Event::Eof => break,
            _ => continue,
        };
        let name = e.local_name().as_ref().to_string();
        if name == root && !empty {
            depth += 1;
        }
        match name.as_str() {
            // 新版 Word 在 `w:object` 里也写 DrawingML。
            "docPr" => {
                d.alt = d
                    .alt
                    .take()
                    .or_else(|| attr(e, "descr").filter(|s| !s.trim().is_empty()));
            }
            "group" if !shape_seen => {
                shape_seen = true;
                composite = true;
                read_vml_shape(&mut d, e, &name);
            }
            "shape" | "rect" | "roundrect" | "oval" | "line" | "image" | "polyline" | "arc"
            | "curve"
                if !shape_seen =>
            {
                shape_seen = true;
                read_vml_shape(&mut d, e, &name);
            }
            // 填充、描边也可以写成形状的子元素：`on="f"` 是没有。
            "fill" | "stroke" if shape_seen => {
                if let Some(shape) = d.shape.as_mut() {
                    let off = attr(e, "on").is_some_and(|v| matches!(v.as_str(), "f" | "false"));
                    let color = attr(e, "color").and_then(|c| vml_color(&c));
                    if name == "fill" {
                        if off {
                            shape.fill = None;
                        } else if let (Some(fill), Some(c)) = (shape.fill.as_mut(), color) {
                            *fill = c;
                        }
                    } else if off {
                        shape.line = None;
                    } else if let Some((w, c)) = shape.line.as_mut() {
                        *w = attr(e, "weight").and_then(|v| vml_length(&v)).unwrap_or(*w);
                        *c = color.unwrap_or(*c);
                    }
                }
            }
            "imagedata" if shape_seen => {
                d.alt = d
                    .alt
                    .take()
                    .or_else(|| attr(e, "title").filter(|s| !s.trim().is_empty()));
                let target = attr(e, "id").or_else(|| attr(e, "relid"));
                let crop = |k| {
                    let v = attr(e, k)?;
                    let v = v.trim();
                    // `6554f` 是以 65536 为 1 的分数。
                    let f = match v.strip_suffix('f') {
                        Some(n) => n.parse::<f64>().ok()? / 65536.0,
                        None => v.parse::<f64>().ok()?,
                    };
                    Some((f * 100_000.0).round() as i32)
                };
                d.picture = Some(Picture {
                    target,
                    crop: ["cropleft", "croptop", "cropright", "cropbottom"]
                        .map(|k| crop(k).unwrap_or(0)),
                });
                // 图片框的填充、轮廓来自形状类型，本版本不读，图片就只画图。
                d.shape = None;
            }
            "textbox" if shape_seen => {
                if let Some(shape) = d.shape.as_mut() {
                    // `inset="7.2pt,3.6pt,7.2pt,3.6pt"`：左、上、右、下，空着的用缺省。
                    if let Some(inset) = attr(e, "inset") {
                        for (slot, v) in shape.insets.iter_mut().zip(inset.split(',')) {
                            *slot = vml_length(v).unwrap_or(*slot);
                        }
                    }
                }
            }
            "txbxContent" if !empty => {
                let story = text_box(r, level)?;
                if let Some(shape) = d.shape.as_mut() {
                    shape.text = Some(story);
                }
            }
            "wrap" => {
                if let Some(a) = d.anchor.as_mut() {
                    a.wrap = match attr(e, "type").as_deref() {
                        Some("topAndBottom") => WrapKind::TopAndBottom,
                        Some("square") => WrapKind::Square,
                        Some("tight") => WrapKind::Tight,
                        Some("through") => WrapKind::Through,
                        _ => WrapKind::None,
                    };
                }
            }
            _ => {}
        }
    }
    if composite {
        d.picture = None;
        d.shape = None;
    }
    if !shape_seen {
        // 没有形状，也就没有大小。
        d.extent = None;
    }
    Ok(d)
}

/// VML 形状元素上的样式：位置、大小、层次、填充与轮廓。
fn read_vml_shape(d: &mut Drawing, e: &BytesStart, name: &str) {
    let style: std::collections::HashMap<String, String> = attr(e, "style")
        .unwrap_or_default()
        .split(';')
        .filter_map(|kv| {
            let (k, v) = kv.split_once(':')?;
            Some((k.trim().to_ascii_lowercase(), v.trim().to_string()))
        })
        .collect();
    let len = |k: &str| style.get(k).and_then(|v| vml_length(v));
    let geometry = vml_geometry(e, name);
    // 位置与大小：直线写的是两个端点（`from`、`to`），其余写在样式里。
    let point = |k| {
        let v = attr(e, k)?;
        let (x, y) = v.split_once(',')?;
        Some((vml_length(x)?, vml_length(y)?))
    };
    let ends = (name == "line")
        .then(|| point("from").zip(point("to")))
        .flatten();
    let mut offset = (
        len("margin-left").or_else(|| len("left")),
        len("margin-top").or_else(|| len("top")),
    );
    let mut flip = style
        .get("flip")
        .map_or([false; 2], |f| [f.contains('x'), f.contains('y')]);
    match ends {
        Some(((x1, y1), (x2, y2))) => {
            d.extent = Some(((x2 - x1).abs(), (y2 - y1).abs()));
            offset = (
                Some(offset.0.unwrap_or(0) + x1.min(x2)),
                Some(offset.1.unwrap_or(0) + y1.min(y2)),
            );
            // 从左下到右上的线：相当于左右翻转过。
            flip = [(x2 - x1).signum() * (y2 - y1).signum() < 0, false];
        }
        None => d.extent = len("width").zip(len("height")),
    }
    d.alt = d
        .alt
        .take()
        .or_else(|| attr(e, "alt").filter(|s| !s.trim().is_empty()));
    if style.get("position").is_some_and(|v| v == "absolute") {
        d.inline = false;
        // 换成 DrawingML 的写法；没写就是相对文字（横向是栏，竖向是段落）。
        let from = |k: &str, text: &str| {
            let v = style.get(k).map_or("text", String::as_str);
            Some(
                match v {
                    "text" => text,
                    "char" => "character",
                    "left-margin-area" => "leftMargin",
                    "right-margin-area" => "rightMargin",
                    "inner-margin-area" => "insideMargin",
                    "outer-margin-area" => "outsideMargin",
                    "top-margin-area" => "topMargin",
                    "bottom-margin-area" => "bottomMargin",
                    other => other,
                }
                .to_string(),
            )
        };
        let align = |k: &str| style.get(k).filter(|v| *v != "absolute").cloned();
        d.anchor = Some(Anchor {
            h: AnchorPos {
                from: from("mso-position-horizontal-relative", "column"),
                offset: offset.0,
                align: align("mso-position-horizontal"),
            },
            v: AnchorPos {
                from: from("mso-position-vertical-relative", "paragraph"),
                offset: offset.1,
                align: align("mso-position-vertical"),
            },
            behind: style
                .get("z-index")
                .and_then(|z| z.parse::<i64>().ok())
                .is_some_and(|z| z < 0),
            ..Default::default()
        });
    }
    // VML 的缺省：填白色、描 0.75pt 的黑边；写了 filled="f" / stroked="f" 就没有。
    // 直线没有填充。
    let off = |k: &str| attr(e, k).is_some_and(|v| matches!(v.as_str(), "f" | "false"));
    let fill = (!off("filled") && geometry != Geometry::Line).then(|| {
        attr(e, "fillcolor")
            .and_then(|c| vml_color(&c))
            .unwrap_or([255, 255, 255])
    });
    let line = (!off("stroked")).then(|| {
        let w = attr(e, "strokeweight")
            .and_then(|w| vml_length(&w))
            .unwrap_or(9525);
        let c = attr(e, "strokecolor")
            .and_then(|c| vml_color(&c))
            .unwrap_or([0, 0, 0]);
        (w, c)
    });
    // `rotation:90`（度），`fd` 结尾的以 65536 为 1 度。
    let rotation = style.get("rotation").and_then(|r| {
        let deg = match r.strip_suffix("fd") {
            Some(n) => n.trim().parse::<f64>().ok()? / 65536.0,
            None => r.trim().parse::<f64>().ok()?,
        };
        Some((deg * 60_000.0).round() as i64)
    });
    d.shape = Some(Shape {
        geometry,
        fill,
        line,
        text: None,
        insets: TEXT_INSETS,
        text_align: VAlign::Top,
        flip,
        rotation: rotation.unwrap_or(0),
    });
}

/// VML 形状的几何：看元素名；`v:shape` 看它的形状类型（`type="#_x0000_t202"`、
/// `o:spt="202"`），没写类型的按矩形。
fn vml_geometry(e: &BytesStart, name: &str) -> Geometry {
    match name {
        "line" => Geometry::Line,
        "roundrect" => Geometry::RoundRect,
        "rect" | "image" | "group" => Geometry::Rect,
        "shape" => {
            let spt = attr(e, "spt").or_else(|| {
                attr(e, "type").and_then(|t| t.rsplit_once("_t").map(|(_, n)| n.to_string()))
            });
            match spt.as_deref().map(str::trim) {
                // 矩形、图片框、文本框。
                None | Some("1" | "75" | "202") => Geometry::Rect,
                Some("2") => Geometry::RoundRect,
                // 直线、直线连接符。
                Some("20" | "32") => Geometry::Line,
                Some(_) => Geometry::Other,
            }
        }
        _ => Geometry::Other,
    }
}

/// 容器（表格、文本框）最多套几层。再往里的只留下文字：正常的文书不会套这么深，
/// 一层层递归下去却可能把栈用完。
const MAX_DEPTH: usize = 16;

/// 文本框里的内容（`w:txbxContent`），与正文一样读。`level`：文本框所在段落外面
/// 套着几层容器。
fn text_box(r: &mut Rd, level: usize) -> Result<Story> {
    if level >= MAX_DEPTH {
        return Ok(vec![Block::Para(flat_text(r, "txbxContent")?)]);
    }
    let mut story = Vec::new();
    parse_blocks(r, "txbxContent", &mut story, &mut None, level + 1)?;
    Ok(story)
}

/// 读一张表格。`depth`：它是第几层容器（正文里的表格是 1）。
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
    let mut alts = Alternates::default();
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) if alts.skips(&e) => {
                let name = e.local_name().as_ref().to_string();
                skip(r, &name)?
            }
            Event::End(e) if alts.ends(&e) => {}
            Event::Start(e) => match e.local_name().as_ref() {
                "tcPr" => cell.props = parse_tc_pr(r)?,
                "p" => cell.content.push(Block::Para(parse_paragraph(r, depth)?)),
                "tbl" if depth >= MAX_DEPTH => cell.content.push(Block::Para(flat_text(r, "tbl")?)),
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

/// 套得太深的表格、文本框（根元素 `root`）：不再解析结构，把里面的字收成一段，
/// 段与段之间隔一个空格。
fn flat_text(r: &mut Rd, root: &str) -> Result<Para> {
    let mut depth = 1usize;
    let mut text = String::new();
    let mut in_text = false;
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) => match e.local_name().as_ref() {
                name if name == root => depth += 1,
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
                name if name == root => {
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
