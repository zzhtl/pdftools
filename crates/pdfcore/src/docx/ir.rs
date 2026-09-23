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

use super::layout::{
    Calib, Cascade, CharGrid, HeaderFooter, ListNumbers, RunFormat, Sections, Tables, Theme,
};
use super::model::{
    self, BreakKind, FieldChar, FontRef, LineRule, NumSuffix, PPr, RPr, RunItem, ThemeScript,
};
pub use super::model::{BorderStyle, SectionStart, TabAlign, TabLeader, UnderlineStyle, VertAlign};
use super::numbering::Lists;
use super::resolve::Resolver;

pub const LINE_BREAK: char = '\u{2028}';
pub const PAGE_BREAK: char = '\u{000C}';
pub const COLUMN_BREAK: char = '\u{000B}';

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
    /// 本段挂了自动编号，但编号文字没有生成。
    pub numbering_dropped: bool,
    /// 段首的编号（已经写在 `text` 开头）。
    pub number: Option<NumberLabel>,
    pub text: String,
    pub spans: Vec<Span>,
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
    pub columns: Vec<f32>,
    /// 左对齐时，第一条列边界离版心左边多远。由 `w:tblInd` 按兼容模式折算，
    /// 见 [`Tables::Drawn`]。
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
    /// 文档引用了页眉或页脚，但本版本不渲染。
    pub has_header_footer: bool,
    /// 偶数页用单独的页眉页脚（`w:evenAndOddHeaders`）。
    pub even_and_odd_headers: bool,
    /// 相邻两段的段后距与段前距取较大值而不是相加（HTML 的规矩）。
    /// 文档没有设置 `w:doNotUseHTMLParagraphAutoSpacing` 时为真，见 `Calib::para_spacing`。
    pub html_paragraph_spacing: bool,
    /// 默认制表位的间距（点）。`settings.xml` 没写时是 Word 的缺省 36pt。
    pub default_tab_stop: f32,
    /// 不认识、按阿拉伯数字输出的编号格式。
    pub num_format_fallbacks: Vec<String>,
    pub blocks: Vec<Block>,
}

