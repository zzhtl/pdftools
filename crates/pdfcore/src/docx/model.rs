//! OOXML 的忠实映射。
//!
//! 这一层**不做样式层叠、不做单位换算**，所有值保持 OOXML 原始形态
//! （twips、半磅、1/100 字符、240 分之一行）。
//! 这样「属性有没有解析到」和「层叠有没有算对」是两个独立的测试面。
//!
//! 正文、页眉页脚、表格单元格、文本框里装的都是同一种东西：一串块（段落与表格）。
//! 它们共用一个解析器（`parse::story`），所以这里也只有一种 [`Story`]。

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
    /// `both`：两端对齐，段落最后一行不拉开。
    Both,
    /// `distribute`：分散对齐，最后一行也拉开。
    Distribute,
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
    pub underline: Option<Underline>,
    pub strike: Option<bool>,
    /// `w:dstrike`：双删除线。
    pub double_strike: Option<bool>,
    /// `w:highlight`：突出显示。`Some(None)` 是明确写了 none。
    pub highlight: Option<Option<[u8; 3]>>,
    /// `w:shd/@w:fill`：底纹。`Some(None)` 是明确写了 auto / 没有填充。
    pub shading: Option<Option<[u8; 3]>>,
    /// `w:sz`，单位是**半磅**。
    pub size_half_pt: Option<u32>,
    pub color: Option<[u8; 3]>,
    /// 西文字体：`w:rFonts` 的 `w:asciiTheme` / `w:ascii`，都没写时取 `w:hAnsiTheme` /
    /// `w:hAnsi`。同一个元素里主题字体优先。
    pub font_ascii: Option<FontRef>,
    /// 中日韩字体：`w:eastAsiaTheme` / `w:eastAsia`。一个 run 需要两个字体，
    /// 少了哪个都会让身份证号或者汉字其中之一显示成错的样子。
    pub font_east_asia: Option<FontRef>,
    /// 重写前的读法：只认字体名（`w:ascii`，缺省 `w:hAnsi`），逐个属性覆盖。
    /// 只给 [`Theme::Ignored`](crate::docx::layout::Theme::Ignored) 用。
    pub legacy_font_ascii: Option<String>,
    pub legacy_font_east_asia: Option<String>,
    /// `w:rFonts/@w:hint` 是不是 `eastAsia`：归属不明的字符（引号、破折号、①……）
    /// 用东亚字体。
    pub hint_east_asia: Option<bool>,
    /// `w:spacing`：字符间距，twips。每个字后面加（负数是紧缩）。
    pub spacing: Option<i32>,
    /// `w:vanish`：隐藏文字。
    pub vanish: Option<bool>,
    /// `w:vertAlign`：上标、下标。
    pub vert_align: Option<VertAlign>,
    /// `w:position`：升降，半磅，正数往上。
    pub position: Option<i32>,
    pub caps: Option<bool>,
    pub small_caps: Option<bool>,
    /// `w:kern`：字号达到这么多（半磅）才做字距调整；0 是不调整。
    pub kern: Option<u32>,
}

/// `w:rFonts` 里一个字体槽写的是什么。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FontRef {
    Name(String),
    /// 引用主题字体（`w:asciiTheme="minorHAnsi"` 之类），要等拿到主题部件才知道是哪个字体。
    Theme(ThemeFont),
}

/// 主题字体引用：主题里的哪一套（标题 major / 正文 minor）、哪一种文字。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeFont {
    pub major: bool,
    pub script: ThemeScript,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeScript {
    /// `majorAscii`、`majorHAnsi`、`minorAscii`、`minorHAnsi` → `a:latin`。
    Latin,
    /// `majorEastAsia`、`minorEastAsia` → `a:ea` 或按语言找的文种字体。
    EastAsia,
    /// `majorBidi`、`minorBidi` → `a:cs`。本版本不排复杂文种，读到了也不用。
    ComplexScript,
}

