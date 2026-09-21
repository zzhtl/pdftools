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

use std::collections::HashSet;

use super::model::{PPr, RPr, Style, Styles};

pub struct Resolver<'a> {
    styles: &'a Styles,
}

impl<'a> Resolver<'a> {
    pub fn new(styles: &'a Styles) -> Self {
        Self { styles }
    }

    /// 展开一条 basedOn 链，返回从根到叶的顺序。
    fn chain(&self, map: &'a std::collections::HashMap<String, Style>, id: &str) -> Vec<&'a Style> {
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
