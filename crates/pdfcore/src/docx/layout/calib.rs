//! 校准开关：与 Word 的行为可能有出入、要对着参照实测才能定的排版规则。
//!
//! 每个开关都要写明现在的取值依据。[`Calib::legacy`] 复刻重写前引擎的行为，
//! 新引擎用它排出来的结果必须与旧引擎逐坐标一致 —— 这是重写过程中「行为没变」的证据；
//! 改规则时新增一个取值，而不是改掉旧的。

#[derive(Debug, Clone)]
pub struct Calib {
    /// docDefaults、样式、直接格式都没写 `w:sz` 时用的字号。
    ///
    /// OOXML 规定缺省是 10pt（20 个半磅）；LibreOffice 在 docDefaults 不写字号时
    /// 实测也是 10pt。重写前按五号字 10.5pt 算。
    pub default_size_pt: f32,
    pub empty_para: EmptyPara,
    pub grid: GridLayout,
    pub para_spacing: ParaSpacing,
    pub page_bottom: PageBottom,
    pub fixed_baseline: FixedBaseline,
    pub trailing_spaces: TrailingSpaces,
    pub hanging_punct: HangingPunct,
    pub justify: Justify,
    pub breaks: Breaks,
    pub page_break_before: PageBreakBefore,
    pub tabs: Tabs,
    pub hanging_indent: HangingIndent,
    pub overflow: Overflow,
    pub run_format: RunFormat,
    pub cascade: Cascade,
    pub flow: Flow,
    pub theme: Theme,
    pub char_class: CharClass,
}

/// `w:rFonts` 的主题字体（`w:asciiTheme`、`w:eastAsiaTheme` 等）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    /// 不认主题字体，只看字体名；各级样式逐个属性覆盖。
    Ignored,
    /// 对照 LibreOffice 实测：
    /// - 同一个 `w:rFonts` 里主题字体优先于字体名（`w:ascii` 与 `w:asciiTheme` 都写时用后者）；
    /// - 各级样式按「字体槽」整体覆盖：run 上写了 `w:ascii`，docDefaults 里的
    ///   `w:asciiTheme` 就不再起作用；
    /// - 东亚主题字体先按 `w:themeFontLang/@w:eastAsia` 找主题里对应文种的字体
    ///   （zh-CN → Hans、ja-JP → Jpan），找不到才用 `a:ea`。两个都有时 LibreOffice
    ///   用的也是文种字体。都没有时（没写 `w:themeFontLang`、`a:ea` 又是空的）
    ///   按没写字体处理。
    Resolved,
}

/// 每个字用西文字体还是东亚字体。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharClass {
    /// 按 Unicode script 分：汉字、假名、谚文与全角标点用东亚字体，其余有 script 的
    /// 用西文字体；Common 类（标点、符号、空格）跟随同一个 run 里的前一个字，
    /// run 开头的跟随后一个字。
    Legacy,
    /// 对照 LibreOffice 实测，按 Unicode 区段分：
    /// - 拉丁字母补充、拉丁扩展、希腊、西里尔（含 ×、· 这类符号）一律用西文字体；
    ///   中日韩各区段（含 ㎡、㈠）与全角形式用东亚字体；
    /// - 其余符号与标点（“”、—、…、①、℃、□、→）没有归属，跟随**整段**里前一个
    ///   有归属的字，跨 run 也一样；段首的算西文（LibreOffice 按界面语言定，en-US
    ///   下是西文；Word 没写 `w:hint` 时这些字符也用西文字体）。
    ///
    /// run 写了 `w:hint="eastAsia"` 时，这些没有归属的字，以及拉丁补充里的符号、
    /// 希腊与西里尔字母，改用东亚字体 —— 这是 Word 的规则（ECMA-376 §17.3.2.26）。
    /// LibreOffice 不认 `w:hint`，这一条没有参照可比。
    Blocks,
}

/// 段落之间的版流控制：`w:keepNext`、`w:keepLines`、`w:widowControl`、
/// `w:contextualSpacing`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// 都不认。
    Legacy,
    /// - 与下段同页：一串连续的 keepNext 段落加上下一段的第一行，当前页放不下、
    ///   又不在页首时，整串移到下一页；已经在页首就照排，超过一页的串自然被断开。
    /// - 段中不分页：整段放不下就整段挪走；比一页还高时照常拆。
    /// - 孤行控制：段落的第一行不单独留在页底，最后一行不单独落到下一页；
    ///   哪一级都没写 `w:widowControl` 时是开着的。
    /// - 同一样式的相邻段落之间不加段距（`contextualSpacing` 写在谁身上管谁的那一侧）。
    Word,
}

/// 样式层叠。见 `resolve` 模块。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cascade {
    /// 重写前的做法：设了 `w:pStyle` 仍叠加 Normal，段落标记的格式套到每个 run 上。
    Legacy,
    /// 按 ECMA-376：只走段落自己的样式链；段落标记的格式只管段落标记；
    /// 开关属性在段落样式与字符样式之间取异或。
    Spec,
}

