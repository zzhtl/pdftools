//! 样式层叠。
//!
//! ECMA-376 §17.7.2 规定的优先级，从低到高：
//!   1. `docDefaults`
//!   2. 标了 `w:default="1"` 的那个段落样式（通常是 Normal）
//!   3. `w:pStyle` 指向的样式链，**沿 basedOn 回溯到根，再从根往下套用**
//!   4. 段落上的直接 `w:pPr`
//!   5. 段落标记的 `w:pPr/w:rPr`（只影响 run）
//!   6. `w:rStyle` 样式链
//!   7. run 上的直接 `w:rPr`
//!
//! 现实中的文件会出现 `basedOn` 成环，所以回溯必须带访问集合。
//!
//! 重写前的实现有两处与规范不符，按 `Cascade::Legacy` 保留以便对照：
//! 设了 `w:pStyle` 仍把 Normal 叠上去；段落标记的格式（5）被套到每个 run 上，
//! 而它本来只管段落标记自己。按规范（`Cascade::Spec`）时，开关属性（粗体、斜体……）
//! 在段落样式与字符样式之间取异或：两边都开就是关。

use std::collections::{HashMap, HashSet};

use super::model::{PPr, RPr, Style, Styles};

pub struct Resolver<'a> {
    styles: &'a Styles,
    spec: bool,
}

impl<'a> Resolver<'a> {
    /// `spec`：按规范层叠；否则复刻重写前的做法。
    pub fn new(styles: &'a Styles, spec: bool) -> Self {
        Self { styles, spec }
    }

    /// 段落用的样式链：写了 `w:pStyle` 就是它，没写才是默认段落样式。
    fn paragraph_chain(&self, style_id: Option<&str>) -> Vec<&'a Style> {
        style_id
            .or(self.styles.default_paragraph_style.as_deref())
            .map(|id| self.chain(&self.styles.paragraph, id))
            .unwrap_or_default()
    }

    /// 一条样式链上的字符属性，逐级覆盖。
    fn rpr_of(chain: &[&Style]) -> RPr {
        let mut out = RPr::default();
        for st in chain {
            out.merge(&st.rpr);
        }
        out
    }

    /// 段落标记（¶）的字符属性：docDefaults、段落样式、段落标记自己的 rPr。
    pub fn mark(&self, para: &PPr) -> RPr {
        if !self.spec {
            return self.run(para, &RPr::default());
        }
        let mut out = self.styles.doc_default_rpr.clone();
        out.merge(&Self::rpr_of(
            &self.paragraph_chain(para.style_id.as_deref()),
        ));
        if let Some(id) = &para.mark_rpr.style_id {
            out.merge(&Self::rpr_of(&self.chain(&self.styles.character, id)));
        }
        out.merge(&para.mark_rpr);
        out
    }

    /// 展开一条 basedOn 链，返回从根到叶的顺序。
    fn chain(&self, map: &'a HashMap<String, Style>, id: &str) -> Vec<&'a Style> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut cur = Some(id.to_string());
        while let Some(c) = cur {
            if !seen.insert(c.clone()) {
                // basedOn 成环。停下来用已经收集到的部分，不要死循环。
                break;
            }
            let Some(st) = map.get(&c) else { break };
            out.push(st);
            cur = st.based_on.clone();
        }
        // 从根往叶套用，所以反过来。
        out.reverse();
        out
    }

    pub fn paragraph(&self, direct: &PPr) -> PPr {
        let mut out = self.styles.doc_default_ppr.clone();
        if self.spec {
            for st in self.paragraph_chain(direct.style_id.as_deref()) {
                out.merge(&st.ppr);
            }
            out.merge(direct);
            return out;
        }

        if let Some(def) = &self.styles.default_paragraph_style {
            if Some(def.as_str()) != direct.style_id.as_deref() {
                for st in self.chain(&self.styles.paragraph, def) {
                    out.merge(&st.ppr);
                }
            }
        }
        if let Some(id) = &direct.style_id {
            for st in self.chain(&self.styles.paragraph, id) {
                out.merge(&st.ppr);
            }
        }
        out.merge(direct);
        out
    }

    /// `para` 必须是已经展开过的段落属性。
    pub fn run(&self, para: &PPr, direct: &RPr) -> RPr {
        let mut out = self.styles.doc_default_rpr.clone();
        if self.spec {
            let from_para = Self::rpr_of(&self.paragraph_chain(para.style_id.as_deref()));
            let from_char = direct
                .style_id
                .as_deref()
                .map(|id| Self::rpr_of(&self.chain(&self.styles.character, id)))
                .unwrap_or_default();
            out.merge(&from_para);
            out.merge(&from_char);
            toggle(&mut out, &from_para, &from_char);
            out.merge(direct);
            return out;
        }

        if let Some(def) = &self.styles.default_paragraph_style {
            for st in self.chain(&self.styles.paragraph, def) {
                out.merge(&st.rpr);
            }
        }
        if let Some(id) = &para.style_id {
            for st in self.chain(&self.styles.paragraph, id) {
                out.merge(&st.rpr);
            }
        }
        // 段落标记自身的格式，优先级在字符样式之下。
        out.merge(&para.mark_rpr);
        if let Some(id) = &direct.style_id {
            for st in self.chain(&self.styles.character, id) {
                out.merge(&st.rpr);
            }
        }
        out.merge(direct);
        out
    }
}

