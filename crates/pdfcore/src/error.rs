//! 核心错误类型与「诚实失败」的载体。

use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, CoreError>;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("读写文件失败 {path}：{source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("图像处理失败：{0}")]
    Image(String),

    #[error("字体问题：{0}")]
    Font(String),

    #[error("PDF 处理失败：{0}")]
    Pdf(String),

    #[error("docx 解析失败：{0}")]
    Docx(String),

    #[error("{0}")]
    Unsupported(String),

    #[error("已取消")]
    Cancelled,
}

impl CoreError {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

/// 一条「我们没能完整处理」的记录。
///
/// 这是整个项目的诚实性契约：任何被跳过、降级、替换的内容都必须在这里留痕，
/// 并在界面上呈现给用户。静默丢弃是不允许的 —— 用户会以为转全了。
#[derive(Debug, Clone)]
pub struct Warning {
    /// 出现在第几页（从 1 开始）。不适用时为 None。
    pub page: Option<usize>,
    pub kind: WarningKind,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WarningKind {
    /// 输入里有本版本没能完整还原的内容：docx 里不渲染的元素、多页 TIFF 的其余页……
    UnsupportedElement,
    /// 请求的字体不可用，换成了别的
    FontSubstituted,
    /// 图片原样保留（重编码后反而更大）
    ImageKeptOriginal,
    /// 拿不到 EXIF 拍摄时间，退而使用文件修改时间。
    /// 在取证场景里这个区别很要紧，所以单独成类而不是混进「失败」里。
    CaptureTimeMissing,
    /// 单个条目失败，但整体继续
    ItemFailed,
}

impl Warning {
    pub fn new(kind: WarningKind, detail: impl Into<String>) -> Self {
        Self {
            page: None,
            kind,
            detail: detail.into(),
        }
    }

    pub fn at_page(mut self, page: usize) -> Self {
        self.page = Some(page);
        self
    }
}

/// 一次转换的完整产物：结果 + 所有未能完整处理之处。
#[derive(Debug)]
pub struct Report<T> {
    pub value: T,
    pub warnings: Vec<Warning>,
}

impl<T> Report<T> {
    pub fn new(value: T) -> Self {
        Self {
            value,
            warnings: Vec::new(),
        }
    }

    pub fn with(value: T, warnings: Vec<Warning>) -> Self {
        Self { value, warnings }
    }
}
