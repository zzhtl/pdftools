//! 样式层叠。
//!
//! ECMA-376 §17.7.2 规定的优先级，从低到高：
//!   1. `docDefaults`
//!   2. 表格里的段落：表格样式（连同首行、隔行这些条件格式）
//!   3. 标了 `w:default="1"` 的那个段落样式（通常是 Normal）
//!   4. `w:pStyle` 指向的样式链，**沿 basedOn 回溯到根，再从根往下套用**
//!   5. 段落上的直接 `w:pPr`
//!   6. 段落标记的 `w:pPr/w:rPr`（只影响 run）
//!   7. `w:rStyle` 样式链
//!   8. run 上的直接 `w:rPr`
//!
//! 现实中的文件会出现 `basedOn` 成环，所以回溯必须带访问集合。
//!
//! 设了 `w:pStyle` 就不再叠 Normal；段落标记的格式（6）只管段落标记自己，不套到
//! run 上。开关属性（粗体、斜体……）在段落样式与字符样式之间取异或：两边都开就是关。

use std::collections::{HashMap, HashSet};

use super::model::{Numbering, PPr, RPr, Style, Styles};

/// 表格样式给单元格里的段落、文字的格式，见 [`Resolver::paragraph_in`]。
#[derive(Debug, Clone, Default)]
pub struct TableLayer {
    pub ppr: PPr,
    pub rpr: RPr,
}

pub struct Resolver<'a> {
    styles: &'a Styles,
    /// 编号定义。给了才让编号级别的缩进、制表位参与层叠。
    numbering: Option<&'a Numbering>,
}

impl<'a> Resolver<'a> {
    pub fn new(styles: &'a Styles, numbering: Option<&'a Numbering>) -> Self {
        Self { styles, numbering }
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
        self.mark_in(para, None)
    }

    /// 同 [`mark`](Self::mark)，`table`：段落在表格里时表格样式给的格式。
    pub fn mark_in(&self, para: &PPr, table: Option<&TableLayer>) -> RPr {
        let mut out = self.styles.doc_default_rpr.clone();
        if let Some(t) = table {
            out.merge(&t.rpr);
        }
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
        self.paragraph_in(direct, None)
    }

    /// 同 [`paragraph`](Self::paragraph)，`table`：段落在表格里时表格样式给的格式，
    /// 优先级在 docDefaults 之上、段落样式（含 Normal）之下 —— LibreOffice 实测如此：
    /// Normal 自己写了段距时表格样式的段距不起作用，只写在 docDefaults 里时起作用。
    pub fn paragraph_in(&self, direct: &PPr, table: Option<&TableLayer>) -> PPr {
        let mut out = self.styles.doc_default_ppr.clone();
        if let Some(t) = table {
            out.cascade(&t.ppr);
        }
        let chain = self.paragraph_chain(direct.style_id.as_deref());
        // 编号级别的缩进、制表位插在层叠的哪一层（LibreOffice 实测）：编号直接写在
        // 段落上时在样式之后；来自样式时在写着编号的那个样式之前、它的基样式之后
        // —— 那个样式自己的缩进优先，基样式里的（常见的「首行缩进 2 字符」）不优先。
        let defining = chain.iter().rposition(|st| st.ppr.num_id.is_some());
        let at = match direct.num_id {
            Some(_) => chain.len(),
            None => defining.unwrap_or(chain.len()),
        };
        let level = self.numbering.and_then(|n| {
            let id = direct
                .num_id
                .or_else(|| chain.iter().rev().find_map(|st| st.ppr.num_id))?;
            let ilvl = direct
                .num_ilvl
                .or_else(|| chain.iter().rev().find_map(|st| st.ppr.num_ilvl))
                .unwrap_or(0);
            let (_, level) = n.level(self.styles, id, u8::try_from(ilvl).ok()?)?;
            Some(&level.ppr)
        });
        for (i, st) in chain.iter().enumerate() {
            if let (true, Some(l)) = (i == at, level) {
                out.cascade(l);
            }
            out.cascade(&st.ppr);
        }
        if let (true, Some(l)) = (at == chain.len(), level) {
            out.cascade(l);
        }
        out.cascade(direct);
        out
    }

    /// `para` 必须是已经展开过的段落属性。
    pub fn run(&self, para: &PPr, direct: &RPr) -> RPr {
        self.run_in(para, direct, None)
    }