/// 重写前没有实现的字符格式：字符间距、隐藏文字、`w:sym` 符号等。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunFormat {
    /// 都不认：隐藏文字照常显示，`w:sym` 丢掉，字符间距为 0。
    Legacy,
    Full,
}

/// 悬挂缩进（首行缩进为负）的段落，首行从哪里开始。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HangingIndent {
    /// 首行仍从左缩进处开始，可用宽度反而少了悬挂量。
    Legacy,
    /// 首行往左伸出悬挂量：从「左缩进 − 悬挂」开始，可用宽度相应多出这么多。
    /// 编号列表的序号就靠这个伸到左边去。
    Outdent,
}

/// 一整串不可断的内容（长网址、长数字）比一行还宽时怎么办。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overflow {
    /// 整串留在一行里，越过右边距。
    NextOpportunity,
    /// 在字符边界上断开，把这一行排满（至少放一个字）。
    CharBoundary,
}

/// `w:tab` 怎么排。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tabs {
    /// 每个制表符当一个全角空格。
    IdeographicSpace,
    /// 跳到下一个制表位：段落的自定义位优先，越过最后一个自定义位之后按
    /// `w:defaultTabStop` 的整数倍（从正文区左缘量起，缩进不影响）；右对齐、居中、
    /// 小数点位按后面那段文字的宽度往回让；前导符画成一串字符。
    ///
    /// 对照 LibreOffice 实测，默认位 21pt / 缺省 36pt、有首行缩进和左缩进、四种对齐、
    /// 越过最后一个自定义位，误差都在 0.1pt 以内。
    Stops,
}

/// `w:br` 的分页符、分栏符。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Breaks {
    /// 都当成换行。
    AsLineBreaks,
    /// 分页符之后的内容从下一页开始；单栏的节里分栏符也是这样。
    ///
    /// 对照 LibreOffice 实测：段中、段末、独占一段的分页符，后面的文字都在下一页的
    /// 正文顶；只有分页符的那一段，段落标记不会在下一页再占一行；分页符所在段的
    /// 段后距不带到新页上。
    Typed,
}

/// `w:pageBreakBefore` 什么时候不另起新页。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageBreakBefore {
    /// 只在文档第一页的页首。
    FirstPageTopOnly,
    /// 在任何一页的页首都不另起：分页符后面紧跟一个段前分页的段落，
    /// LibreOffice 不会多出一张空白页。
    AnyPageTop,
}

/// 两端对齐把一行剩下的空间分给哪些间隙。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Justify {
    /// 含半角空格的行不拉开（实际是左对齐）；其余的行平均分给每个字形之后。
    Legacy,
    /// 有半角空格的行只拉开空格；没有空格的行分给汉字前后的间隙，西文、数字内部不拉开。
    ///
    /// 对照 LibreOffice 实测的结构：中文夹着西文词的行，词内的字母间距一点没变，
    /// 余量都在汉字之间和中西文交界处；中西混排带空格的行，空格拿走了绝大部分余量。
    /// 各处具体分多少 LibreOffice 另有细节（交界处约为字间的两倍，汉字在有空格的行里
    /// 也分到一点），这里没有照搬。
    Gaps,
}

/// 行尾标点能不能伸出右边距（段落写了 `w:overflowPunct w:val="0"` 时一律不能）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HangingPunct {
    Never,
    /// 句读标点可以：它不算行宽，行里其余的字照常排满、对齐到边距，标点整个在边距外。
    ///
    /// 对照 LibreOffice 实测：36 个字正好排满一行时，后面的「，。、；：！？．」以及半角的
    /// 「, . ; : ! ?」都留在本行（37 字），「」）”」这类后引号、后括号不行（它们不能出现在
    /// 行首，只好带着前一个字换行，本行剩 35 字）；两端对齐时最后一个字的右缘在边距上，
    /// 标点紧挨着它伸出去。
    Punctuation,
}

/// 行尾的半角空格算不算行宽。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrailingSpaces {
    Counted,
    /// 不算：行尾空格悬挂在右边距外，放不放得下、居中和右对齐都只看可见的文字。
    ///
    /// 对照 LibreOffice 实测：右对齐的一段西文，首行可见文字的右缘正好贴着右边距，
    /// 行尾那个空格伸出边距约一个空格宽。
    Hang,
}

/// 固定行距、最小行距时，基线在行框里的位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixedBaseline {
    /// 与单倍行距一样摆，多出来（或少掉）的高度全在基线下方。
    Legacy,
    /// 对照 LibreOffice 实测（12pt 正文，固定 30pt / 10pt、最小 30pt / 10pt）：
    /// - 固定行距：基线在行高的 80% 处，多出或少掉的高度按 8:2 分在基线上下；
    /// - 最小行距把行撑高时，多出来的高度全在文字上方。
    ///
    /// 有行网格时 LibreOffice 干脆忽略固定行距与最小行距、照样吸附网格。这里没有跟：
    /// Word 在这种情况下怎么排没有实测过，而固定行距是作者明确写下的数值，以它为准。
    Measured,
}