pub fn build(doc: &model::Document, calib: &Calib) -> Document {
    let numbering = (calib.list_numbers == ListNumbers::Rendered).then_some(&doc.numbering);
    let resolver = Resolver::new(&doc.styles, calib.cascade == Cascade::Spec, numbering);
    let mut lists = numbering.map(|n| Lists::new(n, &doc.styles));
    let fonts = FontNames::new(doc, calib);
    // 段落里的 `w:sectPr` 结束一节；最后一节的设置在 body 末尾。
    // 重写前只用最后一节排全文。
    let each = calib.sections == Sections::Each;

    let ctx = Ctx {
        doc,
        resolver: &resolver,
        fonts: &fonts,
        calib,
    };
    let mut blocks = Vec::with_capacity(doc.body.len());
    let mut ends: Vec<(&model::SectPr, Range<usize>)> = Vec::new();
    let mut start = 0;
    for block in &doc.body {
        match block {
            model::Block::Para(p) => {
                push_paragraph(&mut blocks, p, &ctx, lists.as_mut());
                if let (true, Some(sp)) = (each, &p.section) {
                    ends.push((sp, start..blocks.len()));
                    start = blocks.len();
                }
            }
            model::Block::Table(t) if calib.tables == Tables::Drawn => {
                blocks.push(table(t, &ctx, &mut lists))
            }
            model::Block::Table(t) => blocks.push(Block::Placeholder(table_placeholder(t))),
        }
    }
    ends.push((&doc.section, start..blocks.len()));

    // 页眉页脚：本节没写的那一类沿用上一节的。
    let hf_on = calib.header_footer == HeaderFooter::Drawn;
    let (mut headers, mut footers) = (HeaderSet::default(), HeaderSet::default());
    // 字符网格的格宽以 Normal 样式的字号为基准。
    let normal_pt = resolver
        .mark(&resolver.paragraph(&PPr::default()))
        .size_half_pt
        .map(half_pt)
        .unwrap_or(calib.default_size_pt);
    let mut sections = Vec::with_capacity(ends.len());
    for (sp, range) in ends {
        let mut section = Section::from_model(sp, range);
        if calib.char_grid == CharGrid::Cells {
            section.char_pitch = sp
                .doc_grid
                .filter(|g| g.chars)
                .and_then(|g| g.char_space)
                .map(|cs| normal_pt + cs as f32 / 4096.0)
                .filter(|p| *p > 0.0);
        }
        if hf_on {
            headers = header_set(&headers, &sp.headers, &ctx);
            footers = header_set(&footers, &sp.footers, &ctx);
            section.headers = headers.clone();
            section.footers = footers.clone();
        }
        sections.push(section);
    }
    let has_header_footer = doc.section.has_header_footer
        || (each
            && doc.body.iter().any(|b| {
                matches!(b, model::Block::Para(p) if p.section.as_ref().is_some_and(|s| s.has_header_footer))
            }));

    Document {
        sections,
        has_header_footer,
        even_and_odd_headers: doc.settings.even_and_odd_headers,
        html_paragraph_spacing: !doc.settings.no_html_paragraph_spacing,
        default_tab_stop: doc.settings.default_tab_stop.map(tw).unwrap_or(36.0),
        num_format_fallbacks: lists
            .map(|l| l.fallbacks.into_iter().collect())
            .unwrap_or_default(),
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
            model::Block::Para(p) => push_paragraph(&mut out, p, ctx, None),
            model::Block::Table(t) if ctx.calib.tables == Tables::Drawn => {
                out.push(table(t, ctx, &mut None))
            }
            model::Block::Table(t) => out.push(Block::Placeholder(table_placeholder(t))),
        }
    }
    out
}

/// 构建各段落都要用到的东西。
struct Ctx<'a> {
    doc: &'a model::Document,
    resolver: &'a Resolver<'a>,
    fonts: &'a FontNames<'a>,
    calib: &'a Calib,
}