impl ThemeFont {
    /// `ST_Theme` 的取值。
    pub fn parse(v: &str) -> Option<Self> {
        let (major, rest) = if let Some(r) = v.strip_prefix("major") {
            (true, r)
        } else {
            (false, v.strip_prefix("minor")?)
        };
        let script = match rest {
            "Ascii" | "HAnsi" => ThemeScript::Latin,
            "EastAsia" => ThemeScript::EastAsia,
            "Bidi" => ThemeScript::ComplexScript,
            _ => return None,
        };
        Some(Self { major, script })
    }
}

/// 主题部件（`word/theme/theme1.xml`）里的字体方案。
#[derive(Debug, Clone, Default)]
pub struct Theme {
    pub major: ThemeFonts,
    pub minor: ThemeFonts,
}

/// `a:majorFont` / `a:minorFont`。空的 typeface 按没写处理。
#[derive(Debug, Clone, Default)]
pub struct ThemeFonts {
    pub latin: Option<String>,
    pub east_asia: Option<String>,
    pub complex_script: Option<String>,
    /// `a:font script="Hans" typeface="宋体"`：文种代码 → 字体。
    pub by_script: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VertAlign {
    #[default]
    Baseline,
    Superscript,
    Subscript,
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
            double_strike,
            highlight,
            shading,
            size_half_pt,
            color,
            font_ascii,
            font_east_asia,
            legacy_font_ascii,
            legacy_font_east_asia,
            hint_east_asia,
            spacing,
            vanish,
            vert_align,
            position,
            caps,
            small_caps,
            kern
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
    /// `w:overflowPunct`：行尾标点可以伸出右边距。缺省为 true。
    pub overflow_punct: Option<bool>,
    /// `w:tabs`：自定义制表位。样式里的与段落上的要合并，见 [`merge_tabs`]。
    pub tabs: Vec<TabDef>,
    /// `w:keepNext`：与下一段在同一页。
    pub keep_next: Option<bool>,
    /// `w:keepLines`：段中不分页。
    pub keep_lines: Option<bool>,
    /// `w:widowControl`：孤行控制。哪一级都没写时是开着的 —— WPS 要关掉它就得在
    /// Normal 样式里明确写 `w:val="0"`，LibreOffice 也按开着排。
    pub widow_control: Option<bool>,
    /// `w:contextualSpacing`：与同一样式的相邻段落之间不加段距。
    pub contextual_spacing: Option<bool>,
    /// `w:pBdr`：段落边框。
    pub borders: ParaBorders,
    /// `w:shd`：段落底纹。`Some(None)` 是明确写了没有底纹。
    pub shading: Option<Option<[u8; 3]>>,
    /// 本段挂了自动编号（写了 `w:numPr`，不管指向哪里）。重写前只认这一点。
    pub numbering: bool,
    /// `w:numPr/w:numId`：用哪个编号定义。0 是明确取消样式带来的编号。
    pub num_id: Option<i32>,
    /// `w:numPr/w:ilvl`：第几级（0 起）。
    pub num_ilvl: Option<i32>,
    /// `w:pPr/w:rPr`：段落标记自身的格式。它参与 run 的层叠，优先级低于 run 上的直接格式。
    pub mark_rpr: RPr,
}

impl PPr {
    /// 按规范层叠：与 [`merge`](Self::merge) 相同，只是首行缩进与悬挂缩进互斥 ——
    /// 上层写了其中一个，就取代下层写的另一个。同一个元素里两个都写时照旧悬挂优先。
    pub fn cascade(&mut self, other: &PPr) {
        let o = &other.indent;
        let first = o.first_line_twips.is_some() || o.first_line_chars.is_some();
        let hanging = o.hanging_twips.is_some() || o.hanging_chars.is_some();
        if first && !hanging {
            self.indent.hanging_twips = None;
            self.indent.hanging_chars = None;
        }
        if hanging && !first {
            self.indent.first_line_twips = None;
            self.indent.first_line_chars = None;
        }
        self.merge(other);
    }

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
        if other.overflow_punct.is_some() {
            self.overflow_punct = other.overflow_punct;
        }
        merge_tabs(&mut self.tabs, &other.tabs);
        macro_rules! take {
            ($($f:ident),*) => { $( if other.$f.is_some() { self.$f = other.$f; } )* };
        }
        take!(keep_next, keep_lines, widow_control, contextual_spacing);
        self.borders.merge(&other.borders);
        if other.shading.is_some() {
            self.shading = other.shading;
        }
        self.numbering |= other.numbering;
        if other.num_id.is_some() {
            self.num_id = other.num_id;
        }
        if other.num_ilvl.is_some() {
            self.num_ilvl = other.num_ilvl;
        }
        self.mark_rpr.merge(&other.mark_rpr);
    }
}

