//! 校准开关：与 Word 的行为可能有出入、要对着参照实测才能定的排版规则。
//!
//! 每个开关都要写明现在的取值依据。[`Calib::legacy`] 复刻重写前引擎的行为，
//! 新引擎用它排出来的结果必须与旧引擎逐坐标一致 —— 这是重写过程中「行为没变」的证据；
//! 改规则时新增一个取值，而不是改掉旧的。

#[derive(Debug, Clone)]
pub struct Calib {
    /// docDefaults、样式、直接格式都没写 `w:sz` 时用的字号。
    pub default_size_pt: f32,
    pub empty_para: EmptyPara,
}

/// 空段落（没有任何文字）的高度规则。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmptyPara {
    /// 首个 run 的字号（一个 run 都没有时 10.5pt）× 1.2，再按行距规则放大；
    /// 不检查放不放得下，可以越过页底。
    Legacy,
}

impl Calib {
    /// 现行规则。每条与 [`legacy`](Self::legacy) 不同的取值都要有对照实测的依据。
    pub fn current() -> Self {
        Self::legacy()
    }

    pub fn legacy() -> Self {
        Self {
            default_size_pt: 10.5,
            empty_para: EmptyPara::Legacy,
        }
    }
}
