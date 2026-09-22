//! 用 rustybuzz 做文本整形。
//!
//! 输出刻意保留**字体单位**而不是点：调用方（断行、绘制）各自按字号缩放，
//! 避免在多个地方重复做同一个浮点换算并积累误差。

use super::FontFace;

/// 整形后的单个字形。
#[derive(Debug, Clone, Copy)]
pub struct ShapedGlyph {
    pub gid: u16,
    /// 该字形对应源字符串的起始字节偏移（HarfBuzz cluster）。
    /// 断行、ToUnicode 映射都靠它把字形和原文对回去。
    pub cluster: u32,
    /// 整形后的步进，字体单位。含 GPOS 调整。
    pub x_advance: i32,
}

#[derive(Debug, Clone)]
pub struct ShapedRun {
    pub glyphs: Vec<ShapedGlyph>,
    /// `prefix_width[i]` = 前 i 个字形的步进之和（字体单位）。
    /// 有了它，「从第 a 个到第 b 个字形有多宽」是一次减法而不是一次求和，
    /// 贪心断行会反复问这个问题。
    prefix_width: Vec<i64>,
}

impl ShapedRun {
    /// 字形区间 `[from, to)` 的宽度，字体单位。
    pub fn width_between(&self, from: usize, to: usize) -> i64 {
        self.prefix_width[to.min(self.glyphs.len())]
            - self.prefix_width[from.min(self.glyphs.len())]
    }

    pub fn total_width(&self) -> i64 {
        self.width_between(0, self.glyphs.len())
    }

    /// 找到源字节偏移 `byte` 对应的字形下标（第一个 cluster >= byte 的位置）。
    pub fn glyph_index_at_byte(&self, byte: u32) -> usize {
        self.glyphs.partition_point(|g| g.cluster < byte)
    }
}

/// 对一段**同字体、同 script** 的文本整形。
pub fn shape_run(face: &FontFace, text: &str, script: rustybuzz::Script) -> ShapedRun {
    let rb = match rustybuzz::Face::from_slice(face.data(), face.index()) {
        Some(f) => f,
        None => {
            return ShapedRun {
                glyphs: Vec::new(),
                prefix_width: vec![0],
            }
        }
    };

    let mut buf = rustybuzz::UnicodeBuffer::new();
    buf.push_str(text);
    buf.set_direction(rustybuzz::Direction::LeftToRight);
    buf.set_script(script);

    let out = rustybuzz::shape(&rb, &[], buf);
    let infos = out.glyph_infos();
    let positions = out.glyph_positions();

    let mut glyphs = Vec::with_capacity(infos.len());
    let mut prefix_width = Vec::with_capacity(infos.len() + 1);
    let mut acc: i64 = 0;
    prefix_width.push(0);

    for (info, pos) in infos.iter().zip(positions.iter()) {
        glyphs.push(ShapedGlyph {
            gid: info.glyph_id as u16,
            cluster: info.cluster,
            x_advance: pos.x_advance,
        });
        acc += pos.x_advance as i64;
        prefix_width.push(acc);
    }

    ShapedRun {
        glyphs,
        prefix_width,
    }
}

