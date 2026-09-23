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
    pub auto_space: AutoSpace,
    pub decor: Decor,
    pub list_numbers: ListNumbers,
    pub sections: Sections,
    pub header_footer: HeaderFooter,
    pub kerning: Kerning,
    pub line_gap: LineGap,
    pub char_grid: CharGrid,
    pub tables: Tables,
}

/// 表格。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tables {
    /// 不画：一句说明加上表格里的文字，并给出警告。
    Placeholder,
    /// 画出来。对照 LibreOffice 实测：
    /// - 列宽取 `w:tblGrid`，是相邻两条列边界之间的距离；竖框线骑在列边界上，文字
    ///   从列边界让出左右边距（默认各 5.4pt，上下为 0）；
    /// - 左对齐时，Word 2013 起（`compatibilityMode` ≥ 15，没写也按 15）左框线的外沿
    ///   在版心左边加 `w:tblInd` 处；Word 2010 让首格文字对齐正文，表格往左让出左边距。
    ///   居中的表格按列宽居中，右对齐的最后一条列边界落在版心右边，都不看 `w:tblInd`；
    ///   嵌套的表格从所在单元格的文字左边量，两种兼容模式都按 Word 2013 的规则；
    /// - 没写网格、单元格也没写宽度的列平分剩下的宽度；
    /// - 行首、行尾空着的列（`w:gridBefore`、`w:gridAfter`）：上下两行有一行在这一列
    ///   有格就画横线，空着的地方没有竖线；
    /// - 竖直方向依次是上框线、第一行、行间框线……下框线，各占自己的线宽；行高的
    ///   最小值、固定值都含本行的上框线；
    /// - 底纹从上框线的外沿铺到下一条框线，横向从左框线中线到右框线中线；
    /// - 纵向合并的格内容比几行加起来还高时，撑高最后一行；
    /// - 跨页：行放不下就在页底拆开（至少放得下一行字才拆，不然整行挪走），每页的一截
    ///   在最后一行下面收口，下一页按那一行自己的上边开头；写了 `w:cantSplit` 的行、
    ///   固定行高的行整行挪到下一页；续页先重复开头的标题行（`w:tblHeader`）；纵向
    ///   合并的几行照样在行与行之间分页；与下段同页的段落带着表格的第一行走；
    /// - 单元格里不做孤行控制（写了 `w:widowControl` 也一样），拆开的行不做竖直对齐；
    /// - 单元格里行尾的标点不伸出去：放不下就带着前一个字换行；
    /// - 表格样式（`w:tblStyle`，没写用默认的表格样式）给框线、边距、底纹，表格自己的
    ///   `w:tblPr` 优先；单元格里的段落按 docDefaults → 表格样式 → 段落样式 → 直接格式
    ///   层叠（Normal 写了段距时表格样式的段距不起作用）；首行、末行、首列、末列、隔行、
    ///   隔列、四角的条件格式按 `w:tblLook` 生效，排在后面的优先，框线按单元格在那一块里
    ///   的位置取。两处按 Word 的规则、与 LibreOffice 不同：表格样式的段落格式沿 basedOn
    ///   继承（LibreOffice 只取样式自己写的），没写 `w:tblStyle` 的表格用默认表格样式
    ///   （LibreOffice 不用）。
    Drawn,
}

/// 字符网格（`w:docGrid w:type="linesAndChars"`，公文按它排成每行 28 字）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharGrid {
    /// 不认，汉字按自己的字宽排。
    Ignored,
    /// 按 Word 的规则：格宽 = Normal 样式的字号 + `w:charSpace`/4096 磅，汉字与全角
    /// 标点每个占整格（字号比格宽略大时仍占一格，明显更大时占两格），西文按原宽。
    /// 公文的三号字、`w:charSpace="-849"` 算出来格宽 15.79pt，版心 156mm 正好 28 格。
    ///
    /// LibreOffice 在这里不能当参照：它把比格宽稍大的字放进两格，公文每行只排 14 字。
    Cells,
}

/// 字体的行间距（hhea 的 lineGap，Liberation Serif 每 em 约 0.04）放在字的上面还是下面。
/// 行高都算上它，只是基线的位置不同。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineGap {
    /// 放在下面：基线离行顶一个上伸。
    Below,
    /// 放在上面：基线离行顶「上伸 + 行间距」。对照 LibreOffice 实测：只有西文的行
    /// 基线比放在下面低一个行间距（Liberation Serif 12pt 低 0.52pt、24pt 低 1.03pt，
    /// Liberation Sans 12pt 低 0.40pt），多行段落每行都如此，有网格时也一样；
    /// 夹着汉字的行由中文字体的上伸决定，不受影响。
    Above,
}

/// 字距调整（字体里的 kern 对，如 AV、To、11）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kerning {
    /// 一律调整。
    Always,
    /// run 写了 `w:kern`、字号又达到它给的阈值时才调整。对照 LibreOffice：没写
    /// `w:kern` 时不调整，写了就调整，与我们调整的结果一致；但 LibreOffice 不看阈值，
    /// 字号不到阈值也调整，这一点按 Word 的规则。
    Word,
}

