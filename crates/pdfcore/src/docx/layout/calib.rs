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
        }
    }

    pub fn legacy() -> Self {
        Self {
            default_size_pt: 10.5,
            empty_para: EmptyPara::Legacy,
        }
    }
}
