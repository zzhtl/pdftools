//! `numbering.xml`：编号定义。坏了不影响转换 —— 能读到多少用多少，读不到的编号
//! 按没有编号处理。

use quick_xml::events::Event;
use quick_xml::Reader;

use super::{attr, attr_i32, on_off, props, xml_err, Rd};
use crate::docx::model::{AbstractNum, Align, Level, LevelOverride, Num, NumSuffix, Numbering};
use crate::error::Result;

pub fn parse_numbering(xml: &str) -> Numbering {
    let mut r = Reader::from_str(xml);
    let mut out = Numbering::default();
    while let Ok(ev) = r.read_event() {
        match ev {
            Event::Start(e) if e.local_name().as_ref() == "abstractNum" => {
                let id = attr_i32(&e, "abstractNumId");
                if let (Some(id), Ok(a)) = (id, parse_abstract(&mut r)) {
                    out.abstracts.insert(id, a);
                }
            }
            Event::Start(e) if e.local_name().as_ref() == "num" => {
                let id = attr_i32(&e, "numId");
                if let (Some(id), Ok(n)) = (id, parse_num(&mut r)) {
                    out.nums.insert(id, n);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// 0–8 级之外的级别号不认。
fn level_index(v: Option<i32>) -> Option<u8> {
    v.filter(|i| (0..9).contains(i)).map(|i| i as u8)
}

fn parse_abstract(r: &mut Rd) -> Result<AbstractNum> {
    let mut a = AbstractNum::default();
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) if e.local_name().as_ref() == "lvl" => {
                let ilvl = level_index(attr_i32(&e, "ilvl"));
                let level = parse_level(r)?;
                if let Some(i) = ilvl {
                    a.levels.insert(i, level);
                }
            }
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "numStyleLink" => {
                a.num_style_link = attr(&e, "val");
            }
            Event::End(e) if e.local_name().as_ref() == "abstractNum" => break,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(a)
}

/// `w:lvl` 里的内容，读到它的结束标签为止。
///
/// 新版 Word 把自定义格式包在 `mc:AlternateContent` 里，后面的 `mc:Fallback` 再写一个
/// 通用格式。这里不区分，依次读下来，留下的正好是 Fallback 的那个。
fn parse_level(r: &mut Rd) -> Result<Level> {
    let mut l = Level::default();
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) if e.local_name().as_ref() == "pPr" => l.ppr = props::parse_ppr(r)?.0,
            Event::Start(e) if e.local_name().as_ref() == "rPr" => l.rpr = props::parse_rpr(r)?,
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                "start" => l.start = attr_i32(&e, "val"),
                "numFmt" => l.format = attr(&e, "val"),
                "lvlText" => l.text = attr(&e, "val"),
                "lvlRestart" => l.restart = attr_i32(&e, "val"),
                "isLgl" => l.legal = on_off(&e),
                "suff" => {
                    l.suffix = match attr(&e, "val").as_deref() {
                        Some("tab") => Some(NumSuffix::Tab),
                        Some("space") => Some(NumSuffix::Space),
                        Some("nothing") => Some(NumSuffix::Nothing),
                        _ => None,
                    }
                }
                "lvlJc" => {
                    l.align = match attr(&e, "val").as_deref() {
                        Some("left" | "start") => Some(Align::Left),
                        Some("center") => Some(Align::Center),
                        Some("right" | "end") => Some(Align::Right),
                        _ => None,
                    }
                }
                "lvlPicBulletId" => l.picture_bullet = true,
                _ => {}
            },
            Event::End(e) if e.local_name().as_ref() == "lvl" => break,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(l)
}

fn parse_num(r: &mut Rd) -> Result<Num> {
    let mut n = Num {
        abstract_id: -1,
        ..Num::default()
    };
    // 正在读的 `w:lvlOverride`。
    let mut current: Option<(u8, LevelOverride)> = None;
    loop {
        match r.read_event().map_err(xml_err)? {
            Event::Start(e) if e.local_name().as_ref() == "lvlOverride" => {
                current = level_index(attr_i32(&e, "ilvl")).map(|i| (i, LevelOverride::default()));
            }
            Event::Start(e) if e.local_name().as_ref() == "lvl" => {
                let level = parse_level(r)?;
                if let Some((_, o)) = &mut current {
                    o.level = Some(level);
                }
            }
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                "abstractNumId" => n.abstract_id = attr_i32(&e, "val").unwrap_or(-1),
                "startOverride" => {
                    if let Some((_, o)) = &mut current {
                        o.start = attr_i32(&e, "val");
                    }
                }
                _ => {}
            },
            Event::End(e) if e.local_name().as_ref() == "lvlOverride" => {
                if let Some((i, o)) = current.take() {
                    n.overrides.insert(i, o);
                }
            }
            Event::End(e) if e.local_name().as_ref() == "num" => break,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(n)
}
