//! pdftools 的核心逻辑。
//!
//! 这个 crate **不依赖任何 GUI**，所以整条流水线都能在无显示器的 CI 上跑测试。
//! 界面壳子在 `pdftools` crate 里。

pub mod docx;
pub mod error;
pub mod fonts;
pub mod fsio;
pub mod imaging;
pub mod ops;
pub mod pdf;
pub mod progress;
pub mod timestamp;

pub use error::{CoreError, Report, Result, Warning, WarningKind};
pub use progress::{Cancel, NoProgress, Progress, ProgressSink};
pub use timestamp::{DatedFile, TimeSource, Timestamp};
