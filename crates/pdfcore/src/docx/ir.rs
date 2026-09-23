//! 中间表示：样式已展开、单位已统一成**点**、字体仍是文档里写的名字。
//!
//! 从这里往后不再出现 twips、半磅、1/100 字符这些 OOXML 单位，排版代码只跟点打交道；
//! 字体要等排版时才解析成本机真实的字体，所以这一层不依赖字体也能测。
//!
//! 一个段落的文字是**一个字符串加若干 span**：断行（unicode-linebreak）与整形
//! （rustybuzz）都作用在同一个字符串上，字节偏移是唯一的坐标系。
//! 不是文字的东西也编进这个字符串：
//!
//! | 来源 | 字符 |
//! | --- | --- |
//! | `w:tab` | `\t` |
//! | `w:br`、`w:cr` | U+2028 |
//! | `w:br w:type="page"` | U+000C |
//! | `w:br w:type="column"` | U+000B |
//! | `w:noBreakHyphen` | U+2011 |

use std::ops::Range;

use super::model::{
    self, BreakKind, FieldChar, FontRef, LineRule, NumSuffix, PPr, RPr, RunItem, ThemeScript,
};
pub use super::model::{BorderStyle, SectionStart, TabAlign, TabLeader, UnderlineStyle, VertAlign};
use super::numbering::Lists;
use super::resolve::{Resolver, TableLayer};

pub const LINE_BREAK: char = '\u{2028}';
pub const PAGE_BREAK: char = '\u{000C}';
pub const COLUMN_BREAK: char = '\u{000B}';
/// 行内对象在段落文字里的替身：第 k 个是 [`Paragraph::objects`] 的第 k 项。
pub const OBJECT: char = '\u{FFFC}';

/// 行内对象（`wp:inline` 的图片）：底边在基线上，像一个很大的字。
#[derive(Debug, Clone)]
pub struct InlineObject {
    /// 显示大小（点）。
    pub width: f32,
    pub height: f32,
    pub content: ObjectContent,
}

#[derive(Debug, Clone)]
pub enum ObjectContent {
    /// 包里的图片：部件路径，左、上、右、下各裁掉的比例（0–1）。
    Image { part: String, crop: [f32; 4] },
    /// 矩形、直线、文本框。
    Shape(Box<ShapeObject>),
    /// 画不出来的（组合、图表、找不到的图）：按大小画一个灰框，版面不乱。
    Missing { alt: Option<String> },
}

/// 形状：填充、轮廓，框里的字与正文一样排。
#[derive(Debug, Clone)]
pub struct ShapeObject {
    pub kind: ShapeKind,
    pub fill: Option<[u8; 3]>,
    /// 轮廓（直线就是线本身）：线宽（点）与颜色。
    pub line: Option<(f32, [u8; 3])>,
    /// 框里的字（文本框）。
    pub text: Vec<Block>,
    /// 字离框的距离：上、左、下、右（点），与单元格边距同序。
    pub insets: [f32; 4],
    /// 字在框里竖直方向怎么放。
    pub text_align: model::VAlign,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeKind {
    Rect,
    /// 外框的一条对角线：`rising` 是从左下到右上，否则从左上到右下。
    Line {
        rising: bool,
    },
    /// 画不了的形状（椭圆、箭头……）：只排框里的字。
    TextOnly,
}

/// 浮动的图（`wp:anchor`）：不占行里的位置，放段落时按锚点定位。
#[derive(Debug, Clone)]
pub struct FloatObject {
    pub width: f32,
    pub height: f32,
    pub content: ObjectContent,
    pub h: Placement,
    pub v: Placement,
    pub wrap: Wrap,
    /// 画在文字下面（`@behindDoc`）；否则画在上面。
    pub behind: bool,
    /// 上、下、左、右与文字的距离（点）。
    pub dist: [f32; 4],
}

/// 一个方向上的位置：相对什么，偏移多少或者怎么对齐。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub from: RelativeFrom,
    pub at: At,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelativeFrom {
    Page,
    Margin,
    Column,
    Paragraph,
    Line,
    /// 横向相对锚点所在的字。按所在的栏近似。
    Character,
    LeftMargin,
    RightMargin,
    TopMargin,
    BottomMargin,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum At {
    /// 从参照区的起点（左边、上边）量的偏移（点），正数往右、往下。
    Offset(f32),
    /// 靠参照区的起点、居中、靠终点（`wp:align`）。
    Start,
    Center,
    End,
}

/// 浮动的图怎么让文字。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrap {
    /// 不让文字（`wp:wrapNone`）。
    None,
    /// 上下型：图所在的那一段横条上不排字，碰到的行挪到图下面。
    TopAndBottom,
}

/// EMU（DrawingML 的长度单位）换成点。
fn emu(v: i64) -> f32 {
    v as f32 / 12_700.0
}

/// 小型大写：小写字母画成这么大的大写字母。LibreOffice 实测 80%。
const SMALL_CAPS_SCALE: f32 = 0.8;

/// twips → 点。1 点 = 20 twips。
fn tw(v: i32) -> f32 {
    v as f32 / 20.0
}

/// 半磅 → 点。
fn half_pt(v: u32) -> f32 {
    v as f32 / 2.0
}

/// 行网格。`pitch_pt` 是网格行距（点）。
#[derive(Debug, Clone, Copy)]
pub struct Grid {
    pub pitch_pt: f32,
}

impl Grid {
    /// 把单倍行高向上吸附到网格整数倍。
    ///
    /// 这是中文排版行密度的决定性一步：12pt 宋体自然行高 17.4pt，
    /// 在 15.6pt 的网格上要占满 2 格 = 31.2pt。不做这步，整篇会挤掉近一半。
    pub fn snap(self, natural_pt: f32) -> f32 {
        if self.pitch_pt <= 0.0 {
            return natural_pt;
        }
        (natural_pt / self.pitch_pt).ceil().max(1.0) * self.pitch_pt
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageGeom {
    pub w_pt: f32,
    pub h_pt: f32,
    pub margin_top: f32,
    pub margin_bottom: f32,
    pub margin_left: f32,
    pub margin_right: f32,
    /// 页眉顶端离纸张上边、页脚底端离纸张下边的距离。
    pub header_dist: f32,
    pub footer_dist: f32,
}

impl PageGeom {
    pub fn content_width(&self) -> f32 {
        (self.w_pt - self.margin_left - self.margin_right).max(1.0)
    }
    pub fn content_height(&self) -> f32 {
        (self.h_pt - self.margin_top - self.margin_bottom).max(1.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Debug, Clone, Copy)]
pub enum LineSpacing {
    /// 行距倍数。`lineRule="auto"` 时 `w:line` 是 240 分之一行，312 → 1.3 倍。
    Multiple(f32),
    Exact(f32),
    AtLeast(f32),
}

/// 一段文字的格式，层叠已算完。
#[derive(Debug, Clone, PartialEq)]
pub struct RunStyle {
    pub size_pt: f32,
    pub bold: bool,
    pub italic: bool,
    pub underline: Option<Underline>,
    pub strike: bool,
    pub double_strike: bool,
    /// 文字背后的底色：突出显示，没有的话是底纹。
    pub background: Option<[u8; 3]>,
    pub color: [u8; 3],
    /// 西文字体家族名（来自 `w:rFonts/@w:ascii`，主题字体已落实成名字）。
    pub font_latin: Option<String>,
    /// 中日韩字体家族名（来自 `w:rFonts/@w:eastAsia`）。
    pub font_east_asia: Option<String>,
    /// `w:rFonts/@w:hint="eastAsia"`：归属不明的字符用东亚字体。
    pub hint_east_asia: bool,
    /// 每个字后面额外加的间距（点），负数是紧缩。
    pub char_spacing: f32,
    /// 上标、下标：画小一号并抬高或压低，具体多少要看字体，排版时再算。
    pub vert_align: VertAlign,
    /// 升降（点），正数往上。
    pub position_pt: f32,
    /// 超链接的网址。
    pub link: Option<String>,
    /// 页码类域的结果文字：排版时代入真实的数。
    pub field: Option<Field>,
    /// 做字距调整（`w:kern` 的阈值不超过字号）。
    pub kern: bool,
}

/// 页码类的域。同一个域的结果可能分在几个 span 里，靠 `id` 认出是同一个。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field {
    /// 段落里第几个域。
    pub id: u32,
    pub kind: FieldKind,
    /// 域代码里的 `\*` 格式开关，换算成编号格式的名字（`upperRoman` 之类）。
    pub format: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    /// 本页页码。
    Page,
    /// 总页数。
    NumPages,
    /// 本节页数。
    SectionPages,
}

/// 认页码类的域代码：`PAGE \* ROMAN`、`NUMPAGES`、`SECTIONPAGES`。
fn page_field(code: &str) -> Option<(FieldKind, Option<&'static str>)> {
    let words: Vec<&str> = code.split_whitespace().collect();
    let kind = match words.first()?.to_ascii_uppercase().as_str() {
        "PAGE" => FieldKind::Page,
        "NUMPAGES" => FieldKind::NumPages,
        "SECTIONPAGES" => FieldKind::SectionPages,
        _ => return None,
    };
    let format = words.windows(2).find(|w| w[0] == "\\*").and_then(|w| {
        Some(match w[1] {
            "Arabic" => "decimal",
            "ArabicDash" => "arabicDash",
            "roman" => "lowerRoman",
            "ROMAN" => "upperRoman",
            "alphabetic" => "lowerLetter",
            "ALPHABETIC" => "upperLetter",
            "CircleNum" => "decimalEnclosedCircle",
            "CHINESENUM1" | "CHINESENUM3" => "chineseCounting",
            "CHINESENUM2" => "chineseLegalSimplified",
            // MERGEFORMAT、CHARFORMAT 之类与数字写法无关。
            _ => return None,
        })
    });
    Some((kind, format))
}

/// 段落里正在读的域，可以嵌套。
#[derive(Default)]
struct OpenFields {
    stack: Vec<OpenField>,
    next_id: u32,
}

struct OpenField {
    id: u32,
    code: String,
    /// 已经过了分隔符，接下来是结果。
    result: bool,
    kind: Option<(FieldKind, Option<&'static str>)>,
}

impl OpenFields {
    /// 接下来的文字属于哪个页码域的结果。
    fn current(&self) -> Option<Field> {
        let top = self.stack.last().filter(|f| f.result)?;
        let (kind, format) = top.kind?;
        Some(Field {
            id: top.id,
            kind,
            format,
        })
    }

    fn begin(&mut self) {
        self.stack.push(OpenField {
            id: self.next_id,
            code: String::new(),
            result: false,
            kind: None,
        });
        self.next_id += 1;
    }

    fn code(&mut self, code: &str) {
        if let Some(top) = self.stack.last_mut().filter(|f| !f.result) {
            top.code.push_str(code);
        }
    }

    fn separate(&mut self) {
        if let Some(top) = self.stack.last_mut() {
            top.result = true;
            top.kind = page_field(&top.code);
        }
    }

    /// 结束一个域；它是页码类的域时返回它（没有分隔符也认）。
    fn end(&mut self) -> Option<Field> {
        let f = self.stack.pop()?;
        let (kind, format) = f.kind.or_else(|| page_field(&f.code))?;
        Some(Field {
            id: f.id,
            kind,
            format,
        })
    }
}

/// 下划线，颜色已经落实（没写时就是文字颜色）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Underline {
    pub style: UnderlineStyle,
    pub color: [u8; 3],
}

/// 段落文字里的一段同格式区间。span 首尾相接、不重叠、都不为空。
#[derive(Debug, Clone)]
pub struct Span {
    pub range: Range<usize>,
    pub style: RunStyle,
}

/// 一个制表位。`pos` 是从正文区左缘量起的点数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TabStop {
    pub pos: f32,
    pub align: TabAlign,
    pub leader: TabLeader,
}

#[derive(Debug, Clone)]
pub struct Paragraph {
    pub align: Align,
    pub indent_left: f32,
    pub indent_right: f32,
    /// 首行缩进。负值表示悬挂缩进。
    pub first_line: f32,
    pub space_before: f32,
    pub space_after: f32,
    pub line: LineSpacing,
    pub page_break_before: bool,
    /// 本段是否参与行网格吸附（`w:snapToGrid`，缺省 true）。
    pub snap_to_grid: bool,
    /// 中日韩文字与西文/数字之间是否自动加间距（`w:autoSpaceDE`/`DN`，缺省 true）。
    pub auto_space: bool,
    /// 行尾标点可以伸出右边距（`w:overflowPunct`，缺省 true）。
    pub overflow_punct: bool,
    /// 自定义制表位，按位置升序。竖线位不在其中。
    pub tabs: Vec<TabStop>,
    pub keep_next: bool,
    pub keep_lines: bool,
    pub widow_control: bool,
    pub contextual_spacing: bool,
    pub borders: Borders,
    /// 段落底纹的颜色。
    pub shading: Option<[u8; 3]>,
    /// 段落样式（没写 `w:pStyle` 时是默认段落样式）。判断「同一样式的相邻段落」用。
    pub style_id: Option<String>,
    /// 段首的编号（已经写在 `text` 开头）。
    pub number: Option<NumberLabel>,
    pub text: String,
    pub spans: Vec<Span>,
    /// 行内对象，按在文字里出现的顺序，见 [`OBJECT`]。
    pub objects: Vec<InlineObject>,
    /// 锚在这一段上的浮动对象。
    pub floats: Vec<FloatObject>,
    /// 段落标记（¶）的格式。空段落的行高由它决定。
    pub mark: RunStyle,
}

/// 写在段首的编号。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NumberLabel {
    /// 编号文字（不含后面的制表符、空格）的字节长度。
    pub len: usize,
    /// `w:lvlJc`：编号在首行起点处左对齐、居中还是右对齐。
    pub align: Align,
}

/// 段落边框的一条线，单位已换成点。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Border {
    /// 不会是 [`BorderStyle::None`]：没有的边不出现在 [`Borders`] 里。
    pub style: BorderStyle,
    /// 一条线的宽度。双线是两条这么宽的线，中间再隔一条线宽。
    pub width: f32,
    /// 与文字的距离。
    pub space: f32,
    pub color: [u8; 3],
}