/// 页底最后一行怎样才算放得下。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageBottom {
    /// 整个行框都要在页底以内。
    WholeLine,
    /// 行距倍数在文字下方多出来的空白可以越过页底，文字本身在页底以内就行。
    ///
    /// 对照 LibreOffice 实测：一段 12pt 长文，无网格 1.5 倍行距首页放 27 行（整个行框都算
    /// 只能放 26 行）；有网格 1.3 倍放 17 行（16 行）；单倍行距没有这部分空白，两种算法一样。
    TextOnly,
}

/// 上一段的段后距与下一段的段前距怎么合并。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParaSpacing {
    /// 相加。
    Sum,
    /// 文档没有设置 `w:doNotUseHTMLParagraphAutoSpacing` 时取两者较大值，设置了才相加。
    ///
    /// 对照 LibreOffice 实测：段后 4 + 段前 8 → 8，段后 18 + 段前 12 → 18，有无网格、
    /// 单倍或 1.3 倍行距都一样；加上这个兼容选项后变回相加。参照语料里「正文 → 小标题」
    /// 的间距因此比相加少 3pt，与这条吻合。
    ///
    /// OOXML 规范说这个选项只管「自动」段落间距；Word 对写明数值的间距是否也这样合并，
    /// 没有实测过。
    HtmlCollapse,
}

/// 行网格（`w:docGrid` 为 lines 等类型）下，行在格子里、网格在版心里怎么摆。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridLayout {
    /// 吸附多出来的空间全加在基线上方（文字贴着格子底边），网格从版心顶端开始。
    Legacy,
    /// 对照 LibreOffice 实测（10.5–32pt 七个字号、有无行距倍数）：
    /// - 文字在它所占的整格里上下居中：基线 = 上伸 + (整格高 − 字高) / 2。
    ///   字号相同的行之间两种摆法行距一样，所以只有标题这类大字号行才看得出差别。
    /// - 网格在版心里上下居中：版心放得下 ⌊版心高 / 格高⌋ 格，余下的对半分在上下；
    ///   正文从上面那一半之下开始，排到网格区底边为止。每页都如此，
    ///   不吸附网格的段落（`w:snapToGrid w:val="0"`）也一样。
    Centered,
}

/// 空段落（没有任何文字）的高度规则。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmptyPara {
    /// 首个 run 的字号（一个 run 都没有时 10.5pt）× 1.2，再按行距规则放大；
    /// 不检查放不放得下，可以越过页底。
    Legacy,
    /// 空段落是只有一个段落标记的一行：行高取标记的西文字体在标记字号下的自然行高，
    /// 之后与正文行一样吸附网格、套行距，也一样要放得下才放。
    ///
    /// 对照 LibreOffice 实测（无网格）：12pt / 16pt 标记分别是 13.8 / 18.4pt，
    /// 即 Liberation Serif 的自然行高；写了 `w:hint="eastAsia"` 也仍按西文字体；
    /// 1.5 倍行距是 20.7pt，与正文末行的规则一致。
    ///
    /// 有行网格时 LibreOffice 把空段落一律排成一格，连 16pt、固定行距也不例外，
    /// 而正文行是按字高吸附整格的。这里与正文行保持一致 —— Word 的网格也按字高吸附。
    MarkLine,
}

impl Calib {
    /// 现行规则。每条与 [`legacy`](Self::legacy) 不同的取值都要有对照实测的依据。
    pub fn current() -> Self {
        Self {
            default_size_pt: 10.0,
            empty_para: EmptyPara::MarkLine,
            grid: GridLayout::Centered,
            para_spacing: ParaSpacing::HtmlCollapse,
            page_bottom: PageBottom::TextOnly,
            fixed_baseline: FixedBaseline::Measured,
            trailing_spaces: TrailingSpaces::Hang,
            hanging_punct: HangingPunct::Punctuation,
            justify: Justify::Gaps,
            breaks: Breaks::Typed,
            page_break_before: PageBreakBefore::AnyPageTop,
            tabs: Tabs::Stops,
            hanging_indent: HangingIndent::Outdent,
            overflow: Overflow::CharBoundary,
            run_format: RunFormat::Full,
            cascade: Cascade::Spec,
            flow: Flow::Word,
            theme: Theme::Resolved,
            char_class: CharClass::Blocks,
        }
    }

    pub fn legacy() -> Self {
        Self {
            default_size_pt: 10.5,
            empty_para: EmptyPara::Legacy,
            grid: GridLayout::Legacy,
            para_spacing: ParaSpacing::Sum,
            page_bottom: PageBottom::WholeLine,
            fixed_baseline: FixedBaseline::Legacy,
            trailing_spaces: TrailingSpaces::Counted,
            hanging_punct: HangingPunct::Never,
            justify: Justify::Legacy,
            breaks: Breaks::AsLineBreaks,
            page_break_before: PageBreakBefore::FirstPageTopOnly,
            tabs: Tabs::IdeographicSpace,
            hanging_indent: HangingIndent::Legacy,
            overflow: Overflow::NextOpportunity,
            run_format: RunFormat::Legacy,
            cascade: Cascade::Legacy,
            flow: Flow::Legacy,
            theme: Theme::Ignored,
            char_class: CharClass::Legacy,
        }
    }
}