/// 页眉页脚与页码域。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderFooter {
    /// 不画，汇总成一条警告；域照原样显示缓存的结果。
    Warned,
    /// 对照 LibreOffice 实测：
    /// - 页眉顶端在离纸张上边 `w:header` 处，页脚最后一行的底边在离纸张下边
    ///   `w:footer` 处；页眉页脚的行不吸附行网格；
    /// - 页眉的底边低过上边距时，正文紧接在页眉底下开始；页脚的顶边高过下边距时，
    ///   正文排到页脚顶为止；
    /// - 首页（本节写了 `w:titlePg`）、偶数页（`w:evenAndOddHeaders`）用各自的页眉
    ///   页脚，没有定义就是空的，不退回默认的那个；某类在本节没写时沿用上一节的；
    /// - PAGE、NUMPAGES、SECTIONPAGES 代入真实的数。PAGE 按本节的页码格式写。
    ///   NUMPAGES 按阿拉伯数字写 —— LibreOffice 让它也跟随页码格式（「共 IV 页」），
    ///   这里按 Word 的做法。
    Drawn,
}

/// 多节文档（段落里的 `w:sectPr`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sections {
    /// 只用最后一节（body 末尾的 `w:sectPr`）的设置排全文。
    Last,
    /// 每节用自己的纸张、边距、网格，按 `w:type` 开始：
    /// - 另起一页；奇数页、偶数页起时，页码奇偶不对就空出一页（规范如此，
    ///   LibreOffice 实测不空页）；
    /// - 连续：不换页，本节的左右边距从分节处起生效，其余设置等下一页；纸张大小、
    ///   方向变了则照样换页。LibreOffice 实测在连续分节之后一直沿用上一节的边距，
    ///   连之后新开的页也不改。
    /// - `w:pgNumType/@w:start` 让本节第一页的页码重新起头。
    Each,
}

/// 自动编号（`w:numPr`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListNumbers {
    /// 不生成编号文字，编号级别的缩进也不用；汇总成一条警告。
    Dropped,
    /// 生成编号文字（见 `docx::numbering`），编号级别的缩进、制表位参与层叠。
    /// 对照 LibreOffice 实测：编号来自段落样式时，样式里的缩进优先于编号级别的；
    /// 编号直接写在段落上时，编号级别的优先于样式的。
    Rendered,
}

/// 段落边框（`w:pBdr`）与段落底纹（`w:shd`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decor {
    /// 不画，也不占位置。
    Ignored,
    /// 对照 LibreOffice 实测：
    /// - 上、下边框各占「线厚 + `w:space`」的高度：框顶紧贴段前距之下，隔着距离才是
    ///   第一行；下边框同理。左右边框画在缩进之外（再往外隔 `w:space`），不挤文字；
    ///   有左右边框时上下边框横向伸到左右边框的外沿。双线是两条线中间隔一条线宽。
    /// - 相邻段落的边框、底纹、缩进都相同时合成一个框：中间没有上下边框，段距算在
    ///   框里；写了 `w:between` 时中间画一条线，线上下各隔它的 `w:space`。
    /// - 跨页时框在断开处各自收口：前一页照样画下边框（放不放得下要算上它），
    ///   后一页重新画上边框。
    /// - 底纹铺满整个框（含边框与距离）；没有边框时就是各行的范围，不占高度。
    Boxes,
}

/// 中西文之间的自动间距（`w:autoSpaceDE` / `w:autoSpaceDN`，0.2em）加在哪里。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoSpace {
    /// 相邻两片一个用东亚字体、一个用西文字体，边界上又没有空白，就加。
    Legacy,
    /// 只加在汉字（假名、谚文）与西文字母、数字之间。对照 LibreOffice 实测：
    /// 「中a」「中1」「中α」「①中」加；中文标点两侧（「，a」「：1」「a。」「「a」）、
    /// 西文标点与符号两侧（「中×」「中%」「a“中」「中(」）、「1㎡」都不加。
    Letters,
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
            auto_space: AutoSpace::Letters,
            decor: Decor::Boxes,
            list_numbers: ListNumbers::Rendered,
            sections: Sections::Each,
            header_footer: HeaderFooter::Drawn,
            kerning: Kerning::Word,
            line_gap: LineGap::Above,
            char_grid: CharGrid::Cells,
            tables: Tables::Drawn,
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
            auto_space: AutoSpace::Legacy,
            decor: Decor::Ignored,
            list_numbers: ListNumbers::Dropped,
            sections: Sections::Last,
            header_footer: HeaderFooter::Warned,
            kerning: Kerning::Always,
            line_gap: LineGap::Below,
            char_grid: CharGrid::Ignored,
            tables: Tables::Placeholder,
        }
    }
}