impl Border {
    /// 这条边一共占多厚。
    pub fn thickness(&self) -> f32 {
        match self.style {
            BorderStyle::Double => self.width * 3.0,
            _ => self.width,
        }
    }
}

/// 段落边框（`w:pBdr`）。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Borders {
    pub top: Option<Border>,
    pub left: Option<Border>,
    pub bottom: Option<Border>,
    pub right: Option<Border>,
    /// 相邻的、边框相同的段落之间的线。
    pub between: Option<Border>,
}

/// 一条框线换成点。`nil`、`none` 或零宽都是「没有」。
fn border(b: model::Border) -> Option<Border> {
    (b.style != BorderStyle::None && b.size_eighths > 0).then(|| Border {
        style: b.style,
        width: b.size_eighths as f32 / 8.0,
        space: b.space_pt as f32,
        color: b.color.unwrap_or([0, 0, 0]),
    })
}

impl Borders {
    fn from_model(b: &model::ParaBorders) -> Self {
        let side = |b: Option<model::Border>| b.and_then(border);
        Self {
            top: side(b.top),
            left: side(b.left),
            bottom: side(b.bottom),
            right: side(b.right),
            between: side(b.between),
        }
    }
}

/// 本版本画不出来的内容。它是 IR 的一等公民，而不是一个被丢掉的分支 ——
/// 这样「诚实失败」就不是靠自觉，而是类型系统逼着排版层去处理。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaceholderKind {
    Table { rows: usize, cols: usize },
    Drawing { alt: Option<String> },
}

