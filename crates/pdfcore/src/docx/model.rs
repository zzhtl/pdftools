//! OOXML 的忠实映射。
//!
//! 这一层**不做样式层叠、不做单位换算**，所有值保持 OOXML 原始形态
//! （twips、半磅、1/100 字符、240 分之一行）。
//! 这样「属性有没有解析到」和「层叠有没有算对」是两个独立的测试面。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
    Justify,
}

/// `w:spacing/@w:lineRule`。这个枚举决定了 `w:line` 的含义，搞错的话每份文档页数都不对。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineRule {
    /// `w:line` 是 **240 分之一行**的倍数。312 就是 1.3 倍行距，不是 15.6 磅。
    Auto,
    /// `w:line` 是 twips，精确行高。
    Exact,
    /// `w:line` 是 twips，最小行高。
    AtLeast,
}

#[derive(Debug, Clone, Default)]
pub struct RPr {
    pub style_id: Option<String>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub strike: Option<bool>,
    /// `w:sz`，单位是**半磅**。
    pub size_half_pt: Option<u32>,
    pub color: Option<[u8; 3]>,
    /// `w:rFonts/@w:ascii`，西文字体。
    pub font_ascii: Option<String>,
    /// `w:rFonts/@w:eastAsia`，中日韩字体。一个 run 需要两个字体，
    /// 少了哪个都会让身份证号或者汉字其中之一显示成错的样子。
    pub font_east_asia: Option<String>,
}

impl RPr {
    /// 用 `other` 覆盖自身已设置的字段（`other` 优先级更高）。
    pub fn merge(&mut self, other: &RPr) {
        macro_rules! take {
            ($($f:ident),*) => { $( if other.$f.is_some() { self.$f = other.$f.clone(); } )* };
        }
        take!(
            style_id,
            bold,
            italic,
            underline,
            strike,
            size_half_pt,
            color,
            font_ascii,
            font_east_asia
        );
    }
}

#[derive(Debug, Clone, Default)]
pub struct Indent {
    pub left_twips: Option<i32>,
    pub right_twips: Option<i32>,
    pub first_line_twips: Option<i32>,
    pub hanging_twips: Option<i32>,
    /// 1/100 个字符。**优先级高于 twips 版本** —— Word 在两者都存在时用字符版，
    /// 而中文文档里「首行缩进 2 字符」几乎无处不在。
    pub left_chars: Option<i32>,
    pub first_line_chars: Option<i32>,
    pub hanging_chars: Option<i32>,
}

impl Indent {
    fn merge(&mut self, other: &Indent) {
        macro_rules! take {
            ($($f:ident),*) => { $( if other.$f.is_some() { self.$f = other.$f; } )* };
        }
        take!(
            left_twips,
            right_twips,
            first_line_twips,
            hanging_twips,
            left_chars,
            first_line_chars,
            hanging_chars
        );
    }
}

#[derive(Debug, Clone, Default)]
pub struct PPr {
    pub style_id: Option<String>,
    pub align: Option<Align>,
    pub indent: Indent,
    pub space_before_twips: Option<i32>,
    pub space_after_twips: Option<i32>,
    pub line: Option<i32>,
    pub line_rule: Option<LineRule>,
    pub page_break_before: Option<bool>,
    /// `w:snapToGrid`，段落是否参与行网格吸附。缺省为 true。
    pub snap_to_grid: Option<bool>,
    /// `w:autoSpaceDE`：中日韩文字与西文之间自动加间距。缺省为 true。
    pub auto_space_latin: Option<bool>,
    /// `w:autoSpaceDN`：中日韩文字与数字之间自动加间距。缺省为 true。
    pub auto_space_digits: Option<bool>,
    /// `w:pPr/w:rPr`：段落标记自身的格式。它参与 run 的层叠，优先级低于 run 上的直接格式。
    pub mark_rpr: RPr,
}

impl PPr {
    pub fn merge(&mut self, other: &PPr) {
        if other.style_id.is_some() {
            self.style_id = other.style_id.clone();
        }
        if other.align.is_some() {
            self.align = other.align;
        }
        self.indent.merge(&other.indent);
        if other.space_before_twips.is_some() {
            self.space_before_twips = other.space_before_twips;
        }
        if other.space_after_twips.is_some() {
            self.space_after_twips = other.space_after_twips;
        }
        if other.line.is_some() {
            self.line = other.line;
            self.line_rule = other.line_rule;
        }
        if other.page_break_before.is_some() {
            self.page_break_before = other.page_break_before;
        }
        if other.snap_to_grid.is_some() {
            self.snap_to_grid = other.snap_to_grid;
        }
        if other.auto_space_latin.is_some() {
            self.auto_space_latin = other.auto_space_latin;
        }
        if other.auto_space_digits.is_some() {
            self.auto_space_digits = other.auto_space_digits;
        }
        self.mark_rpr.merge(&other.mark_rpr);
    }
}