fn push_paragraph(out: &mut Vec<Block>, p: &model::Para, ctx: &Ctx, lists: Option<&mut Lists>) {
    let Ctx {
        doc,
        resolver,
        fonts,
        calib,
    } = *ctx;
    let ppr = resolver.paragraph(&p.ppr);
    let mut text = String::new();
    let mut spans: Vec<Span> = Vec::with_capacity(p.runs.len());
    let mut drawings = Vec::new();
    // 页码类域的结果文字标上记号，排版时代入真实的数。重写前不认域，结果照原样显示。
    let fields_on = calib.header_footer == HeaderFooter::Drawn;
    let mut fields = OpenFields::default();
    for run in &p.runs {
        let rpr = resolver.run(&ppr, &run.rpr);
        let full = calib.run_format == RunFormat::Full;
        // 隐藏文字不显示，也不占位置。
        if full && rpr.vanish == Some(true) {
            continue;
        }
        let mut style = run_style(&rpr, fonts, calib);
        if full {
            // 只做外部链接；文档内的书签跳转还没做。
            style.link = match &run.link {
                Some(model::LinkRef::Rel(id)) => doc.hyperlinks.get(id).cloned(),
                _ => None,
            };
        }
        if fields_on {
            style.field = fields.current();
        }
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
                RunItem::Text(t) if full && rpr.small_caps == Some(true) => {
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
                RunItem::Text(t) if full && rpr.caps == Some(true) => {
                    push(t.to_uppercase(), &style)
                }
                RunItem::Text(t) => push(t.clone(), &style),
                RunItem::Tab => push("\t".into(), &style),
                RunItem::Break(BreakKind::Line) => push(LINE_BREAK.into(), &style),
                RunItem::Break(BreakKind::Page) => push(PAGE_BREAK.into(), &style),
                RunItem::Break(BreakKind::Column) => push(COLUMN_BREAK.into(), &style),
                RunItem::NoBreakHyphen => push("\u{2011}".into(), &style),
                RunItem::Drawing { alt } => drawings.push(alt.clone()),
                RunItem::FieldChar(FieldChar::Begin) if fields_on => fields.begin(),
                RunItem::FieldCode(code) if fields_on => fields.code(code),
                RunItem::FieldChar(FieldChar::Separate) if fields_on => fields.separate(),
                RunItem::FieldChar(FieldChar::End) if fields_on => {
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
                RunItem::FieldChar(_) | RunItem::FieldCode(_) => {}
                // 符号用它自己的字体。符号字体（Symbol、Wingdings）里的码位
                // 写成单字节时，实际在私用区 U+F0xx。
                RunItem::Sym { .. } if !full => {}
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
    let mark_rpr = resolver.mark(&ppr);
    let char_size = mark_rpr
        .size_half_pt
        .map(half_pt)
        .or_else(|| spans.first().map(|s| s.style.size_pt))
        .unwrap_or(calib.default_size_pt);

    // 编号写在段首，格式是段落标记的格式叠上编号级别的格式。
    let dropped = lists.is_none() && ppr.numbering;
    let label = lists.and_then(|l| {
        let ilvl = u8::try_from(ppr.num_ilvl.unwrap_or(0)).ok()?;
        l.next(ppr.num_id?, ilvl)
    });
    let (text, spans, number) = match label {
        Some(label) if !label.text.is_empty() || label.suffix != NumSuffix::Nothing => {
            let mut rpr = mark_rpr.clone();
            rpr.merge(&label.rpr);
            let style = run_style(&rpr, fonts, calib);
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

    let mark = run_style(&mark_rpr, fonts, calib);
    let mut para = paragraph(&ppr, text, spans, mark, char_size);
    para.numbering_dropped = dropped;
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

/// 把 `w:rFonts` 的字体槽落实成字体名：主题字体要查主题部件。
struct FontNames<'a> {
    theme: &'a model::Theme,
    /// 东亚主题字体取主题里哪个文种的字体（`Hans` 之类），来自 `w:themeFontLang`。
    east_asia_script: Option<&'static str>,
    /// 不认主题字体时照旧只看字体名。
    legacy: bool,
}

impl<'a> FontNames<'a> {
    fn new(doc: &'a model::Document, calib: &Calib) -> Self {
        Self {
            theme: &doc.theme,
            east_asia_script: doc
                .settings
                .theme_font_lang_east_asia
                .as_deref()
                .and_then(east_asia_script),
            legacy: calib.theme == Theme::Ignored,
        }
    }

    fn latin(&self, rpr: &RPr) -> Option<String> {
        if self.legacy {
            return rpr.legacy_font_ascii.clone();
        }
        self.name(rpr.font_ascii.as_ref()?)
    }

    fn east_asia(&self, rpr: &RPr) -> Option<String> {
        if self.legacy {
            return rpr.legacy_font_east_asia.clone();
        }
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

fn run_style(rpr: &RPr, fonts: &FontNames, calib: &Calib) -> RunStyle {
    let full = calib.run_format == RunFormat::Full;
    let color = rpr.color.unwrap_or([0, 0, 0]);
    RunStyle {
        size_pt: rpr
            .size_half_pt
            .map(half_pt)
            .unwrap_or(calib.default_size_pt),
        bold: rpr.bold.unwrap_or(false),
        italic: rpr.italic.unwrap_or(false),
        underline: rpr
            .underline
            .filter(|u| u.style != UnderlineStyle::None)
            .map(|u| match calib.run_format {
                RunFormat::Full => Underline {
                    style: u.style,
                    color: u.color.unwrap_or(color),
                },
                // 重写前只有单线，颜色跟文字走。
                RunFormat::Legacy => Underline {
                    style: UnderlineStyle::Single,
                    color,
                },
            }),
        strike: rpr.strike.unwrap_or(false),
        double_strike: full && rpr.double_strike.unwrap_or(false),
        background: if full {
            rpr.highlight.flatten().or(rpr.shading.flatten())
        } else {
            None
        },
        color,
        font_latin: fonts.latin(rpr),
        font_east_asia: fonts.east_asia(rpr),
        hint_east_asia: rpr.hint_east_asia.unwrap_or(false),
        char_spacing: match calib.run_format {
            RunFormat::Full => rpr.spacing.map(tw).unwrap_or(0.0),
            RunFormat::Legacy => 0.0,
        },
        vert_align: if full {
            rpr.vert_align.unwrap_or_default()
        } else {
            VertAlign::Baseline
        },
        position_pt: if full {
            rpr.position.map(|v| v as f32 / 2.0).unwrap_or(0.0)
        } else {
            0.0
        },
        link: None,
        field: None,
        kern: rpr
            .kern
            .is_some_and(|k| k > 0 && size_half_pt(rpr, calib) >= k as f32),
    }
}

/// 字号，半磅。
fn size_half_pt(rpr: &RPr, calib: &Calib) -> f32 {
    rpr.size_half_pt
        .map(|v| v as f32)
        .unwrap_or(calib.default_size_pt * 2.0)
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
        numbering_dropped: false,
        number: None,
        text,
        spans,
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
fn table(t: &model::Table, ctx: &Ctx, lists: &mut Option<Lists>) -> Block {
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
    // 列宽以 tblGrid 为准；网格缺列时用只占这一列的格写的宽度补，再不行就用已有列的平均宽度。
    let mut columns: Vec<f32> = t.grid.iter().map(|&w| tw(w)).collect();
    if columns.len() < ncols {
        let mut from_cells = vec![None; ncols];
        for (row, s) in t.rows.iter().zip(&starts) {
            for (c, &col) in row.cells.iter().zip(s) {
                if let (1, Some(model::Width::Twips(w))) = (span_of(c), c.props.width) {
                    from_cells[col].get_or_insert(tw(w));
                }
            }
        }
        let fallback = match columns.len() {
            0 => 72.0,
            n => columns.iter().sum::<f32>() / n as f32,
        };
        let known = columns.len();
        columns.extend(from_cells[known..].iter().map(|w| w.unwrap_or(fallback)));
    }

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
            let mut edges = t.props.borders;
            edges.merge(&row.exceptions.borders);
            let mut margins = DEFAULT_CELL_MARGINS;
            margins.merge(&t.props.cell_margins);
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
                    let own = |mine: Option<model::Border>, fallback: Edge| match mine {
                        Some(b) => Edge {
                            border: border(b),
                            explicit: true,
                        },
                        None => fallback,
                    };
                    let tc = &cell.props.borders;
                    let outer = |at_edge: bool, edge, inside| {
                        inherited(if at_edge { edge } else { inside })
                    };
                    let borders = CellBorders {
                        top: own(tc.top, outer(ri == 0, edges.top, edges.inside_h)),
                        bottom: own(
                            tc.bottom,
                            outer(ri + rows - 1 == last_row, edges.bottom, edges.inside_h),
                        ),
                        left: own(tc.left, outer(col == 0, edges.left, edges.inside_v)),
                        right: own(
                            tc.right,
                            outer(col + span >= ncols, edges.right, edges.inside_v),
                        ),
                    };
                    let mut m = margins;
                    m.merge(&cell.props.margins);
                    let pt = |v: Option<i32>| v.map(tw).unwrap_or(0.0);
                    let shading = cell
                        .props
                        .shading
                        .or(row.exceptions.shading)
                        .or(t.props.shading)
                        .flatten();
                    let mut blocks = Vec::new();
                    for b in &cell.content {
                        match b {
                            model::Block::Para(p) => {
                                push_paragraph(&mut blocks, p, ctx, lists.as_mut())
                            }
                            // 嵌套的表格：本版本按占位处理。
                            model::Block::Table(inner) => {
                                blocks.push(Block::Placeholder(table_placeholder(inner)))
                            }
                        }
                    }
                    TableCell {
                        col,
                        span,
                        rows,
                        continued: is_continued,
                        borders,
                        shading,
                        margins: [pt(m.top), pt(m.left), pt(m.bottom), pt(m.right)],
                        v_align: cell.props.v_align.unwrap_or(model::VAlign::Top),
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
    // 表格往左让出单元格的左边距。
    let indent = t.props.indent.map(tw).unwrap_or(0.0)
        + if ctx.doc.settings.compat_mode.is_some_and(|m| m <= 14) {
            let mut m = DEFAULT_CELL_MARGINS;
            m.merge(&t.props.cell_margins);
            -m.left.map(tw).unwrap_or(0.0)
        } else {
            t.props
                .borders
                .left
                .and_then(border)
                .map_or(0.0, |b| b.thickness() / 2.0)
        };
    Block::Table(Table {
        columns,
        indent,
        align: match t.props.align {
            Some(model::Align::Center) => Align::Center,
            Some(model::Align::Right) => Align::Right,
            _ => Align::Left,
        },
        rows,
    })
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
    fn fonts(paras: &[&str], lang: Option<&str>, calib: &Calib) -> Vec<Fonts> {
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
        build(&doc, calib)
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
            &Calib::current(),
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
        let calib = Calib::current();
        assert_eq!(
            fonts(&[PLAIN, RUN_MAJOR_EA], Some("ja-JP"), &calib),
            [
                pair(Some("Minor Latin"), Some("Minor Jpan")),
                // 主题里没有这个文种的字体、`a:ea` 又是空的：解析不出来。
                pair(Some("Minor Latin"), None),
            ]
        );
        assert_eq!(
            fonts(&[PLAIN], None, &calib),
            [pair(Some("Minor Latin"), None)]
        );
        assert_eq!(east_asia_script("zh-TW"), Some("Hant"));
        assert_eq!(east_asia_script("zh-Hant-HK"), Some("Hant"));
        assert_eq!(east_asia_script("ZH-cn"), Some("Hans"));
        assert_eq!(east_asia_script("ko-KR"), Some("Hang"));
        assert_eq!(east_asia_script("en-US"), None);
    }

    /// 旧规则不认主题字体，字体名逐个属性覆盖。
    #[test]
    fn legacy_rules_ignore_theme_fonts() {
        assert_eq!(
            fonts(
                &[PLAIN, RUN_BOTH, NAMED_THEME_RUN],
                Some("zh-CN"),
                &Calib::legacy()
            ),
            [
                pair(None, None),
                pair(Some("Run Latin"), None),
                pair(Some("Style Latin"), Some("Style Song")),
            ]
        );
    }

    /// 页码类域的结果标上记号；没有结果的补一个占位字；别的域照常显示。
    /// 重写前的规则不认域。
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
        let spans = |calib: &Calib| {
            let Block::Para(p) = &build(&doc, calib).blocks[0] else {
                unreachable!()
            };
            p.spans
                .iter()
                .map(|s| (p.text[s.range.clone()].to_string(), s.style.field))
                .collect::<Vec<_>>()
        };
        let got = spans(&Calib::current());
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
        let legacy = spans(&Calib::legacy());
        assert!(legacy.iter().all(|(_, f)| f.is_none()));
        assert_eq!(
            legacy.iter().map(|(t, _)| t.as_str()).collect::<String>(),
            "第1今天"
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
        let ir = build(&doc, &Calib::current());
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
            match build(&doc, &Calib::current()).blocks.into_iter().next() {
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
        let ir = build(&doc, &Calib::current());
        assert!(
            matches!(ir.blocks.first(), Some(Block::Placeholder(p)) if p.text == ["甲"]),
            "{:?}",
            ir.blocks.first()
        );
    }
}