/// `w:pBdr` 的各条边。样式链上逐条覆盖：写了 `w:val="nil"` 的边会盖掉样式里的边框。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ParaBorders {
    pub top: Option<Border>,
    pub left: Option<Border>,
    pub bottom: Option<Border>,
    pub right: Option<Border>,
    /// 相邻的同样边框的段落之间画的线。
    pub between: Option<Border>,
}

impl ParaBorders {
    fn merge(&mut self, other: &ParaBorders) {
        macro_rules! take {
            ($($f:ident),*) => { $( if other.$f.is_some() { self.$f = other.$f; } )* };
        }
        take!(top, left, bottom, right, between);
    }
}

/// 一条边框线。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Border {
    pub style: BorderStyle,
    /// `w:sz`：线宽，八分之一磅。双线时是每一条的宽度。
    pub size_eighths: i32,
    /// `w:space`：与文字的距离，磅。
    pub space_pt: i32,
    /// None 是 auto（跟文字一个颜色，按黑色画）。
    pub color: Option<[u8; 3]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorderStyle {
    /// `nil` / `none`：没有这条边。
    None,
    Single,
    Double,
    Dotted,
    Dashed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnderlineStyle {
    None,
    Single,
    /// 只划字、不划空格。这里按单线画。
    Words,
    Double,
    Thick,
    Dotted,
    DottedHeavy,
    Dash,
    DashedHeavy,
    DashLong,
    DashLongHeavy,
    DotDash,
    DashDotHeavy,
    DotDotDash,
    DashDotDotHeavy,
    /// 波浪线。这里按单线画。
    Wave,
    WavyHeavy,
    WavyDouble,
}

/// `w:u`。颜色没写（或写 auto）时跟文字一个颜色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Underline {
    pub style: UnderlineStyle,
    pub color: Option<[u8; 3]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabAlign {
    Left,
    Center,
    Right,
    Decimal,
    /// 在该位置画一条竖线，不是制表位。
    Bar,
    /// 清掉样式里同一位置的制表位。
    Clear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabLeader {
    None,
    Dot,
    Hyphen,
    Underscore,
    MiddleDot,
    Heavy,
}

/// `w:tabs/w:tab`。位置是 twips，从正文区左缘量起。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabDef {
    pub align: TabAlign,
    pub leader: TabLeader,
    pub pos: i32,
}

/// 样式链上的制表位逐级合并：同一位置的后者覆盖前者，`clear` 删掉该位置的。
pub fn merge_tabs(base: &mut Vec<TabDef>, over: &[TabDef]) {
    for t in over {
        base.retain(|b| b.pos != t.pos);
        if t.align != TabAlign::Clear {
            base.push(*t);
        }
    }
    base.sort_by_key(|t| t.pos);
}

/// 一串块级内容：正文、页眉页脚、单元格、文本框共用。
pub type Story = Vec<Block>;

// 块几乎都是段落，表格少见：给段落装箱省不下多少内存，反倒每段多一次分配。
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum Block {
    Para(Para),
    Table(Table),
}

#[derive(Debug, Clone, Default)]
pub struct Para {
    pub ppr: PPr,
    pub runs: Vec<Run>,
    /// 段落里的 `w:pPr/w:sectPr`：本段是一节的最后一段。
    pub section: Option<SectPr>,
}

#[derive(Debug, Clone, Default)]
pub struct Run {
    pub rpr: RPr,
    pub items: Vec<RunItem>,
    /// 这个 run 在 `w:hyperlink` 里面。
    pub link: Option<LinkRef>,
}

/// `w:hyperlink` 指向哪里。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkRef {
    /// `r:id`：外部链接的关系 id。
    Rel(String),
    /// `w:anchor`：文档内的书签。
    Anchor(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum RunItem {
    Text(String),
    Tab,
    /// `w:br`，以及等同于换行的 `w:cr`。
    Break(BreakKind),
    /// `w:noBreakHyphen`。
    NoBreakHyphen,
    /// 图片、形状、嵌入对象（`w:drawing` / `w:pict` / `w:object`）。目前只取替代文字。
    Drawing {
        alt: Option<String>,
    },
    /// `w:sym`：用指定字体画的一个符号（Wingdings 的勾选框之类）。
    Sym {
        font: Option<String>,
        code: u32,
    },
    /// `w:fldChar`：域的开始、代码与结果的分隔、结束。`w:fldSimple` 也展开成这样。
    FieldChar(FieldChar),
    /// `w:instrText`：域代码（`PAGE \* MERGEFORMAT`）。
    FieldCode(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldChar {
    Begin,
    Separate,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakKind {
    Line,
    Page,
    Column,
}

#[derive(Debug, Clone, Default)]
pub struct Table {
    pub props: TblPr,
    /// `w:tblGrid/w:gridCol`：各列的宽度，twips。
    pub grid: Vec<i32>,
    pub rows: Vec<Row>,
}

#[derive(Debug, Clone, Default)]
pub struct Row {
    pub props: TrPr,
    /// `w:tblPrEx`：这一行对表格属性的例外（边框、单元格边距）。
    pub exceptions: TblPr,
    pub cells: Vec<Cell>,
}

#[derive(Debug, Clone, Default)]
pub struct Cell {
    pub props: TcPr,
    pub content: Story,
}

/// 表格、单元格的宽度（`w:tblW`、`w:tcW`）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Width {
    Auto,
    Twips(i32),
    /// 占版心（或所在单元格）宽度的百分比。
    Percent(f32),
}

/// 表格或单元格的四边（与表格内部的横线、竖线）。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TableBorders {
    pub top: Option<Border>,
    pub left: Option<Border>,
    pub bottom: Option<Border>,
    pub right: Option<Border>,
    pub inside_h: Option<Border>,
    pub inside_v: Option<Border>,
}

impl TableBorders {
    pub fn merge(&mut self, other: &TableBorders) {
        macro_rules! take {
            ($($f:ident),*) => { $( if other.$f.is_some() { self.$f = other.$f; } )* };
        }
        take!(top, left, bottom, right, inside_h, inside_v);
    }
}

/// 单元格边距（`w:tblCellMar`、`w:tcMar`），twips。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CellMargins {
    pub top: Option<i32>,
    pub left: Option<i32>,
    pub bottom: Option<i32>,
    pub right: Option<i32>,
}

impl CellMargins {
    pub fn merge(&mut self, other: &CellMargins) {
        macro_rules! take {
            ($($f:ident),*) => { $( if other.$f.is_some() { self.$f = other.$f; } )* };
        }
        take!(top, left, bottom, right);
    }
}

/// `w:tblPr`（`w:tblPrEx` 也用它，只是只有其中几项）。
#[derive(Debug, Clone, Default)]
pub struct TblPr {
    pub style_id: Option<String>,
    pub width: Option<Width>,
    /// `w:jc`：表格在版心里的对齐。
    pub align: Option<Align>,
    /// `w:tblInd`：表格往里缩进多少，twips。从哪里量随兼容模式而变，见 `ir::Table::indent`。
    pub indent: Option<i32>,
    pub borders: TableBorders,
    pub cell_margins: CellMargins,
    /// `w:tblLayout w:type="fixed"`：列宽不随内容调整。
    pub fixed_layout: bool,
    pub shading: Option<Option<[u8; 3]>>,
    /// `w:tblLook`：表格样式里的哪些条件格式生效。
    pub look: Option<TblLook>,
    /// `w:tblStyleRowBandSize` / `w:tblStyleColBandSize`：隔行、隔列底纹几行（列）一换。
    pub row_band: Option<u32>,
    pub col_band: Option<u32>,
}

impl TblPr {
    /// 表格样式沿 basedOn 合并、直接格式盖样式时用：写了的项覆盖。
    pub fn merge(&mut self, other: &TblPr) {
        macro_rules! take {
            ($($f:ident),*) => { $( if other.$f.is_some() { self.$f = other.$f.clone(); } )* };
        }
        take!(style_id, width, align, indent, shading, look, row_band, col_band);
        self.borders.merge(&other.borders);
        self.cell_margins.merge(&other.cell_margins);
        self.fixed_layout |= other.fixed_layout;
    }
}

/// `w:tblLook`。没写的项是「不」。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TblLook {
    pub first_row: bool,
    pub last_row: bool,
    pub first_col: bool,
    pub last_col: bool,
    /// 不要隔行底纹。
    pub no_h_band: bool,
    /// 不要隔列底纹。
    pub no_v_band: bool,
}

/// 表格样式（`w:style w:type="table"`）。
#[derive(Debug, Clone, Default)]
pub struct TableStyle {
    pub based_on: Option<String>,
    /// 表格里段落、文字的格式，层叠时在 docDefaults 之后、段落样式之前。
    pub ppr: PPr,
    pub rpr: RPr,
    pub tbl_pr: TblPr,
    pub tc_pr: TcPr,
    /// `w:tblStylePr`：首行、末行、隔行……各自的格式。
    pub conditions: Vec<(TableRegion, TableCondition)>,
}

/// 表格样式里一类区域的格式（`w:tblStylePr`）。
#[derive(Debug, Clone, Default)]
pub struct TableCondition {
    pub ppr: PPr,
    pub rpr: RPr,
    pub tbl_pr: TblPr,
    pub tc_pr: TcPr,
}

/// 条件格式作用的区域。排在后面的优先（ECMA-376 §17.7.6）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TableRegion {
    WholeTable,
    Band1Vert,
    Band2Vert,
    Band1Horz,
    Band2Horz,
    FirstCol,
    LastCol,
    FirstRow,
    LastRow,
    NeCell,
    NwCell,
    SeCell,
    SwCell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeightRule {
    Auto,
    AtLeast,
    Exact,
}

/// `w:trPr`。
#[derive(Debug, Clone, Default)]
pub struct TrPr {
    /// `w:trHeight`：行高（twips）与它的含义。
    pub height: Option<(i32, HeightRule)>,
    /// `w:cantSplit`：这一行不跨页拆开。
    pub cant_split: bool,
    /// `w:tblHeader`：标题行，跨页时在新的一页重复。
    pub header: bool,
    /// `w:gridBefore` / `w:gridAfter`：行首、行尾空出几列。
    pub grid_before: u32,
    pub grid_after: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VMerge {
    /// 纵向合并的第一格。
    Restart,
    /// 并入上面那一格。
    Continue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VAlign {
    Top,
    Center,
    Bottom,
}

/// `w:tcPr`。
#[derive(Debug, Clone, Default)]
pub struct TcPr {
    pub width: Option<Width>,
    /// `w:gridSpan`：横跨几列。没写是 1。
    pub grid_span: Option<u32>,
    pub v_merge: Option<VMerge>,
    pub borders: TableBorders,
    pub shading: Option<Option<[u8; 3]>>,
    pub margins: CellMargins,
    pub v_align: Option<VAlign>,
}

impl TcPr {
    /// 表格样式里的单元格格式（沿 basedOn、各条件格式）逐层盖上去：写了的项覆盖。
    /// 宽度、合并这类只属于单元格自己的不参与。
    pub fn merge_style(&mut self, other: &TcPr) {
        if other.shading.is_some() {
            self.shading = other.shading;
        }
        if other.v_align.is_some() {
            self.v_align = other.v_align;
        }
        self.borders.merge(&other.borders);
        self.margins.merge(&other.margins);
    }
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
    /// 还有字符网格（`w:type` 为 linesAndChars / snapToChars）。
    pub chars: bool,
    /// `w:charSpace`：字符网格的格宽比 Normal 样式的字号多出多少，单位 1/4096 磅。
    pub char_space: Option<i32>,
}

/// `w:sectPr`：一节的页面设置。单位 twips。
#[derive(Debug, Clone)]
pub struct SectPr {
    pub page_w: i32,
    pub page_h: i32,
    pub margin_top: i32,
    pub margin_bottom: i32,
    pub margin_left: i32,
    pub margin_right: i32,
    pub doc_grid: Option<DocGrid>,
    /// 本节引用了页眉或页脚。
    pub has_header_footer: bool,
    /// `w:pgMar/@w:header`：页眉顶端离纸张上边的距离。
    pub header_dist: i32,
    /// `w:pgMar/@w:footer`：页脚底端离纸张下边的距离。
    pub footer_dist: i32,
    /// `w:type`：本节从哪里开始。
    pub start: SectionStart,
    /// `w:titlePg`：本节首页用单独的页眉页脚。
    pub title_page: bool,
    /// `w:pgNumType/@w:start`：本节的页码从几开始。没写就接着上一节。
    pub page_number_start: Option<i32>,
    /// `w:pgNumType/@w:fmt`：页码的数字格式，取值与编号格式相同。
    pub page_number_format: Option<String>,
    /// `w:headerReference`：各类页眉的关系 id。
    pub headers: HeaderRefs,
    pub footers: HeaderRefs,
}

/// 一节从哪里开始（`w:sectPr/w:type`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SectionStart {
    /// 另起一页。缺省。
    #[default]
    NextPage,
    /// 接着上一节排，不换页。
    Continuous,
    /// 从下一个偶数页开始，需要时空出一页。
    EvenPage,
    OddPage,
    /// 下一栏。本版本不分栏，按另起一页处理。
    NextColumn,
}

/// 首页、偶数页、其余页各用哪个页眉（或页脚）部件：关系 id。
#[derive(Debug, Clone, Default)]
pub struct HeaderRefs {
    pub default: Option<String>,
    pub first: Option<String>,
    pub even: Option<String>,
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
            has_header_footer: false,
            header_dist: 720,
            footer_dist: 720,
            start: SectionStart::NextPage,
            title_page: false,
            page_number_start: None,
            page_number_format: None,
            headers: HeaderRefs::default(),
            footers: HeaderRefs::default(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Style {
    pub id: String,
    pub based_on: Option<String>,
    pub ppr: PPr,
    pub rpr: RPr,
}

#[derive(Debug, Clone, Default)]
pub struct Styles {
    pub doc_default_ppr: PPr,
    pub doc_default_rpr: RPr,
    pub paragraph: HashMap<String, Style>,
    pub character: HashMap<String, Style>,
    /// 编号样式（`w:type="numbering"`）。只用来解 `w:numStyleLink`：编号定义写在
    /// 样式指向的那个 `w:num` 里。
    pub numbering: HashMap<String, Style>,
    /// 标了 `w:default="1"` 的段落样式，通常是 Normal。
    pub default_paragraph_style: Option<String>,
    pub table: HashMap<String, TableStyle>,
    /// 标了 `w:default="1"` 的表格样式（Normal Table）：没写 `w:tblStyle` 的表格用它。
    pub default_table_style: Option<String>,
}

/// `numbering.xml`：编号定义。
#[derive(Debug, Clone, Default)]
pub struct Numbering {
    pub abstracts: HashMap<i32, AbstractNum>,
    /// `w:num`：段落通过 numId 引用它，它再指向一个 abstractNum。
    pub nums: HashMap<i32, Num>,
}

#[derive(Debug, Clone, Default)]
pub struct AbstractNum {
    /// 第 0–8 级。
    pub levels: HashMap<u8, Level>,
    /// `w:numStyleLink`：各级定义不在这里，在这个编号样式所用的编号定义里。
    pub num_style_link: Option<String>,
}

/// `w:lvl`：一级编号的样子。
#[derive(Debug, Clone, Default)]
pub struct Level {
    /// `w:start`。没写时按规范是 0。
    pub start: Option<i32>,
    /// `w:numFmt`：decimal、chineseCounting、bullet……
    pub format: Option<String>,
    /// `w:lvlText`：`%1.%2` 这样的模板；项目符号时就是符号本身。
    pub text: Option<String>,
    /// `w:lvlRestart`：用到第几级（1 起）或更高的级别时本级重新计数；0 是永不重新计数。
    /// 没写时是上一级。
    pub restart: Option<i32>,
    /// `w:isLgl`：各级一律写成阿拉伯数字。
    pub legal: bool,
    /// `w:suff`：编号后面跟什么。
    pub suffix: Option<NumSuffix>,
    /// `w:lvlJc`：编号在首行起点处怎么对齐。
    pub align: Option<Align>,
    /// `w:lvlPicBulletId`：图片项目符号。
    pub picture_bullet: bool,
    /// 这一级的缩进、制表位。
    pub ppr: PPr,
    /// 编号文字的格式。
    pub rpr: RPr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumSuffix {
    Tab,
    Space,
    Nothing,
}

#[derive(Debug, Clone, Default)]
pub struct Num {
    pub abstract_id: i32,
    /// `w:lvlOverride`，按级别。
    pub overrides: HashMap<u8, LevelOverride>,
}

#[derive(Debug, Clone, Default)]
pub struct LevelOverride {
    /// `w:startOverride`：第一次用到时从这个数重新起头。
    pub start: Option<i32>,
    /// 整级替换（`w:lvlOverride/w:lvl`）。
    pub level: Option<Level>,
}

impl Numbering {
    /// numId 所用的编号定义的第 `ilvl` 级：(abstractNumId, 级别)。
    /// 顺着 `w:numStyleLink` 找到真正写着各级的定义；`w:lvlOverride` 整级替换的优先。
    pub fn level<'a>(&'a self, styles: &Styles, num_id: i32, ilvl: u8) -> Option<(i32, &'a Level)> {
        let num = self.nums.get(&num_id)?;
        if let Some(level) = num.overrides.get(&ilvl).and_then(|o| o.level.as_ref()) {
            return Some((num.abstract_id, level));
        }
        let mut id = num.abstract_id;
        // numStyleLink 可能一层套一层，也可能成环：最多跟几次。
        for _ in 0..4 {
            let abs = self.abstracts.get(&id)?;
            let Some(link) = &abs.num_style_link else {
                return abs.levels.get(&ilvl).map(|l| (id, l));
            };
            let linked = styles.numbering.get(link)?.ppr.num_id?;
            id = self.nums.get(&linked)?.abstract_id;
        }
        None
    }
}

/// `settings.xml` 里影响排版的开关。
#[derive(Debug, Clone, Default)]
pub struct Settings {
    /// `w:compat/w:doNotUseHTMLParagraphAutoSpacing`。
    pub no_html_paragraph_spacing: bool,
    /// `w:defaultTabStop`，twips。
    pub default_tab_stop: Option<i32>,
    /// `w:evenAndOddHeaders`：偶数页用单独的页眉页脚。
    pub even_and_odd_headers: bool,
    /// `w:themeFontLang/@w:eastAsia`（`zh-CN` 之类）：东亚主题字体取主题里哪个文种的字体。
    pub theme_font_lang_east_asia: Option<String>,
    /// `w:compatSetting[@w:name="compatibilityMode"]`：按哪一版 Word 排版（14 是
    /// Word 2010，15 是 Word 2013 及以后）。
    pub compat_mode: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct Document {
    pub body: Story,
    /// `w:body` 末尾的 `w:sectPr`：最后一节（只有一节时就是全文）的页面设置。
    pub section: SectPr,
    pub styles: Styles,
    pub settings: Settings,
    pub theme: Theme,
    pub numbering: Numbering,
    /// 页眉页脚：关系 id → 内容。
    pub header_footer: HashMap<String, Story>,
    /// 外部链接：关系 id → 网址。解析 document.xml 时不知道关系表，由调用方填上。
    pub hyperlinks: HashMap<String, String>,
}