/// 按 Unicode script 把字符串切段。
///
/// 目的不是做双向文字，而是为了决定**每段用哪个字体**：docx 的每个 run 都带
/// `w:rFonts w:ascii=".." w:eastAsia=".."` 两个字体，拉丁文和汉字必须分开用。
/// 这些法律文书里满是身份证号、电话、邮箱，混在汉字段落中间。
///
/// ASCII 可见字符（字母、数字、半角标点）一律归西文 —— Word 的 `w:rFonts w:ascii`
/// 管的就是这一段。只有空格这类既不属于中文也不属于西文的字符才继承相邻字符的归属。
pub fn split_by_script(text: &str) -> Vec<(std::ops::Range<usize>, ScriptClass)> {
    use unicode_script::{Script, UnicodeScript};

    /// 中日韩标点在 Unicode 里的 script 属性是 Common，但排版上必须跟中文字体走。
    ///
    /// 这条很要命：`；`（U+FF1B）跟在 "PDF" 后面时，按 Common 继承规则会被判给
    /// 西文字体，而 Liberation Serif 之类的西文字体没有全角标点字形，
    /// 结果就是 `.notdef`（方框），且多个缺字都塌缩到 GID 0、连带把 ToUnicode 也搞错。
    fn is_cjk_punctuation(c: char) -> bool {
        matches!(c as u32,
            0x3000..=0x303F   // CJK 符号和标点：、。〈〉《》「」〔〕
            | 0xFE10..=0xFE1F // 竖排标点
            | 0xFE30..=0xFE4F // CJK 兼容形式
            | 0xFF01..=0xFF60 // 全角 ASCII 与标点：，；：！？（）
            | 0xFFE0..=0xFFE6 // 全角货币符号
        )
    }

    fn strong(c: char) -> Option<ScriptClass> {
        if is_cjk_punctuation(c) {
            return Some(ScriptClass::EastAsian);
        }
        // ASCII 可见字符（字母、数字、半角标点）一律走西文字体。
        //
        // 这条是 Word 的规则：`w:rFonts w:ascii` 管的就是 0x00-0x7F 这一段。
        // 之前把数字当 Common 让它继承前一个汉字，导致「第9条」整体被当成
        // 中文，既用错了字体，也让中西文之间的自动间距无从插入。
        if c.is_ascii_graphic() {
            return Some(ScriptClass::Latin);
        }
        match c.script() {
            Script::Han
            | Script::Hiragana
            | Script::Katakana
            | Script::Hangul
            | Script::Bopomofo => Some(ScriptClass::EastAsian),
            Script::Common | Script::Inherited | Script::Unknown => None,
            _ => Some(ScriptClass::Latin),
        }
    }

    let chars: Vec<(usize, char)> = text.char_indices().collect();
    if chars.is_empty() {
        return Vec::new();
    }

    // 第一趟：定每个字符的归属，Common 先留空。
    let mut cls: Vec<Option<ScriptClass>> = chars.iter().map(|(_, c)| strong(*c)).collect();

    // 第二趟：Common 向前继承；段首的 Common 向后继承；全是 Common 则算作拉丁。
    let mut last: Option<ScriptClass> = None;
    for slot in cls.iter_mut() {
        match *slot {
            Some(c) => last = Some(c),
            None => *slot = last,
        }
    }
    let mut next: Option<ScriptClass> = None;
    for slot in cls.iter_mut().rev() {
        match *slot {
            Some(c) => next = Some(c),
            None => *slot = next,
        }
    }
    let resolved: Vec<ScriptClass> = cls
        .into_iter()
        .map(|c| c.unwrap_or(ScriptClass::Latin))
        .collect();

    // 第三趟：合并相邻同类。
    let mut out: Vec<(std::ops::Range<usize>, ScriptClass)> = Vec::new();
    for (i, (byte, ch)) in chars.iter().enumerate() {
        let end = byte + ch.len_utf8();
        match out.last_mut() {
            Some((range, c)) if *c == resolved[i] => range.end = end,
            _ => out.push((*byte..end, resolved[i])),
        }
    }
    out
}

/// 只区分「用东亚字体」还是「用拉丁字体」，不做完整的 script 分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptClass {
    EastAsian,
    Latin,
}

impl ScriptClass {
    pub fn to_rustybuzz(self) -> rustybuzz::Script {
        match self {
            ScriptClass::EastAsian => rustybuzz::script::HAN,
            ScriptClass::Latin => rustybuzz::script::LATIN,
        }
    }
}

/// 把每个字形映射回它在原文里对应的子串。
///
/// 这是构造 `/ToUnicode` 的正确做法。反查 cmap 是错的：连字是一对多、
/// CJK 里又常有多码位映射到同一字形，反查只能得到其中任意一个。
/// HarfBuzz 的 cluster 按定义就是「这个字形来自原文的哪一段」。
pub fn cluster_texts(text: &str, glyphs: &[ShapedGlyph]) -> Vec<(u16, String)> {
    let mut out = Vec::with_capacity(glyphs.len());
    for (i, g) in glyphs.iter().enumerate() {
        let start = g.cluster as usize;
        // 同一个 cluster 可能对应多个字形（一对多）。此时只有第一个字形承载原文，
        // 其余的映射为空，避免复制出重复的字符。
        if i > 0 && glyphs[i - 1].cluster == g.cluster {
            out.push((g.gid, String::new()));
            continue;
        }
        // 下一个不同的 cluster 就是本段的终点。
        let end = glyphs[i + 1..]
            .iter()
            .find(|n| n.cluster != g.cluster)
            .map(|n| n.cluster as usize)
            .unwrap_or(text.len());
        let slice = text.get(start..end).unwrap_or("");
        out.push((g.gid, slice.to_string()));
    }
    out
}
