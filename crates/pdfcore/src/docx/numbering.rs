//! 自动编号的计数：全文按顺序走一遍，给每个挂了编号的段落算出编号文字。
//!
//! 规则对照 LibreOffice 实测：
//! - 计数器按编号定义（abstractNum）共享：指向同一个定义的几个 `w:num` 接着编号；
//! - `w:startOverride` 在那个 num 第一次用到时从指定的数重新起头，之后仍与其他
//!   num 共用计数器；
//! - 上一级每往下走一项，下级重新计数（`w:lvlRestart` 另有规定时按它；LibreOffice
//!   不认 `w:lvlRestart w:val="0"`，这里按规范永不重新计数）；
//! - `%1.%2` 里的 `%1` 按第一级自己的格式写（「一.1」），本级写了 `w:isLgl` 时一律
//!   写阿拉伯数字（「1.1」）。

use std::collections::{BTreeSet, HashMap, HashSet};

use super::model::{Align, Level, NumSuffix, Numbering, RPr, Styles};
use super::numfmt;

/// 一个段落的编号。
pub struct Label {
    pub text: String,
    pub suffix: NumSuffix,
    /// 编号文字的格式（叠在段落标记的格式之上）。
    pub rpr: RPr,
    pub align: Align,
}

pub struct Lists<'a> {
    defs: &'a Numbering,
    styles: &'a Styles,
    /// abstractNumId → 各级当前的数。None 是还没用过（或刚被上级重置）。
    counters: HashMap<i32, [Option<i32>; 9]>,
    /// 已经按 `w:startOverride` 重新起过头的 (numId, 级别)。
    restarted: HashSet<(i32, u8)>,
    /// 不认识、按阿拉伯数字输出的格式。
    pub fallbacks: BTreeSet<String>,
}

impl<'a> Lists<'a> {
    pub fn new(defs: &'a Numbering, styles: &'a Styles) -> Self {
        Self {
            defs,
            styles,
            counters: HashMap::new(),
            restarted: HashSet::new(),
            fallbacks: BTreeSet::new(),
        }
    }

    /// numId、第 `ilvl` 级的下一项。numId 为 0、定义找不到时没有编号。
    pub fn next(&mut self, num_id: i32, ilvl: u8) -> Option<Label> {
        if num_id == 0 || ilvl > 8 {
            return None;
        }
        let (abs, level) = self.defs.level(self.styles, num_id, ilvl)?;
        let start = |l: &Level| l.start.unwrap_or(0);
        let counters = self.counters.entry(abs).or_insert([None; 9]);
        let k = ilvl as usize;
        let override_start = self
            .defs
            .nums
            .get(&num_id)
            .and_then(|n| n.overrides.get(&ilvl))
            .and_then(|o| o.start);
        counters[k] = match override_start {
            Some(s) if self.restarted.insert((num_id, ilvl)) => Some(s),
            _ => Some(counters[k].map_or(start(level), |c| c + 1)),
        };
        // 下级重新计数。
        for (j, slot) in counters.iter_mut().enumerate().skip(k + 1) {
            let restart = self
                .defs
                .level(self.styles, num_id, j as u8)
                .and_then(|(_, l)| l.restart);
            // lvlRestart 是 1 起的级别号：用到它或更高的级别时重置；0 是永不重置。
            let resets = match restart {
                None => true,
                Some(0) => false,
                Some(r) => (k as i32) < r,
            };
            if resets {
                *slot = None;
            }
        }
        let values = *counters;

        let text = if level.format.as_deref() == Some("bullet") {
            match level.text.as_deref() {
                // 图片项目符号画不出来，用一个圆点代替。
                _ if level.picture_bullet => "\u{2022}".to_string(),
                Some(t) => t.to_string(),
                None => String::new(),
            }
        } else {
            self.expand(level, num_id, &values)
        };
        Some(Label {
            text,
            suffix: level.suffix.unwrap_or(NumSuffix::Tab),
            rpr: level.rpr.clone(),
            align: level.align.unwrap_or(Align::Left),
        })
    }