#[derive(Debug, Clone)]
pub struct RawRun {
    pub rpr: RPr,
    /// `w:tab` 转成 `\t`，`w:br` 转成 `\n`，`w:t` 原样拼接。
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct RawPara {
    pub ppr: PPr,
    pub runs: Vec<RawRun>,
    /// 段落内遇到的不渲染元素（内嵌图片、文本框、公式……）。
    /// 挂在段落上而不是汇总到文档级，是为了报告里能说出「第几页」。
    pub unsupported: Vec<UnsupportedKind>,
}

/// 本版本不渲染的内容。它是 IR 的一等公民，而不是一个被丢掉的分支 ——
/// 这样「诚实失败」就不是靠自觉，而是类型系统逼着上层去处理。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnsupportedKind {
    Table { rows: usize, cols: usize },
    Drawing { alt: Option<String> },
    TextBox,
    Field,
    Footnote,
    HeaderFooter,
    Math,
}

impl UnsupportedKind {
    pub fn label(&self) -> String {
        match self {
            UnsupportedKind::Table { rows, cols } => format!("表格（{rows} 行 × {cols} 列）"),
            UnsupportedKind::Drawing { .. } => "图片".into(),
            UnsupportedKind::TextBox => "文本框".into(),
            UnsupportedKind::Field => "域（页码/目录等）".into(),
            UnsupportedKind::Footnote => "脚注".into(),
            UnsupportedKind::HeaderFooter => "页眉页脚".into(),
            UnsupportedKind::Math => "公式".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum RawBlock {
    Para(RawPara),
    Unsupported {
        kind: UnsupportedKind,
        /// 能抽出来的纯文本。表格里的文字往往是文档里最重要的内容，
        /// 就算画不出表格也要把字留下。
        text: Vec<String>,
    },
}

/// `w:docGrid` —— 中文排版的**行网格**。
///
/// 这是中文文档排版的关键，漏掉它整篇的行密度就全错。实测 LibreOffice 的行为
/// （4 种字号、2 种 pitch 交叉验证）：网格生效时，单倍行高不是字体的自然行高，
/// 而是**向上吸附到 linePitch 的整数倍**，`lineRule="auto"` 的倍数再乘在这之上。
///
/// 例：12pt 宋体自然行高 17.4pt，linePitch=312twips(15.6pt) → 吸附成 31.2pt，
/// 再乘 1.3 倍行距 = 40.6pt。忽略网格只会得到 22.6pt，差 45%。
#[derive(Debug, Clone, Copy)]
pub struct DocGrid {
    /// 网格行距，twips。
    pub line_pitch: i32,
    /// 是否真的吸附。`w:type` 为 lines / linesAndChars / snapToChars 时吸附，
    /// 为 default 或缺省时不吸附 —— 后两种很常见，一律吸附会把行距撑大一倍。
    pub snaps: bool,
}

/// `w:sectPr`，页面几何。单位 twips。
#[derive(Debug, Clone, Copy)]
pub struct SectPr {
    pub page_w: i32,
    pub page_h: i32,
    pub margin_top: i32,
    pub margin_bottom: i32,
    pub margin_left: i32,
    pub margin_right: i32,
    pub doc_grid: Option<DocGrid>,
}

impl Default for SectPr {
    fn default() -> Self {
        // A4 纵向 + Word 的默认页边距。
        Self {
            page_w: 11906,
            page_h: 16838,
            margin_top: 1440,
            margin_bottom: 1440,
            margin_left: 1800,
            margin_right: 1800,
            doc_grid: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Style {
    pub id: String,
    pub based_on: Option<String>,
    pub ppr: PPr,
    pub rpr: RPr,
    pub is_default: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Styles {
    pub doc_default_ppr: PPr,
    pub doc_default_rpr: RPr,
    pub paragraph: std::collections::HashMap<String, Style>,
    pub character: std::collections::HashMap<String, Style>,
    pub default_paragraph_style: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RawDocument {
    pub blocks: Vec<RawBlock>,
    pub section: SectPr,
    pub styles: Styles,
}