/// 开关属性：段落样式与字符样式都写了时取异或（ECMA-376 §17.7.3）。
/// 只写了一边时就是那一边，已经由逐级覆盖得到。
fn toggle(out: &mut RPr, para: &RPr, chr: &RPr) {
    macro_rules! xor {
        ($($f:ident),*) => { $(
            if let (Some(a), Some(b)) = (para.$f, chr.$f) {
                out.$f = Some(a ^ b);
            }
        )* };
    }
    xor!(bold, italic, caps, small_caps, strike, vanish);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(id: &str, based_on: Option<&str>, ppr: PPr, rpr: RPr) -> Style {
        Style {
            id: id.into(),
            based_on: based_on.map(Into::into),
            ppr,
            rpr,
        }
    }

    fn styles() -> Styles {
        let mut s = Styles {
            default_paragraph_style: Some("Normal".into()),
            ..Default::default()
        };
        s.paragraph.insert(
            "Normal".into(),
            style(
                "Normal",
                None,
                PPr {
                    space_after_twips: Some(200),
                    ..Default::default()
                },
                RPr {
                    size_half_pt: Some(21),
                    ..Default::default()
                },
            ),
        );
        // 不基于 Normal 的标题样式：Normal 的段后距、字号都不该带进来。
        s.paragraph.insert(
            "Title".into(),
            style(
                "Title",
                None,
                PPr::default(),
                RPr {
                    bold: Some(true),
                    ..Default::default()
                },
            ),
        );
        s.character.insert(
            "Strong".into(),
            style(
                "Strong",
                None,
                PPr::default(),
                RPr {
                    bold: Some(true),
                    ..Default::default()
                },
            ),
        );
        s
    }

    #[test]
    fn a_paragraph_style_does_not_pull_in_normal() {
        let s = styles();
        let r = Resolver::new(&s, true);
        let ppr = r.paragraph(&PPr {
            style_id: Some("Title".into()),
            ..Default::default()
        });
        assert_eq!(ppr.space_after_twips, None);
        assert_eq!(r.run(&ppr, &RPr::default()).size_half_pt, None);
        // 旧做法会把 Normal 叠进来。
        let legacy = Resolver::new(&s, false);
        assert_eq!(
            legacy
                .paragraph(&PPr {
                    style_id: Some("Title".into()),
                    ..Default::default()
                })
                .space_after_twips,
            Some(200)
        );
    }

    #[test]
    fn the_paragraph_mark_formats_only_the_mark() {
        let s = styles();
        let r = Resolver::new(&s, true);
        let ppr = r.paragraph(&PPr {
            mark_rpr: RPr {
                size_half_pt: Some(48),
                ..Default::default()
            },
            ..Default::default()
        });
        assert_eq!(r.run(&ppr, &RPr::default()).size_half_pt, Some(21));
        assert_eq!(r.mark(&ppr).size_half_pt, Some(48));
    }

    #[test]
    fn toggles_cancel_between_paragraph_and_character_styles() {
        let s = styles();
        let r = Resolver::new(&s, true);
        let ppr = r.paragraph(&PPr {
            style_id: Some("Title".into()),
            ..Default::default()
        });
        let strong = RPr {
            style_id: Some("Strong".into()),
            ..Default::default()
        };
        assert_eq!(
            r.run(&ppr, &strong).bold,
            Some(false),
            "两边都加粗就是不加粗"
        );
        // 直接格式不是切换，写了就是它。
        let direct = RPr {
            bold: Some(true),
            ..strong
        };
        assert_eq!(r.run(&ppr, &direct).bold, Some(true));
    }
}