impl PlaceholderKind {
    pub fn label(&self) -> String {
        match self {
            Self::Table { rows, cols } => format!("表格（{rows} 行 × {cols} 列）"),
            Self::Drawing { .. } => "图片".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Placeholder {
    pub kind: PlaceholderKind,
    /// 能抽出来的文字。表格里的文字往往是文档里最重要的内容，
    /// 就算画不出表格也要把字留下。
    pub text: Vec<String>,
}

// 块几乎都是段落，占位块少见：给段落装箱省不下多少内存，反倒每段多一次分配。
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum Block {
    Para(Paragraph),
    Placeholder(Placeholder),
    Table(Table),
}

/// 表格。单位已换成点。
#[derive(Debug, Clone)]
pub struct Table {
    /// 各列的宽度（`w:tblGrid`）：相邻两条列边界（框线的中线）之间的距离。
    /// 0 是不知道宽度的列（没写网格，单元格也没写宽度），排版时平分版心剩下的宽度。
    pub columns: Vec<f32>,
    /// 左对齐时，第一条列边界离版心左边多远。由 `w:tblInd` 按兼容模式折算，
    /// 见排版 `rules` 模块「表格」一节。
    pub indent: f32,
    /// 表格在版心里的对齐（`w:jc`）。
    pub align: Align,
    pub rows: Vec<TableRow>,
}

#[derive(Debug, Clone)]
pub struct TableRow {
    /// 行高：最小值或固定值（都含本行的上框线）。
    pub height: Option<(f32, model::HeightRule)>,
    pub cant_split: bool,
    pub header: bool,
    pub cells: Vec<TableCell>,
}

#[derive(Debug, Clone)]
pub struct TableCell {
    /// 从第几列开始（`w:gridBefore` 已算进去），横跨几列。
    pub col: usize,
    pub span: usize,
    /// 纵向合并：本格往下占几行（含本行）。
    pub rows: usize,
    /// 纵向合并里接在上一格下面的格：内容、底纹、框线都归开头的那一格。
    pub continued: bool,
    /// 四边的框线，已按单元格在表格里的位置取好（外沿用表格的四边，内部用
    /// insideH / insideV），再叠上行例外与单元格自己的。
    pub borders: CellBorders,
    pub shading: Option<[u8; 3]>,
    /// 上、左、下、右边距。
    pub margins: [f32; 4],
    pub v_align: model::VAlign,
    pub blocks: Vec<Block>,
}

/// 单元格一条边的框线。`explicit`：单元格自己写的，与相邻单元格争这条边时优先。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Edge {
    pub border: Option<Border>,
    pub explicit: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CellBorders {
    pub top: Edge,
    pub left: Edge,
    pub bottom: Edge,
    pub right: Edge,
}

/// 一节：页面设置相同的一段正文。
#[derive(Debug, Clone)]
pub struct Section {
    pub page: PageGeom,
    /// 本节的行网格。None 表示没有网格或网格类型不吸附。
    pub grid: Option<Grid>,
    /// 字符网格的格宽（点）。None 是没有字符网格。
    pub char_pitch: Option<f32>,
    /// 本节从哪里开始。第一节总是从第一页开始。
    pub start: SectionStart,
    /// 本节的块在 [`Document::blocks`] 里的区间。
    pub blocks: Range<usize>,
    /// 本节首页用单独的页眉页脚。
    pub title_page: bool,
    /// 本节第一页的页码。None 是接着上一节。
    pub page_number_start: Option<i32>,
    /// 页码的数字格式（与编号格式同一套取值）。
    pub page_number_format: String,
    /// 本节的页眉、页脚（没写的已按规则从上一节沿用）。
    pub headers: HeaderSet,
    pub footers: HeaderSet,
}

/// 一节的首页、偶数页、其余页各用什么页眉（或页脚）。None 是没有。
#[derive(Debug, Clone, Default)]
pub struct HeaderSet {
    pub default: Option<Vec<Block>>,
    pub first: Option<Vec<Block>>,
    pub even: Option<Vec<Block>>,
}

impl Section {
    fn from_model(s: &model::SectPr, blocks: Range<usize>) -> Self {
        Self {
            page: PageGeom {
                w_pt: tw(s.page_w),
                h_pt: tw(s.page_h),
                margin_top: tw(s.margin_top),
                margin_bottom: tw(s.margin_bottom),
                margin_left: tw(s.margin_left),
                margin_right: tw(s.margin_right),
                header_dist: tw(s.header_dist),
                footer_dist: tw(s.footer_dist),
            },
            grid: s
                .doc_grid
                .filter(|g| g.snaps && g.line_pitch > 0)
                .map(|g| Grid {
                    pitch_pt: tw(g.line_pitch),
                }),
            char_pitch: None,
            start: s.start,
            blocks,
            title_page: s.title_page,
            page_number_start: s.page_number_start,
            page_number_format: s
                .page_number_format
                .clone()
                .unwrap_or_else(|| "decimal".into()),
            headers: HeaderSet::default(),
            footers: HeaderSet::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Document {
    /// 至少一节，按顺序首尾相接地覆盖全部块。
    pub sections: Vec<Section>,
    /// 偶数页用单独的页眉页脚（`w:evenAndOddHeaders`）。
    pub even_and_odd_headers: bool,
    /// 相邻两段的段后距与段前距取较大值而不是相加（HTML 的规矩）。
    /// 文档没有设置 `w:doNotUseHTMLParagraphAutoSpacing` 时为真，见排版 `rules` 模块「段距」一节。
    pub html_paragraph_spacing: bool,
    /// 默认制表位的间距（点）。`settings.xml` 没写时是 Word 的缺省 36pt。
    pub default_tab_stop: f32,
    /// 不认识、按阿拉伯数字输出的编号格式。
    pub num_format_fallbacks: Vec<String>,
    /// 画成灰框的对象（组合、图表、找不到的图片）有几个。
    pub missing_objects: usize,
    /// 画不准的形状（圆角、旋转、其他几何形状）有几个。
    pub approximated_shapes: usize,
    /// 脚注、尾注的引用有几处（本版本不排注释）。
    pub notes: usize,
    /// 设置了分栏的节有几个（本版本按单栏排）。
    pub multi_column_sections: usize,
    /// 文字绕着图走的环绕按上下型近似排了几个。
    pub approximated_wraps: usize,
    pub blocks: Vec<Block>,
}

/// docDefaults、样式、直接格式都没写 `w:sz` 时的字号，见排版 `rules` 模块「字号」一节。
pub const DEFAULT_SIZE_PT: f32 = 10.0;

pub fn build(doc: &model::Document) -> Document {
    let resolver = Resolver::new(&doc.styles, Some(&doc.numbering));
    let mut lists = Some(Lists::new(&doc.numbering, &doc.styles));
    let fonts = FontNames::new(doc);
    // 段落里的 `w:sectPr` 结束一节；最后一节的设置在 body 末尾。

    let ctx = Ctx {
        doc,
        resolver: &resolver,
        fonts: &fonts,
        missing: Default::default(),
        shapes: Default::default(),
        notes: Default::default(),
        approximated: Default::default(),
    };
    let mut blocks = Vec::with_capacity(doc.body.len());
    let mut ends: Vec<(&model::SectPr, Range<usize>)> = Vec::new();
    let mut start = 0;
    for block in &doc.body {
        match block {
            model::Block::Para(p) => {
                push_paragraph(&mut blocks, p, &ctx, lists.as_mut(), None);
                if let Some(sp) = &p.section {
                    ends.push((sp, start..blocks.len()));
                    start = blocks.len();
                }
            }
            model::Block::Table(t) => blocks.push(table(t, &ctx, &mut lists, false)),
        }
    }
    ends.push((&doc.section, start..blocks.len()));
    let multi_column_sections = ends.iter().filter(|(sp, _)| sp.columns > 1).count();

    // 页眉页脚：本节没写的那一类沿用上一节的。
    let (mut headers, mut footers) = (HeaderSet::default(), HeaderSet::default());
    // 字符网格的格宽以 Normal 样式的字号为基准。
    let normal_pt = resolver
        .mark(&resolver.paragraph(&PPr::default()))
        .size_half_pt
        .map(half_pt)
        .unwrap_or(DEFAULT_SIZE_PT);
    let mut sections = Vec::with_capacity(ends.len());
    for (sp, range) in ends {
        let mut section = Section::from_model(sp, range);
        section.char_pitch = sp
            .doc_grid
            .filter(|g| g.chars)
            .and_then(|g| g.char_space)
            .map(|cs| normal_pt + cs as f32 / 4096.0)
            .filter(|p| *p > 0.0);
        headers = header_set(&headers, &sp.headers, &ctx);
        footers = header_set(&footers, &sp.footers, &ctx);
        section.headers = headers.clone();
        section.footers = footers.clone();
        sections.push(section);
    }

    Document {
        sections,
        even_and_odd_headers: doc.settings.even_and_odd_headers,
        html_paragraph_spacing: !doc.settings.no_html_paragraph_spacing,
        default_tab_stop: doc.settings.default_tab_stop.map(tw).unwrap_or(36.0),
        num_format_fallbacks: lists
            .map(|l| l.fallbacks.into_iter().collect())
            .unwrap_or_default(),
        missing_objects: ctx.missing.get(),
        approximated_shapes: ctx.shapes.get(),
        notes: ctx.notes.get(),
        multi_column_sections,
        approximated_wraps: ctx.approximated.get(),
        blocks,
    }
}

/// 一节的页眉（或页脚）：写了的用本节的，没写的沿用上一节的。
fn header_set(prev: &HeaderSet, refs: &model::HeaderRefs, ctx: &Ctx) -> HeaderSet {
    let pick = |id: &Option<String>, prev: &Option<Vec<Block>>| match id {
        Some(id) => ctx
            .doc
            .header_footer
            .get(id)
            .map(|story| story_blocks(story, ctx)),
        None => prev.clone(),
    };
    HeaderSet {
        default: pick(&refs.default, &prev.default),
        first: pick(&refs.first, &prev.first),
        even: pick(&refs.even, &prev.even),
    }
}

/// 页眉页脚里的内容。编号不接正文的计数。
fn story_blocks(story: &model::Story, ctx: &Ctx) -> Vec<Block> {
    let mut out = Vec::new();
    for block in story {
        match block {
            model::Block::Para(p) => push_paragraph(&mut out, p, ctx, None, None),
            model::Block::Table(t) => out.push(table(t, ctx, &mut None, false)),
        }
    }
    out
}

/// 构建各段落都要用到的东西。
struct Ctx<'a> {
    doc: &'a model::Document,
    resolver: &'a Resolver<'a>,
    fonts: &'a FontNames<'a>,
    /// 画成灰框的对象有几个。
    missing: std::cell::Cell<usize>,
    /// 画不准的形状有几个。
    shapes: std::cell::Cell<usize>,
    /// 脚注、尾注的引用有几处。
    notes: std::cell::Cell<usize>,
    /// 按上下型近似排的环绕（四周型、紧密型、穿越型）有几个。
    approximated: std::cell::Cell<usize>,
}

/// `table`：段落在表格里时表格样式给的格式。
fn push_paragraph(
    out: &mut Vec<Block>,
    p: &model::Para,
    ctx: &Ctx,
    lists: Option<&mut Lists>,
    table: Option<&TableLayer>,
) {
    let Ctx {
        doc,
        resolver,
        fonts,
        ..
    } = *ctx;
    let ppr = resolver.paragraph_in(&p.ppr, table);
    let mut text = String::new();
    let mut spans: Vec<Span> = Vec::with_capacity(p.runs.len());
    let mut drawings = Vec::new();
    let mut objects = Vec::new();
    let mut floats = Vec::new();
    // 页码类域的结果文字标上记号，排版时代入真实的数。
    let mut fields = OpenFields::default();
    for run in &p.runs {
        let rpr = resolver.run_in(&ppr, &run.rpr, table);
        // 隐藏文字不显示，也不占位置。
        if rpr.vanish == Some(true) {
            continue;
        }
        let mut style = run_style(&rpr, fonts);
        // 只做外部链接；文档内的书签跳转还没做。
        style.link = match &run.link {
            Some(model::LinkRef::Rel(id)) => doc.hyperlinks.get(id).cloned(),
            _ => None,
        };
        style.field = fields.current();
        let run_has_text = run.items.iter().any(|i| matches!(i, RunItem::Text(_)));
        // 一个 run 先切成若干（文字, 格式）小块：符号、小型大写的小写字母要换格式。
        // 同格式的相邻小块合成一个 span；不同 run 之间不合并。
        let mut chunks: Vec<(String, RunStyle)> = Vec::new();
        let mut push = |s: String, st: &RunStyle| match chunks.last_mut() {
            Some((t, last)) if last == st => t.push_str(&s),
            _ => chunks.push((s, st.clone())),
        };
        let small = RunStyle {
            size_pt: style.size_pt * SMALL_CAPS_SCALE,
            ..style.clone()
        };
        for item in &run.items {
            match item {
                RunItem::Text(t) if rpr.small_caps == Some(true) => {
                    // 小写字母换成缩小的大写，其余照旧。
                    let mut rest = t.as_str();
                    while let Some(c) = rest.chars().next() {
                        let lower = c.is_lowercase();
                        let end = rest
                            .find(|x: char| x.is_lowercase() != lower)
                            .unwrap_or(rest.len());
                        let (seg, tail) = rest.split_at(end);
                        if lower {
                            push(seg.to_uppercase(), &small);
                        } else {
                            push(seg.to_string(), &style);
                        }
                        rest = tail;
                    }
                }
                RunItem::Text(t) if rpr.caps == Some(true) => push(t.to_uppercase(), &style),
                RunItem::Text(t) => push(t.clone(), &style),
                RunItem::Tab => push("\t".into(), &style),
                RunItem::Break(BreakKind::Line) => push(LINE_BREAK.into(), &style),
                RunItem::Break(BreakKind::Page) => push(PAGE_BREAK.into(), &style),
                RunItem::Break(BreakKind::Column) => push(COLUMN_BREAK.into(), &style),
                RunItem::NoBreakHyphen => push("\u{2011}".into(), &style),
                RunItem::Drawing(d) if d.inline => {
                    // 零大小的图看不见，也不占位置。
                    if let Some((cx, cy)) = shown_size(d) {
                        objects.push(InlineObject {
                            width: emu(cx),
                            height: emu(cy),
                            content: object_content(d, ctx),
                        });
                        push(OBJECT.to_string(), &style);
                    }
                }
                RunItem::Drawing(d) if d.anchor.is_some() => {
                    if let (Some(a), Some((cx, cy))) = (&d.anchor, shown_size(d)) {
                        let wrap = match a.wrap {
                            model::WrapKind::None => Wrap::None,
                            model::WrapKind::TopAndBottom => Wrap::TopAndBottom,
                            // 文字绕着图走的几种，本版本按上下型排。
                            _ => {
                                ctx.approximated.set(ctx.approximated.get() + 1);
                                Wrap::TopAndBottom
                            }
                        };
                        floats.push(FloatObject {
                            width: emu(cx),
                            height: emu(cy),
                            content: object_content(d, ctx),
                            h: placement(&a.h, true),
                            v: placement(&a.v, false),
                            wrap,
                            behind: a.behind,
                            dist: a.dist.map(emu),
                        });
                    }
                }
                RunItem::Drawing(d) => drawings.push(d.alt.clone()),
                RunItem::FieldChar(FieldChar::Begin) => fields.begin(),
                RunItem::FieldCode(code) => fields.code(code),
                RunItem::FieldChar(FieldChar::Separate) => fields.separate(),
                RunItem::FieldChar(FieldChar::End) => {
                    // 没有结果文字的页码域（有的生成器不写缓存值）：补一个占位的字，
                    // 排版时照样代入。
                    if let Some(f) = fields.end() {
                        let shown = run_has_text
                            || spans
                                .iter()
                                .any(|s| s.style.field.is_some_and(|x| x.id == f.id));
                        if !shown {
                            let marked = RunStyle {
                                field: Some(f),
                                ..style.clone()
                            };
                            push("1".into(), &marked);
                        }
                    }
                }
                RunItem::NoteReference => ctx.notes.set(ctx.notes.get() + 1),
                // 符号用它自己的字体。符号字体（Symbol、Wingdings）里的码位
                // 写成单字节时，实际在私用区 U+F0xx。
                RunItem::Sym { font, code } => {
                    let symbolic = font
                        .as_deref()
                        .is_some_and(crate::fonts::pua::is_symbol_font);
                    let code = if *code <= 0xFF && symbolic {
                        0xF000 + code
                    } else {
                        *code
                    };
                    let Some(c) = char::from_u32(code) else {
                        continue;
                    };
                    let sym = RunStyle {
                        font_latin: font.clone().or_else(|| style.font_latin.clone()),
                        font_east_asia: font.clone().or_else(|| style.font_east_asia.clone()),
                        ..style.clone()
                    };
                    push(c.to_string(), &sym);
                }
            }
        }
        // 没有文字的 run（只有格式、只有一张图）不成 span：它不占位置，
        // 也不该决定空段落的行高。
        for (s, st) in chunks {
            if s.is_empty() {
                continue;
            }
            let start = text.len();
            text.push_str(&s);
            spans.push(Span {
                range: start..text.len(),
                style: st,
            });
        }
    }

    // 首行缩进按「字符」算时，用的是段落标记的东亚字号。
    let mark_rpr = resolver.mark_in(&ppr, table);
    let char_size = mark_rpr
        .size_half_pt
        .map(half_pt)
        .or_else(|| spans.first().map(|s| s.style.size_pt))
        .unwrap_or(DEFAULT_SIZE_PT);

    // 编号写在段首，格式是段落标记的格式叠上编号级别的格式。
    let label = lists.and_then(|l| {
        let ilvl = u8::try_from(ppr.num_ilvl.unwrap_or(0)).ok()?;
        l.next(ppr.num_id?, ilvl)
    });
    let (text, spans, number) = match label {
        Some(label) if !label.text.is_empty() || label.suffix != NumSuffix::Nothing => {
            let mut rpr = mark_rpr.clone();
            rpr.merge(&label.rpr);
            let style = run_style(&rpr, fonts);
            let len = label.text.len();
            let mut prefix = label.text;
            match label.suffix {
                NumSuffix::Tab => prefix.push('\t'),
                NumSuffix::Space => prefix.push(' '),
                NumSuffix::Nothing => {}
            }
            let shift = prefix.len();
            let spans = std::iter::once(Span {
                range: 0..shift,
                style,
            })
            .chain(spans.into_iter().map(|s| Span {
                range: s.range.start + shift..s.range.end + shift,
                ..s
            }))
            .collect();
            let align = match label.align {
                model::Align::Center => Align::Center,
                model::Align::Right => Align::Right,
                _ => Align::Left,
            };
            (prefix + &text, spans, Some(NumberLabel { len, align }))
        }
        _ => (text, spans, None),
    };

    let mark = run_style(&mark_rpr, fonts);
    let mut para = paragraph(&ppr, text, spans, mark, char_size);
    para.objects = objects;
    para.floats = floats;
    para.number = number;
    para.style_id = ppr
        .style_id
        .clone()
        .or_else(|| doc.styles.default_paragraph_style.clone());
    out.push(Block::Para(para));
    for alt in drawings {
        out.push(Block::Placeholder(Placeholder {
            kind: PlaceholderKind::Drawing { alt },
            text: Vec::new(),
        }));
    }
}

/// 显示大小（EMU）。零大小的看不见；水平、竖直的直线除外。
fn shown_size(d: &model::Drawing) -> Option<(i64, i64)> {
    let line = d
        .shape
        .as_ref()
        .is_some_and(|s| s.geometry == model::Geometry::Line);
    d.extent
        .filter(|&(cx, cy)| cx >= 0 && cy >= 0 && ((cx > 0 && cy > 0) || (line && cx + cy > 0)))
}

/// 对象画什么：找得到的图片、形状，或者画不出来的（组合、图表、找不到的图）。
fn object_content(d: &model::Drawing, ctx: &Ctx) -> ObjectContent {
    if let Some((part, crop)) = d
        .picture
        .as_ref()
        .and_then(|p| Some((p.target.clone()?, p.crop)))
    {
        return ObjectContent::Image {
            part,
            crop: crop.map(|c| c as f32 / 100_000.0),
        };
    }
    match &d.shape {
        Some(shape) => ObjectContent::Shape(Box::new(shape_object(shape, ctx))),
        None => {
            ctx.missing.set(ctx.missing.get() + 1);
            ObjectContent::Missing { alt: d.alt.clone() }
        }
    }
}

/// 形状换成排版用的：圆角矩形按矩形画，旋转的按不旋转画，其他几何形状只排框里的字，
/// 这几种都记下来汇总成一条警告。
fn shape_object(s: &model::Shape, ctx: &Ctx) -> ShapeObject {
    use model::Geometry;
    // 转 180° 的矩形、直线看起来与不转一样。
    let rotated = s.rotation.rem_euclid(10_800_000) != 0;
    let kind = match s.geometry {
        Geometry::Rect | Geometry::RoundRect => ShapeKind::Rect,
        Geometry::Line => ShapeKind::Line {
            rising: s.flip[0] != s.flip[1],
        },
        Geometry::Other => ShapeKind::TextOnly,
    };
    if rotated || matches!(s.geometry, Geometry::RoundRect | Geometry::Other) {
        ctx.shapes.set(ctx.shapes.get() + 1);
    }
    let [left, top, right, bottom] = s.insets.map(emu);
    ShapeObject {
        kind,
        fill: s.fill.filter(|_| kind == ShapeKind::Rect),
        line: s
            .line
            .filter(|_| kind != ShapeKind::TextOnly)
            .map(|(w, color)| (emu(w), color)),
        text: s
            .text
            .as_ref()
            .map(|story| story_blocks(story, ctx))
            .unwrap_or_default(),
        insets: [top, left, bottom, right],
        text_align: s.text_align,
    }
}

/// `wp:positionH` / `wp:positionV` 换成排版用的位置。没写参照时横向相对栏、
/// 纵向相对段落（Word 的缺省）。
fn placement(p: &model::AnchorPos, horizontal: bool) -> Placement {
    let from = match p.from.as_deref() {
        Some("page") => RelativeFrom::Page,
        Some("margin") => RelativeFrom::Margin,
        Some("column") => RelativeFrom::Column,
        Some("paragraph") => RelativeFrom::Paragraph,
        Some("line") => RelativeFrom::Line,
        Some("character") => RelativeFrom::Character,
        Some("leftMargin" | "insideMargin") => RelativeFrom::LeftMargin,
        Some("rightMargin" | "outsideMargin") => RelativeFrom::RightMargin,
        Some("topMargin") => RelativeFrom::TopMargin,
        Some("bottomMargin") => RelativeFrom::BottomMargin,
        _ if horizontal => RelativeFrom::Column,
        _ => RelativeFrom::Paragraph,
    };
    let at = match p.align.as_deref() {
        Some("center") => At::Center,
        Some("right" | "bottom" | "outside") => At::End,
        Some(_) => At::Start,
        None => At::Offset(p.offset.map(emu).unwrap_or(0.0)),
    };
    Placement { from, at }
}

/// 把 `w:rFonts` 的字体槽落实成字体名：主题字体要查主题部件。
struct FontNames<'a> {
    theme: &'a model::Theme,
    /// 东亚主题字体取主题里哪个文种的字体（`Hans` 之类），来自 `w:themeFontLang`。
    east_asia_script: Option<&'static str>,
}

impl<'a> FontNames<'a> {
    fn new(doc: &'a model::Document) -> Self {
        Self {
            theme: &doc.theme,
            east_asia_script: doc
                .settings
                .theme_font_lang_east_asia
                .as_deref()
                .and_then(east_asia_script),
        }
    }

    fn latin(&self, rpr: &RPr) -> Option<String> {
        self.name(rpr.font_ascii.as_ref()?)
    }

    fn east_asia(&self, rpr: &RPr) -> Option<String> {
        self.name(rpr.font_east_asia.as_ref()?)
    }

    fn name(&self, font: &FontRef) -> Option<String> {
        let t = match font {
            FontRef::Name(n) => return Some(n.clone()),
            FontRef::Theme(t) => t,
        };
        let fonts = if t.major {
            &self.theme.major
        } else {
            &self.theme.minor
        };
        match t.script {
            ThemeScript::Latin => fonts.latin.clone(),
            ThemeScript::EastAsia => self
                .east_asia_script
                .and_then(|s| fonts.by_script.get(s))
                .or(fonts.east_asia.as_ref())
                .cloned(),
            ThemeScript::ComplexScript => fonts.complex_script.clone(),
        }
    }
}

/// `w:themeFontLang/@w:eastAsia` 的语言标记 → 主题里 `a:font/@script` 的文种代码。
fn east_asia_script(lang: &str) -> Option<&'static str> {
    let lang = lang.to_ascii_lowercase();
    let (primary, rest) = lang.split_once('-').unwrap_or((&lang, ""));
    match primary {
        "zh" if ["tw", "hk", "mo", "hant"]
            .iter()
            .any(|r| rest.starts_with(r)) =>
        {
            Some("Hant")
        }
        "zh" => Some("Hans"),
        "ja" => Some("Jpan"),
        "ko" => Some("Hang"),
        _ => None,
    }
}

fn run_style(rpr: &RPr, fonts: &FontNames) -> RunStyle {
    let color = rpr.color.unwrap_or([0, 0, 0]);
    RunStyle {
        size_pt: rpr.size_half_pt.map(half_pt).unwrap_or(DEFAULT_SIZE_PT),
        bold: rpr.bold.unwrap_or(false),
        italic: rpr.italic.unwrap_or(false),
        underline: rpr
            .underline
            .filter(|u| u.style != UnderlineStyle::None)
            .map(|u| Underline {
                style: u.style,
                color: u.color.unwrap_or(color),
            }),
        strike: rpr.strike.unwrap_or(false),
        double_strike: rpr.double_strike.unwrap_or(false),
        background: rpr.highlight.flatten().or(rpr.shading.flatten()),
        color,
        font_latin: fonts.latin(rpr),
        font_east_asia: fonts.east_asia(rpr),
        hint_east_asia: rpr.hint_east_asia.unwrap_or(false),
        char_spacing: rpr.spacing.map(tw).unwrap_or(0.0),
        vert_align: rpr.vert_align.unwrap_or_default(),
        position_pt: rpr.position.map(|v| v as f32 / 2.0).unwrap_or(0.0),
        link: None,
        field: None,
        kern: rpr
            .kern
            .is_some_and(|k| k > 0 && size_half_pt(rpr) >= k as f32),
    }
}

/// 字号，半磅。
fn size_half_pt(rpr: &RPr) -> f32 {
    rpr.size_half_pt
        .map(|v| v as f32)
        .unwrap_or(DEFAULT_SIZE_PT * 2.0)
}

fn paragraph(
    ppr: &PPr,
    text: String,
    spans: Vec<Span>,
    mark: RunStyle,
    char_size_pt: f32,
) -> Paragraph {
    let ind = &ppr.indent;

    // `*Chars` 版本优先于 twips 版本 —— Word 就是这么做的，而中文文档里
    // 「首行缩进 2 字符」几乎无处不在（写成 firstLineChars="200"）。
    let left = ind
        .left_chars
        .map(|c| c as f32 / 100.0 * char_size_pt)
        .or_else(|| ind.left_twips.map(tw))
        .unwrap_or(0.0);

    let hanging = ind
        .hanging_chars
        .map(|c| c as f32 / 100.0 * char_size_pt)
        .or_else(|| ind.hanging_twips.map(tw));

    let first_line = match hanging {
        // 悬挂缩进与首行缩进互斥，悬挂优先且方向相反。
        Some(h) => -h,
        None => ind
            .first_line_chars
            .map(|c| c as f32 / 100.0 * char_size_pt)
            .or_else(|| ind.first_line_twips.map(tw))
            .unwrap_or(0.0),
    };

    let line = match (ppr.line, ppr.line_rule) {
        (Some(v), Some(LineRule::Exact)) => LineSpacing::Exact(tw(v)),
        (Some(v), Some(LineRule::AtLeast)) => LineSpacing::AtLeast(tw(v)),
        // auto：w:line 的单位是 240 分之一行。312/240 = 1.3 倍行距。
        // 当成 twips 处理的话就变成 15.6 磅的固定行高，每份文档的页数都会错。
        (Some(v), _) => LineSpacing::Multiple((v as f32 / 240.0).max(0.1)),
        _ => LineSpacing::Multiple(1.0),
    };

    Paragraph {
        align: match ppr.align {
            Some(model::Align::Center) => Align::Center,
            Some(model::Align::Right) => Align::Right,
            Some(model::Align::Both | model::Align::Distribute) => Align::Justify,
            Some(model::Align::Left) | None => Align::Left,
        },
        indent_left: left,
        indent_right: ind.right_twips.map(tw).unwrap_or(0.0),
        first_line,
        space_before: ppr.space_before_twips.map(tw).unwrap_or(0.0),
        space_after: ppr.space_after_twips.map(tw).unwrap_or(0.0),
        line,
        page_break_before: ppr.page_break_before.unwrap_or(false),
        snap_to_grid: ppr.snap_to_grid.unwrap_or(true),
        // 两个开关只要有一个开着就加间距：它们分别管西文和数字，
        // 而我们不在字符级区分这两类，统一按「非中日韩」处理。
        auto_space: ppr.auto_space_latin.unwrap_or(true) || ppr.auto_space_digits.unwrap_or(true),
        overflow_punct: ppr.overflow_punct.unwrap_or(true),
        tabs: ppr
            .tabs
            .iter()
            .filter(|t| !matches!(t.align, TabAlign::Bar | TabAlign::Clear))
            .map(|t| TabStop {
                pos: tw(t.pos),
                align: t.align,
                leader: t.leader,
            })
            .collect(),
        keep_next: ppr.keep_next.unwrap_or(false),
        keep_lines: ppr.keep_lines.unwrap_or(false),
        widow_control: ppr.widow_control.unwrap_or(true),
        contextual_spacing: ppr.contextual_spacing.unwrap_or(false),
        borders: Borders::from_model(&ppr.borders),
        shading: ppr.shading.flatten(),
        style_id: None,
        number: None,
        text,
        spans,
        objects: Vec::new(),
        floats: Vec::new(),
        mark,
    }
}

/// 单元格的默认边距：左右各 108 twips（5.4pt），上下为 0。
const DEFAULT_CELL_MARGINS: model::CellMargins = model::CellMargins {
    top: Some(0),
    left: Some(108),
    bottom: Some(0),
    right: Some(108),
};

/// 列数的上限。Word 的表格最多 63 列；超过这个数的只能是坏文件，按占位处理，
/// 不去按它分配列宽。
const MAX_COLUMNS: usize = 256;

/// 表格：列宽、各格占哪几列哪几行、每格四边的框线与边距都在这里定下来。
/// `nested`：它在别的表格的单元格里。
fn table(t: &model::Table, ctx: &Ctx, lists: &mut Option<Lists>, nested: bool) -> Block {
    let span_of = |c: &model::Cell| c.props.grid_span.unwrap_or(1).max(1) as usize;
    // 各行每一格从第几列开始。
    let starts: Vec<Vec<usize>> = t
        .rows
        .iter()
        .map(|row| {
            let mut col = row.props.grid_before as usize;
            row.cells
                .iter()
                .map(|c| {
                    let start = col;
                    col = col.saturating_add(span_of(c));
                    start
                })
                .collect()
        })
        .collect();
    let ncols = t
        .rows
        .iter()
        .zip(&starts)
        .map(|(row, s)| {
            let end = s.last().zip(row.cells.last());
            end.map_or(0, |(&col, c)| col.saturating_add(span_of(c)))
                .saturating_add(row.props.grid_after as usize)
        })
        .max()
        .unwrap_or(0)
        .max(t.grid.len());
    if ncols > MAX_COLUMNS {
        return Block::Placeholder(table_placeholder(t));
    }
    // 列宽以 tblGrid 为准；网格缺列时用只占这一列的格写的宽度（twips）补，
    // 还不知道的记作 0，排版时平分版心剩下的宽度。
    let mut columns: Vec<f32> = t.grid.iter().map(|&w| tw(w.max(0))).collect();
    if columns.len() < ncols {
        let mut from_cells = vec![0.0; ncols];
        for (row, s) in t.rows.iter().zip(&starts) {
            for (c, &col) in row.cells.iter().zip(s) {
                if let (1, Some(model::Width::Twips(w))) = (span_of(c), c.props.width) {
                    if from_cells[col] == 0.0 {
                        from_cells[col] = tw(w.max(0));
                    }
                }
            }
        }
        let known = columns.len();
        columns.extend_from_slice(&from_cells[known..]);
    }

    let format = table_format(&ctx.doc.styles, t);
    let tp = &format.tbl_pr;
    let look = tp.look.unwrap_or_default();
    let bands = (
        tp.row_band.unwrap_or(1).max(1) as usize,
        tp.col_band.unwrap_or(1).max(1) as usize,
    );
    let nrows = t.rows.len();

    // 纵向合并：写着 vMerge（续）、上一行同一列也有格开头的，接在那一格下面。
    let cell_at = |ri: usize, col: usize| starts.get(ri)?.iter().position(|&s| s == col);
    let continued = |ri: usize, col: usize| {
        ri > 0
            && cell_at(ri - 1, col).is_some()
            && cell_at(ri, col)
                .is_some_and(|k| t.rows[ri].cells[k].props.v_merge == Some(model::VMerge::Continue))
    };

    let last_row = t.rows.len().saturating_sub(1);
    let rows = t
        .rows
        .iter()
        .enumerate()
        .map(|(ri, row)| {
            let mut edges = tp.borders;
            edges.merge(&row.exceptions.borders);
            let mut margins = DEFAULT_CELL_MARGINS;
            margins.merge(&tp.cell_margins);
            margins.merge(&row.exceptions.cell_margins);
            let cells = row
                .cells
                .iter()
                .zip(&starts[ri])
                .map(|(cell, &col)| {
                    let span = span_of(cell);
                    let is_continued = continued(ri, col);
                    let rows = if is_continued {
                        1
                    } else {
                        1 + (ri + 1..t.rows.len())
                            .take_while(|&rj| continued(rj, col))
                            .count()
                    };
                    // 按位置取表格的框线：外沿用四边，内部用 insideH / insideV。
                    let inherited = |b: Option<model::Border>| Edge {
                        border: b.and_then(border),
                        explicit: false,
                    };
                    // 单元格一级（表格样式的单元格格式、条件格式、单元格自己）写了的边。
                    let mine = |b: Option<model::Border>, fallback: Edge| match b {
                        Some(b) => Edge {
                            border: border(b),
                            explicit: true,
                        },
                        None => fallback,
                    };
                    let outer = |at_edge: bool, edge, inside| {
                        inherited(if at_edge { edge } else { inside })
                    };
                    let mut borders = CellBorders {
                        top: outer(ri == 0, edges.top, edges.inside_h),
                        bottom: outer(ri + rows - 1 == last_row, edges.bottom, edges.inside_h),
                        left: outer(col == 0, edges.left, edges.inside_v),
                        right: outer(col + span >= ncols, edges.right, edges.inside_v),
                    };
                    // 表格样式里的单元格格式，再按优先级从低到高盖上各条件格式，最后是
                    // 单元格自己的。条件格式的框线按单元格在它那一块里的位置取。
                    let mut shading = tp.shading;
                    if row.exceptions.shading.is_some() {
                        shading = row.exceptions.shading;
                    }
                    let mut tc = format.tc_pr.clone();
                    let mut layer = format.layer.clone();
                    let cell_box = CellSpot {
                        rows: (ri, ri + rows - 1),
                        cols: (col, col + span - 1),
                    };
                    for (region, cond) in &format.conditions {
                        let Some(area) =
                            region_area(*region, look, bands, (nrows, ncols), cell_box)
                        else {
                            continue;
                        };
                        tc.merge_style(&cond.tc_pr);
                        let mut b = cond.tbl_pr.borders;
                        b.merge(&cond.tc_pr.borders);
                        let side =
                            |at_edge: bool, edge, inside| if at_edge { edge } else { inside };
                        tc.borders.merge(&model::TableBorders {
                            top: side(cell_box.rows.0 == area.rows.0, b.top, b.inside_h),
                            bottom: side(cell_box.rows.1 == area.rows.1, b.bottom, b.inside_h),
                            left: side(cell_box.cols.0 == area.cols.0, b.left, b.inside_v),
                            right: side(cell_box.cols.1 == area.cols.1, b.right, b.inside_v),
                            inside_h: None,
                            inside_v: None,
                        });
                        layer.ppr.cascade(&cond.ppr);
                        layer.rpr.merge(&cond.rpr);
                    }
                    tc.merge_style(&cell.props);
                    if tc.shading.is_some() {
                        shading = tc.shading;
                    }
                    let own = &tc.borders;
                    borders.top = mine(own.top, borders.top);
                    borders.bottom = mine(own.bottom, borders.bottom);
                    borders.left = mine(own.left, borders.left);
                    borders.right = mine(own.right, borders.right);
                    let mut m = margins;
                    m.merge(&tc.margins);
                    let pt = |v: Option<i32>| v.map(tw).unwrap_or(0.0);
                    let mut blocks = Vec::new();
                    for b in &cell.content {
                        match b {
                            model::Block::Para(p) => {
                                push_paragraph(&mut blocks, p, ctx, lists.as_mut(), Some(&layer))
                            }
                            model::Block::Table(inner) => {
                                blocks.push(table(inner, ctx, lists, true))
                            }
                        }
                    }
                    TableCell {
                        col,
                        span,
                        rows,
                        continued: is_continued,
                        borders,
                        shading: shading.flatten(),
                        margins: [pt(m.top), pt(m.left), pt(m.bottom), pt(m.right)],
                        v_align: tc.v_align.unwrap_or(model::VAlign::Top),
                        blocks,
                    }
                })
                .collect();
            TableRow {
                height: row.props.height.map(|(h, rule)| (tw(h), rule)),
                cant_split: row.props.cant_split,
                header: row.props.header,
                cells,
            }
        })
        .collect();

    // Word 2013 起 `w:tblInd` 量到左框线的外沿；之前的版本让首格的文字与正文对齐，
    // 表格往左让出单元格的左边距 —— 这一条只管正文里的表格，嵌套的表格（LibreOffice
    // 实测）两种兼容模式都量到左框线外沿。
    let indent = tp.indent.map(tw).unwrap_or(0.0)
        + if !nested && ctx.doc.settings.compat_mode.is_some_and(|m| m <= 14) {
            let mut m = DEFAULT_CELL_MARGINS;
            m.merge(&tp.cell_margins);
            -m.left.map(tw).unwrap_or(0.0)
        } else {
            tp.borders
                .left
                .and_then(border)
                .map_or(0.0, |b| b.thickness() / 2.0)
        };
    Block::Table(Table {
        columns,
        indent,
        align: match tp.align {
            Some(model::Align::Center) => Align::Center,
            Some(model::Align::Right) => Align::Right,
            _ => Align::Left,
        },
        rows,
    })
}

/// 一张表格用的样式：写了 `w:tblStyle` 用它，没写（或者找不到）用默认的表格样式；
/// 沿 basedOn 从根往下合好，再盖上表格自己的 `w:tblPr`。
struct TableFormat {
    tbl_pr: model::TblPr,
    tc_pr: model::TcPr,
    layer: TableLayer,
    /// 各区域的条件格式，按优先级从低到高。
    conditions: Vec<(model::TableRegion, model::TableCondition)>,
}

fn table_format(styles: &model::Styles, t: &model::Table) -> TableFormat {
    let id = t
        .props
        .style_id
        .as_deref()
        .filter(|id| styles.table.contains_key(*id))
        .or(styles.default_table_style.as_deref());
    // 从叶往根收集，basedOn 成环就停下。
    let mut chain = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut cur = id.map(str::to_string);
    while let Some(c) = cur {
        let Some(st) = styles.table.get(&c).filter(|_| seen.insert(c.clone())) else {
            break;
        };
        chain.push(st);
        cur = st.based_on.clone();
    }
    let mut tbl_pr = model::TblPr::default();
    let mut tc_pr = model::TcPr::default();
    let mut layer = TableLayer::default();
    let mut conditions: std::collections::BTreeMap<model::TableRegion, model::TableCondition> =
        Default::default();
    for st in chain.iter().rev() {
        tbl_pr.merge(&st.tbl_pr);
        tc_pr.merge_style(&st.tc_pr);
        layer.ppr.cascade(&st.ppr);
        layer.rpr.merge(&st.rpr);
        for (region, c) in &st.conditions {
            let e = conditions.entry(*region).or_default();
            e.ppr.cascade(&c.ppr);
            e.rpr.merge(&c.rpr);
            e.tbl_pr.merge(&c.tbl_pr);
            e.tc_pr.merge_style(&c.tc_pr);
        }
    }
    tbl_pr.merge(&t.props);
    TableFormat {
        tbl_pr,
        tc_pr,
        layer,
        conditions: conditions.into_iter().collect(),
    }
}

/// 一格（或一块区域）占的行、列，两端都含。
#[derive(Clone, Copy)]
struct CellSpot {
    rows: (usize, usize),
    cols: (usize, usize),
}

/// 条件格式 `region` 管不管这一格（按 `w:tblLook`），管的话它是哪一块区域。隔行、隔列
/// 的底纹跳过开了格式的首末行、首末列；四个角的格要行、列两边的格式都开着。
fn region_area(
    region: model::TableRegion,
    look: model::TblLook,
    (row_band, col_band): (usize, usize),
    (nrows, ncols): (usize, usize),
    cell: CellSpot,
) -> Option<CellSpot> {
    use model::TableRegion as R;
    let (last_row, last_col) = (nrows.saturating_sub(1), ncols.saturating_sub(1));
    let all_rows = (0, last_row);
    let all_cols = (0, last_col);
    let first_row = look.first_row && cell.rows.0 == 0;
    let final_row = look.last_row && cell.rows.1 == last_row;
    let first_col = look.first_col && cell.cols.0 == 0;
    let final_col = look.last_col && cell.cols.1 == last_col;
    // 第几条带：从去掉首行（首列）之后数起，每 `size` 行（列）一条。
    let band = |at: usize, skip_first: bool, size: usize, last: usize, skip_last: bool| {
        let from = skip_first as usize;
        let to = if skip_last {
            last.saturating_sub(1)
        } else {
            last
        };
        let k = (at.checked_sub(from)?) / size;
        let start = from + k * size;
        Some((k, (start, (start + size - 1).min(to))))
    };
    let area = match region {
        R::WholeTable => CellSpot {
            rows: all_rows,
            cols: all_cols,
        },
        R::Band1Horz | R::Band2Horz => {
            if look.no_h_band || first_row || final_row {
                return None;
            }
            let (k, rows) = band(
                cell.rows.0,
                look.first_row,
                row_band,
                last_row,
                look.last_row,
            )?;
            if (k % 2 == 0) != (region == R::Band1Horz) {
                return None;
            }
            CellSpot {
                rows,
                cols: all_cols,
            }
        }
        R::Band1Vert | R::Band2Vert => {
            if look.no_v_band || first_col || final_col {
                return None;
            }
            let (k, cols) = band(
                cell.cols.0,
                look.first_col,
                col_band,
                last_col,
                look.last_col,
            )?;
            if (k % 2 == 0) != (region == R::Band1Vert) {
                return None;
            }
            CellSpot {
                rows: all_rows,
                cols,
            }
        }
        R::FirstRow if first_row => CellSpot {
            rows: (0, 0),
            cols: all_cols,
        },
        R::LastRow if final_row => CellSpot {
            rows: (last_row, last_row),
            cols: all_cols,
        },
        R::FirstCol if first_col => CellSpot {
            rows: all_rows,
            cols: (0, 0),
        },
        R::LastCol if final_col => CellSpot {
            rows: all_rows,
            cols: (last_col, last_col),
        },
        R::NwCell if first_row && first_col => cell,
        R::NeCell if first_row && final_col => cell,
        R::SwCell if final_row && first_col => cell,
        R::SeCell if final_row && final_col => cell,
        _ => return None,
    };
    Some(area)
}

/// 画不出来的表格：留下行列数和每个单元格的文字。
fn table_placeholder(t: &model::Table) -> Placeholder {
    let mut text = Vec::new();
    collect_cell_texts(t, &mut text);
    Placeholder {
        kind: PlaceholderKind::Table {
            rows: t.rows.len(),
            cols: t.rows.iter().map(|r| r.cells.len()).max().unwrap_or(0),
        },
        text,
    }
}

/// 每个单元格一条；嵌套表格的单元格紧跟在外层单元格之后。
fn collect_cell_texts(t: &model::Table, out: &mut Vec<String>) {
    for cell in t.rows.iter().flat_map(|r| &r.cells) {
        let mut s = String::new();
        let mut nested = Vec::new();
        for block in &cell.content {
            match block {
                model::Block::Para(p) => {
                    for item in p.runs.iter().flat_map(|r| &r.items) {
                        if let RunItem::Text(t) = item {
                            s.push_str(t);
                        }
                    }
                }
                model::Block::Table(inner) => nested.push(inner),
            }
        }
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            out.push(trimmed.to_string());
        }
        for inner in nested {
            collect_cell_texts(inner, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docx::parse;

    const THEME: &str = r#"<a:theme xmlns:a="a"><a:fontScheme name="x">
<a:majorFont><a:latin typeface="Major Latin"/><a:ea typeface=""/><a:font script="Hans" typeface="Major Hans"/></a:majorFont>
<a:minorFont><a:latin typeface="Minor Latin"/><a:ea typeface=""/><a:font script="Hans" typeface="Minor Hans"/><a:font script="Jpan" typeface="Minor Jpan"/></a:minorFont>
</a:fontScheme></a:theme>"#;

    type Fonts = (Option<String>, Option<String>);

    /// 各段第一个 span 的（西文, 东亚）字体。docDefaults 用正文主题字体；
    /// 段落样式 `Named` 写的是字体名。
    fn fonts(paras: &[&str], lang: Option<&str>) -> Vec<Fonts> {
        let styles = parse::parse_styles(
            r#"<w:styles xmlns:w="w"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:asciiTheme="minorHAnsi" w:hAnsiTheme="minorHAnsi" w:eastAsiaTheme="minorEastAsia"/></w:rPr></w:rPrDefault></w:docDefaults>
<w:style w:type="paragraph" w:styleId="Named"><w:rPr><w:rFonts w:ascii="Style Latin" w:eastAsia="Style Song"/></w:rPr></w:style></w:styles>"#,
        );
        let settings = parse::parse_settings(&format!(
            r#"<w:settings xmlns:w="w">{}</w:settings>"#,
            lang.map(|l| format!(r#"<w:themeFontLang w:val="en-US" w:eastAsia="{l}"/>"#))
                .unwrap_or_default()
        ));
        let body: String = paras.iter().map(|p| format!("<w:p>{p}</w:p>")).collect();
        let mut doc = parse::parse_document(
            &format!(r#"<w:document xmlns:w="w"><w:body>{body}</w:body></w:document>"#),
            styles,
            settings,
        )
        .unwrap();
        doc.theme = parse::parse_theme(THEME);
        build(&doc)
            .blocks
            .iter()
            .map(|b| match b {
                Block::Para(p) => (
                    p.spans[0].style.font_latin.clone(),
                    p.spans[0].style.font_east_asia.clone(),
                ),
                _ => panic!("只有段落"),
            })
            .collect()
    }

    fn pair(latin: Option<&str>, east_asia: Option<&str>) -> Fonts {
        (latin.map(Into::into), east_asia.map(Into::into))
    }

    const PLAIN: &str = "<w:r><w:t>甲</w:t></w:r>";
    const RUN_NAME: &str =
        r#"<w:r><w:rPr><w:rFonts w:ascii="Run Latin"/></w:rPr><w:t>甲</w:t></w:r>"#;
    const RUN_BOTH: &str = r#"<w:r><w:rPr><w:rFonts w:ascii="Run Latin" w:asciiTheme="majorHAnsi"/></w:rPr><w:t>甲</w:t></w:r>"#;
    const RUN_MAJOR_EA: &str =
        r#"<w:r><w:rPr><w:rFonts w:eastAsiaTheme="majorEastAsia"/></w:rPr><w:t>甲</w:t></w:r>"#;
    const NAMED_THEME_RUN: &str = r#"<w:pPr><w:pStyle w:val="Named"/></w:pPr><w:r><w:rPr><w:rFonts w:asciiTheme="majorHAnsi"/></w:rPr><w:t>甲</w:t></w:r>"#;

    #[test]
    fn theme_fonts_resolve_by_slot() {
        let got = fonts(
            &[PLAIN, RUN_NAME, RUN_BOTH, RUN_MAJOR_EA, NAMED_THEME_RUN],
            Some("zh-CN"),
        );
        assert_eq!(
            got,
            [
                pair(Some("Minor Latin"), Some("Minor Hans")),
                // 写了西文字体名只换掉西文那一槽，东亚仍是主题字体。
                pair(Some("Run Latin"), Some("Minor Hans")),
                // 同一个元素里主题字体优先。
                pair(Some("Major Latin"), Some("Minor Hans")),
                pair(Some("Minor Latin"), Some("Major Hans")),
                // 上一级只写了主题字体，也整槽盖掉样式里的字体名。
                pair(Some("Major Latin"), Some("Style Song")),
            ]
        );
    }

    #[test]
    fn east_asian_theme_font_follows_the_theme_font_language() {
        assert_eq!(
            fonts(&[PLAIN, RUN_MAJOR_EA], Some("ja-JP")),
            [
                pair(Some("Minor Latin"), Some("Minor Jpan")),
                // 主题里没有这个文种的字体、`a:ea` 又是空的：解析不出来。
                pair(Some("Minor Latin"), None),
            ]
        );
        assert_eq!(fonts(&[PLAIN], None), [pair(Some("Minor Latin"), None)]);
        assert_eq!(east_asia_script("zh-TW"), Some("Hant"));
        assert_eq!(east_asia_script("zh-Hant-HK"), Some("Hant"));
        assert_eq!(east_asia_script("ZH-cn"), Some("Hans"));
        assert_eq!(east_asia_script("ko-KR"), Some("Hang"));
        assert_eq!(east_asia_script("en-US"), None);
    }

    /// 页码类域的结果标上记号；没有结果的补一个占位字；别的域照常显示。
    #[test]
    fn page_fields_are_marked() {
        let run = |inner: &str| format!("<w:r>{inner}</w:r>");
        let fld = |c: &str| run(&format!(r#"<w:fldChar w:fldCharType="{c}"/>"#));
        let code = |c: &str| run(&format!("<w:instrText>{c}</w:instrText>"));
        let p = [
            run("<w:t>第</w:t>"),
            fld("begin"),
            code(r"PAGE \* ROMAN"),
            fld("separate"),
            run("<w:t>1</w:t>"),
            fld("end"),
            fld("begin"),
            code("NUMPAGES"),
            fld("separate"),
            fld("end"),
            fld("begin"),
            code("DATE"),
            fld("separate"),
            run("<w:t>今天</w:t>"),
            fld("end"),
        ]
        .concat();
        let doc = parse::parse_document(
            &format!(r#"<w:document xmlns:w="w"><w:body><w:p>{p}</w:p></w:body></w:document>"#),
            parse::parse_styles(r#"<w:styles xmlns:w="w"/>"#),
            Default::default(),
        )
        .unwrap();
        let Block::Para(p) = &build(&doc).blocks[0] else {
            unreachable!()
        };
        let got: Vec<_> = p
            .spans
            .iter()
            .map(|s| (p.text[s.range.clone()].to_string(), s.style.field))
            .collect();
        let page = Field {
            id: 0,
            kind: FieldKind::Page,
            format: Some("upperRoman"),
        };
        let pages = Field {
            id: 1,
            kind: FieldKind::NumPages,
            format: None,
        };
        assert_eq!(
            got,
            [
                ("第".to_string(), None),
                ("1".to_string(), Some(page)),
                ("1".to_string(), Some(pages)),
                ("今天".to_string(), None),
            ]
        );
    }

    /// 某类页眉本节没写时沿用上一节的；写了就换成本节的。
    #[test]
    fn headers_are_inherited_by_kind() {
        let mut doc = parse::parse_document(
            r#"<w:document xmlns:w="w" xmlns:r="r"><w:body>
<w:p><w:pPr><w:sectPr><w:headerReference w:type="default" r:id="h1"/><w:headerReference w:type="first" r:id="h2"/></w:sectPr></w:pPr></w:p>
<w:p><w:pPr><w:sectPr><w:headerReference w:type="first" r:id="h3"/></w:sectPr></w:pPr></w:p>
<w:p/><w:sectPr/></w:body></w:document>"#,
            parse::parse_styles(r#"<w:styles xmlns:w="w"/>"#),
            Default::default(),
        )
        .unwrap();
        for (id, text) in [("h1", "默认一"), ("h2", "首页一"), ("h3", "首页二")] {
            doc.header_footer.insert(
                id.into(),
                parse::parse_header_footer(&format!(
                    r#"<w:hdr xmlns:w="w"><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:hdr>"#
                )),
            );
        }
        let ir = build(&doc);
        let text = |b: &Option<Vec<Block>>| match b.as_deref() {
            Some([Block::Para(p)]) => p.text.clone(),
            _ => String::new(),
        };
        let got: Vec<(String, String)> = ir
            .sections
            .iter()
            .map(|s| (text(&s.headers.default), text(&s.headers.first)))
            .collect();
        assert_eq!(
            got,
            [
                ("默认一".into(), "首页一".into()),
                ("默认一".into(), "首页二".into()),
                ("默认一".into(), "首页二".into()),
            ]
        );
    }

    /// 表格：列宽取 tblGrid；框线按位置取表格的四边或 insideH / insideV，行例外与
    /// 单元格自己写的依次盖上去；纵向合并记在开头那一格上。
    #[test]
    fn table_cells_get_positional_borders_and_merges() {
        let b = |side: &str, sz: u32| {
            format!(r#"<w:{side} w:val="single" w:sz="{sz}" w:color="000000"/>"#)
        };
        let tbl_borders: String = [("top", 12), ("left", 12), ("bottom", 12), ("right", 12)]
            .iter()
            .chain(&[("insideH", 4), ("insideV", 4)])
            .map(|&(side, sz)| b(side, sz))
            .collect();
        let cell = |pr: &str, text: &str| {
            format!("<w:tc><w:tcPr>{pr}</w:tcPr><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:tc>")
        };
        let rows = [
            cell(r#"<w:vMerge w:val="restart"/>"#, "甲")
                + &cell("", "乙")
                + &cell(r#"<w:shd w:val="clear" w:fill="FF0000"/>"#, "丙"),
            r#"<w:tblPrEx><w:tblBorders><w:insideV w:val="nil"/></w:tblBorders></w:tblPrEx>"#
                .to_string()
                + &cell("<w:vMerge/>", "")
                + &cell(
                    r#"<w:gridSpan w:val="2"/><w:tcBorders><w:top w:val="nil"/></w:tcBorders>"#,
                    "丁",
                ),
            cell("", "戊") + &cell(r#"<w:gridSpan w:val="2"/>"#, "己"),
        ];
        let body = format!(
            r#"<w:tbl><w:tblPr><w:tblBorders>{tbl_borders}</w:tblBorders><w:tblInd w:w="100" w:type="dxa"/><w:tblCellMar><w:left w:w="200" w:type="dxa"/></w:tblCellMar></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="4000"/><w:gridCol w:w="1000"/></w:tblGrid>{}</w:tbl><w:p/>"#,
            rows.iter()
                .map(|r| format!("<w:tr>{r}</w:tr>"))
                .collect::<String>()
        );
        let build_with = |mode: &str| {
            let doc = parse::parse_document(
                &format!(r#"<w:document xmlns:w="w"><w:body>{body}</w:body></w:document>"#),
                parse::parse_styles(r#"<w:styles xmlns:w="w"/>"#),
                parse::parse_settings(&format!(
                    r#"<w:settings xmlns:w="w"><w:compat>{mode}</w:compat></w:settings>"#
                )),
            )
            .unwrap();
            match build(&doc).blocks.into_iter().next() {
                Some(Block::Table(t)) => t,
                _ => panic!("第一块应当是表格"),
            }
        };
        // Word 2013 起列边界在左框线外沿往里半个线宽处；Word 2010 让首格文字对齐正文，
        // 表格往左让出单元格的左边距（这里是 10pt）。
        let old =
            build_with(r#"<w:compatSetting w:name="compatibilityMode" w:uri="x" w:val="14"/>"#);
        assert_eq!(old.indent, 5.0 - 10.0);
        let t = build_with("");
        assert_eq!(t.indent, 5.0 + 0.75);
        assert_eq!(t.columns, [100.0, 200.0, 50.0]);

        let width = |e: Edge| e.border.map(|b| b.width);
        let cell = |r: usize, c: usize| &t.rows[r].cells[c];
        // 第一行第一格：上、左是表格外框；它往下合并两行，下边就还在表格里面。
        let a = cell(0, 0);
        assert_eq!((a.col, a.span, a.rows, a.continued), (0, 1, 2, false));
        assert_eq!(width(a.borders.top), Some(1.5));
        assert_eq!(width(a.borders.left), Some(1.5));
        assert_eq!(width(a.borders.bottom), Some(0.5));
        assert_eq!(width(a.borders.right), Some(0.5));
        assert_eq!(a.margins, [0.0, 10.0, 0.0, 5.4]);
        assert_eq!(cell(0, 2).shading, Some([255, 0, 0]));
        assert_eq!(width(cell(0, 2).borders.right), Some(1.5));

        // 第二行：第一格接在上面；横跨两列的格自己去掉上边，行例外去掉 insideV。
        assert!(cell(1, 0).continued);
        let d = cell(1, 1);
        assert_eq!((d.col, d.span), (1, 2));
        assert_eq!(
            d.borders.top,
            Edge {
                border: None,
                explicit: true
            }
        );
        assert_eq!(d.borders.left, Edge::default());
        assert_eq!(width(d.borders.right), Some(1.5));

        // 最后一行的下边是表格的下框线。
        assert_eq!(width(cell(2, 0).borders.bottom), Some(1.5));
        assert_eq!(width(cell(2, 1).borders.bottom), Some(1.5));
        assert_eq!(cell(2, 0).rows, 1);
    }

    /// 列数大得离谱的坏表格按占位处理（文字照留），不按它分配列宽。
    #[test]
    fn absurdly_wide_tables_become_placeholders() {
        let doc = parse::parse_document(
            r#"<w:document xmlns:w="w"><w:body><w:tbl><w:tr><w:tc><w:tcPr><w:gridSpan w:val="2000000000"/></w:tcPr><w:p><w:r><w:t>甲</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/></w:body></w:document>"#,
            parse::parse_styles(r#"<w:styles xmlns:w="w"/>"#),
            Default::default(),
        )
        .unwrap();
        let ir = build(&doc);
        assert!(
            matches!(ir.blocks.first(), Some(Block::Placeholder(p)) if p.text == ["甲"]),
            "{:?}",
            ir.blocks.first()
        );
    }

    /// 表格样式：框线、边距来自样式（沿 basedOn 合并，没写 tblStyle 用默认的表格样式）；
    /// 单元格里的段落按 docDefaults → 表格样式 → 段落样式 → 直接格式层叠；首行、隔行、
    /// 末行按 tblLook 生效，末行的上框线按它在那一块里的位置取。
    #[test]
    fn table_styles_cascade_into_cells() {
        let bd = |side: &str, val: &str| {
            format!(r#"<w:{side} w:val="{val}" w:sz="4" w:space="0" w:color="000000"/>"#)
        };
        let all: String = ["top", "left", "bottom", "right", "insideH", "insideV"]
            .iter()
            .map(|s| bd(s, "single"))
            .collect();
        let styles = parse::parse_styles(&format!(
            r#"<w:styles xmlns:w="w"><w:docDefaults><w:rPrDefault><w:rPr><w:sz w:val="24"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after="200" w:line="360" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults>
<w:style w:type="paragraph" w:default="1" w:styleId="Normal"/>
<w:style w:type="paragraph" w:styleId="Tight"><w:basedOn w:val="Normal"/><w:pPr><w:spacing w:after="100"/></w:pPr></w:style>
<w:style w:type="table" w:default="1" w:styleId="TableNormal"><w:tblPr><w:tblCellMar><w:left w:w="200" w:type="dxa"/><w:right w:w="200" w:type="dxa"/></w:tblCellMar></w:tblPr></w:style>
<w:style w:type="table" w:styleId="TableGrid"><w:basedOn w:val="TableNormal"/><w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/></w:pPr><w:rPr><w:sz w:val="20"/></w:rPr><w:tblPr><w:tblBorders>{all}</w:tblBorders></w:tblPr></w:style>
<w:style w:type="table" w:styleId="Fancy"><w:basedOn w:val="TableGrid"/>
<w:tblStylePr w:type="firstRow"><w:rPr><w:b/></w:rPr><w:tcPr><w:shd w:val="clear" w:fill="4472C4"/></w:tcPr></w:tblStylePr>
<w:tblStylePr w:type="band1Horz"><w:tcPr><w:shd w:val="clear" w:fill="D9E2F3"/></w:tcPr></w:tblStylePr>
<w:tblStylePr w:type="lastRow"><w:tcPr><w:tcBorders>{}</w:tcBorders></w:tcPr></w:tblStylePr></w:style>
</w:styles>"#,
            bd("top", "double")
        ));
        let p = |ppr: &str| format!("<w:p><w:pPr>{ppr}</w:pPr><w:r><w:t>字</w:t></w:r></w:p>");
        let row = |ppr: &str| format!("<w:tr><w:tc>{}</w:tc></w:tr>", p(ppr));
        let tbl = |pr: &str, rows: &str| {
            format!(
                r#"<w:tbl><w:tblPr>{pr}</w:tblPr><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid>{rows}</w:tbl><w:p/>"#
            )
        };
        let body = tbl(
            r#"<w:tblStyle w:val="TableGrid"/>"#,
            &(row("") + &row(r#"<w:pStyle w:val="Tight"/>"#)),
        ) + &tbl("", &row(""))
            + &tbl(
                r#"<w:tblStyle w:val="Fancy"/><w:tblLook w:firstRow="1" w:lastRow="1" w:noHBand="0" w:noVBand="1"/>"#,
                &row("").repeat(5),
            )
            + &tbl(
                r#"<w:tblStyle w:val="Fancy"/><w:tblLook w:val="0020"/>"#,
                &row("").repeat(2),
            );
        let doc = parse::parse_document(
            &format!(r#"<w:document xmlns:w="w"><w:body>{body}</w:body></w:document>"#),
            styles,
            Default::default(),
        )
        .unwrap();
        let ir = build(&doc);
        let tables: Vec<&Table> = ir
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Table(t) => Some(t),
                _ => None,
            })
            .collect();
        let para = |t: &Table, r: usize| match &t.rows[r].cells[0].blocks[0] {
            Block::Para(p) => p.clone(),
            _ => panic!("格里应当是段落"),
        };

        // 网格型：样式给框线；段落的段距、字号来自表格样式，写了段距的段落样式优先。
        let grid = tables[0];
        assert!(grid.rows[0].cells[0].borders.top.border.is_some());
        let first = para(grid, 0);
        assert_eq!(
            (first.space_after, first.spans[0].style.size_pt),
            (0.0, 10.0)
        );
        assert!(matches!(first.line, LineSpacing::Multiple(m) if m == 1.0));
        assert_eq!(para(grid, 1).space_after, 5.0);

        // 没写 tblStyle：默认的表格样式（这里左右边距 10pt），没有框线，段落照 docDefaults。
        let plain = tables[1];
        assert!(plain.rows[0].cells[0].borders.top.border.is_none());
        assert_eq!(plain.rows[0].cells[0].margins, [0.0, 10.0, 0.0, 10.0]);
        assert_eq!(para(plain, 0).space_after, 10.0);

        // 首行加粗铺深色，隔行从第二行起，末行上框线是双线；字号沿 basedOn 取网格型的。
        let fancy = tables[2];
        let fill = |r: usize| fancy.rows[r].cells[0].shading;
        assert_eq!(
            (0..5).map(fill).collect::<Vec<_>>(),
            [
                Some([0x44, 0x72, 0xC4]),
                Some([0xD9, 0xE2, 0xF3]),
                None,
                Some([0xD9, 0xE2, 0xF3]),
                None
            ]
        );
        assert!(para(fancy, 0).spans[0].style.bold && !para(fancy, 1).spans[0].style.bold);
        assert_eq!(para(fancy, 1).spans[0].style.size_pt, 10.0);
        let top = |r: usize| fancy.rows[r].cells[0].borders.top.border.map(|b| b.style);
        assert_eq!(
            (top(3), top(4)),
            (Some(BorderStyle::Single), Some(BorderStyle::Double))
        );

        // 只写了十六进制的 tblLook：0x0020 是首行。
        let hex = tables[3];
        assert!(para(hex, 0).spans[0].style.bold);
    }
}