    /// 同 [`run`](Self::run)，`table` 见 [`paragraph_in`](Self::paragraph_in)。
    pub fn run_in(&self, para: &PPr, direct: &RPr, table: Option<&TableLayer>) -> RPr {
        let mut out = self.styles.doc_default_rpr.clone();
        if let Some(t) = table {
            out.merge(&t.rpr);
        }
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
        let r = Resolver::new(&s, None);
        let ppr = r.paragraph(&PPr {
            style_id: Some("Title".into()),
            ..Default::default()
        });
        assert_eq!(ppr.space_after_twips, None);
        assert_eq!(r.run(&ppr, &RPr::default()).size_half_pt, None);
    }

    #[test]
    fn the_paragraph_mark_formats_only_the_mark() {
        let s = styles();
        let r = Resolver::new(&s, None);
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
        let r = Resolver::new(&s, None);
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

    /// 编号级别的缩进插在层叠的哪一层，首行缩进与悬挂缩进互斥（LibreOffice 实测）。
    /// 编号级别是左缩进 840、悬挂 840。
    #[test]
    fn numbering_level_indents_in_the_cascade() {
        use crate::docx::model::{Block, Settings};
        use crate::docx::parse::{parse_document, parse_numbering, parse_styles};
        let styles = parse_styles(
            r#"<w:styles xmlns:w="w">
<w:style w:type="paragraph" w:default="1" w:styleId="Normal"/>
<w:style w:type="paragraph" w:styleId="H"><w:pPr><w:numPr><w:numId w:val="1"/></w:numPr><w:ind w:left="0" w:firstLine="0"/></w:pPr></w:style>
<w:style w:type="paragraph" w:styleId="Base"><w:pPr><w:ind w:firstLine="420" w:firstLineChars="200"/></w:pPr></w:style>
<w:style w:type="paragraph" w:styleId="Hx"><w:basedOn w:val="Base"/><w:pPr><w:numPr><w:numId w:val="1"/></w:numPr></w:pPr></w:style>
<w:style w:type="paragraph" w:styleId="Hz"><w:pPr><w:numPr><w:numId w:val="1"/></w:numPr><w:ind w:left="420"/></w:pPr></w:style>
<w:style w:type="paragraph" w:styleId="S2"><w:pPr><w:ind w:left="0" w:firstLine="0"/></w:pPr></w:style>
</w:styles>"#,
        );
        let numbering = parse_numbering(
            r#"<w:numbering xmlns:w="w"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/><w:pPr><w:ind w:left="840" w:hanging="840"/></w:pPr></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#,
        );
        let resolver = Resolver::new(&styles, Some(&numbering));
        // (左, 首行, 首行字符, 悬挂)
        let indent = |ppr: &str| {
            let doc = parse_document(
                &format!(r#"<w:document xmlns:w="w"><w:body><w:p><w:pPr>{ppr}</w:pPr></w:p></w:body></w:document>"#),
                Styles::default(),
                Settings::default(),
            )
            .unwrap();
            let Block::Para(p) = &doc.body[0] else {
                unreachable!()
            };
            let i = resolver.paragraph(&p.ppr).indent;
            (
                i.left_twips,
                i.first_line_twips,
                i.first_line_chars,
                i.hanging_twips,
            )
        };
        let num = r#"<w:numPr><w:numId w:val="1"/></w:numPr>"#;
        // 编号来自样式：那个样式自己的缩进优先，首行缩进取代级别的悬挂。
        assert_eq!(
            indent(r#"<w:pStyle w:val="H"/>"#),
            (Some(0), Some(0), None, None)
        );
        // 基样式里的首行缩进不优先：级别的悬挂取代它。
        assert_eq!(
            indent(r#"<w:pStyle w:val="Hx"/>"#),
            (Some(840), None, None, Some(840))
        );
        // 样式只写了左缩进：悬挂仍来自级别。
        assert_eq!(
            indent(r#"<w:pStyle w:val="Hz"/>"#),
            (Some(420), None, None, Some(840))
        );
        // 编号直接写在段落上：级别在样式之后，直接格式在级别之后。
        assert_eq!(
            indent(&format!(r#"<w:pStyle w:val="S2"/>{num}"#)),
            (Some(840), None, None, Some(840))
        );
        assert_eq!(
            indent(&format!(r#"{num}<w:ind w:left="1260"/>"#)),
            (Some(1260), None, None, Some(840))
        );
        assert_eq!(
            indent(&format!(r#"{num}<w:ind w:firstLine="200"/>"#)),
            (Some(840), Some(200), None, None)
        );
    }
}