    /// 把 `w:lvlText` 里的 `%1`…`%9` 换成各级的数。
    fn expand(&mut self, level: &Level, num_id: i32, values: &[Option<i32>; 9]) -> String {
        let template = level.text.as_deref().unwrap_or("");
        let mut out = String::new();
        let mut chars = template.chars().peekable();
        while let Some(c) = chars.next() {
            let n = match (c, chars.peek().and_then(|d| d.to_digit(10))) {
                ('%', Some(n @ 1..=9)) => n as u8,
                _ => {
                    out.push(c);
                    continue;
                }
            };
            chars.next();
            let j = n - 1;
            let Some((_, lj)) = self.defs.level(self.styles, num_id, j) else {
                continue;
            };
            // 还没用到的上级按它的起始值写（「1.1」而不是「.1」）。
            let v = values[j as usize].unwrap_or(lj.start.unwrap_or(0));
            let fmt = if level.legal {
                "decimal"
            } else {
                lj.format.as_deref().unwrap_or("decimal")
            };
            match numfmt::format(v, fmt) {
                Some(s) => out.push_str(&s),
                None => {
                    self.fallbacks.insert(fmt.to_string());
                    out.push_str(&v.to_string());
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docx::parse::{parse_numbering, parse_styles};

    /// 按顺序取 (numId, 级别) 的编号文字。
    fn labels(numbering: &str, seq: &[(i32, u8)]) -> Vec<String> {
        let defs = parse_numbering(&format!(
            r#"<w:numbering xmlns:w="w">{numbering}</w:numbering>"#
        ));
        let styles = parse_styles(r#"<w:styles xmlns:w="w"/>"#);
        let mut lists = Lists::new(&defs, &styles);
        seq.iter()
            .map(|&(id, l)| lists.next(id, l).map(|x| x.text).unwrap_or_default())
            .collect()
    }

    fn lvl(i: u8, fmt: &str, text: &str, extra: &str) -> String {
        format!(
            r#"<w:lvl w:ilvl="{i}"><w:start w:val="1"/><w:numFmt w:val="{fmt}"/>{extra}<w:lvlText w:val="{text}"/></w:lvl>"#
        )
    }

    /// 与 LibreOffice 实测一致：`%1` 按第一级的格式写；上级走一项下级重新计数。
    #[test]
    fn multi_level_text_and_restart() {
        let defs = format!(
            r#"<w:abstractNum w:abstractNumId="0">{}{}{}</w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#,
            lvl(0, "chineseCounting", "%1、", ""),
            lvl(1, "decimal", "%1.%2", ""),
            lvl(2, "lowerLetter", "(%3)", "")
        );
        let got = labels(
            &defs,
            &[(1, 0), (1, 1), (1, 1), (1, 2), (1, 0), (1, 1), (1, 2)],
        );
        assert_eq!(got, ["一、", "一.1", "一.2", "(a)", "二、", "二.1", "(a)"]);
    }

    #[test]
    fn legal_numbering_and_restart_rules() {
        let defs = format!(
            r#"<w:abstractNum w:abstractNumId="0">{}{}</w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
<w:abstractNum w:abstractNumId="1">{}{}</w:abstractNum><w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num>"#,
            lvl(0, "chineseCounting", "第%1章", ""),
            lvl(1, "decimal", "%1.%2", "<w:isLgl/>"),
            lvl(0, "decimal", "%1.", ""),
            lvl(1, "decimal", "%2)", r#"<w:lvlRestart w:val="0"/>"#)
        );
        let got = labels(&defs, &[(1, 0), (1, 1), (2, 0), (2, 1), (2, 0), (2, 1)]);
        // 第二组按规范永不重新计数（LibreOffice 在这里会从 1 重来）。
        assert_eq!(got, ["第一章", "1.1", "1.", "1)", "2.", "2)"]);
    }

    /// 与 LibreOffice 实测一致：几个 num 指向同一个定义时接着编号；startOverride
    /// 在那个 num 第一次用到时重新起头，之后大家继续共用计数器。numId 0 没有编号。
    #[test]
    fn nums_share_the_counter_of_their_definition() {
        let defs = format!(
            r#"<w:abstractNum w:abstractNumId="3">{}</w:abstractNum>
<w:num w:numId="4"><w:abstractNumId w:val="3"/></w:num>
<w:num w:numId="13"><w:abstractNumId w:val="3"/></w:num>
<w:num w:numId="14"><w:abstractNumId w:val="3"/><w:lvlOverride w:ilvl="0"><w:startOverride w:val="5"/></w:lvlOverride></w:num>"#,
            lvl(0, "decimal", "%1.", "")
        );
        let seq = [
            (4, 0),
            (4, 0),
            (13, 0),
            (14, 0),
            (14, 0),
            (4, 0),
            (13, 0),
            (0, 0),
            (99, 0),
        ];
        let got = labels(&defs, &seq);
        assert_eq!(got, ["1.", "2.", "3.", "5.", "6.", "7.", "8.", "", ""]);
    }

    #[test]
    fn bullets_and_unknown_formats() {
        let defs = format!(
            r#"<w:abstractNum w:abstractNumId="0">{}{}</w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#,
            lvl(0, "bullet", "\u{F0B7}", ""),
            lvl(1, "hebrew1", "%2.", "")
        );
        let parsed = parse_numbering(&format!(r#"<w:numbering xmlns:w="w">{defs}</w:numbering>"#));
        let styles = parse_styles(r#"<w:styles xmlns:w="w"/>"#);
        let mut lists = Lists::new(&parsed, &styles);
        assert_eq!(lists.next(1, 0).unwrap().text, "\u{F0B7}");
        assert_eq!(lists.next(1, 1).unwrap().text, "1.");
        assert!(lists.fallbacks.contains("hebrew1"));
    }

    /// `w:numStyleLink`：定义本身是空的，各级在编号样式所用的那个定义里。
    #[test]
    fn num_style_link_finds_the_real_definition() {
        let defs = parse_numbering(&format!(
            r#"<w:numbering xmlns:w="w"><w:abstractNum w:abstractNumId="0"><w:numStyleLink w:val="ListStyle"/></w:abstractNum>
<w:abstractNum w:abstractNumId="1"><w:styleLink w:val="ListStyle"/>{}</w:abstractNum>
<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num><w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num></w:numbering>"#,
            lvl(0, "upperRoman", "%1.", "")
        ));
        let styles = parse_styles(
            r#"<w:styles xmlns:w="w"><w:style w:type="numbering" w:styleId="ListStyle"><w:pPr><w:numPr><w:numId w:val="2"/></w:numPr></w:pPr></w:style></w:styles>"#,
        );
        let mut lists = Lists::new(&defs, &styles);
        assert_eq!(lists.next(1, 0).unwrap().text, "I.");
        assert_eq!(
            lists.next(2, 0).unwrap().text,
            "II.",
            "两边用的是同一个定义"
        );
    }
}
