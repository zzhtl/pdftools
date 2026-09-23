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
        }
    }
}
