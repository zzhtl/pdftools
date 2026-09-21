//! 进度上报与取消。
//!
//! 刻意不引入 tokio：这里每一步都是 CPU 密集（JPEG 编解码、缩放、子集化、排版）
//! 或短暂的阻塞 IO，异步运行时帮不上忙，只会让错误处理和取消变复杂，
//! 而且会诱使后来者在排版引擎里写 `async fn`。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::error::Warning;

/// 取消标志。UI 线程置位，工作线程在检查点读取。
#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Clone)]
pub enum Progress {
    Started {
        total: usize,
    },
    /// 完成了第 done 个（共 total 个），label 是当前条目的显示名。
    Item {
        done: usize,
        total: usize,
        label: String,
    },
    Warn(Warning),
}

/// 核心层向外汇报进度的出口。核心层不认识 egui，只认识这个 trait。
pub trait ProgressSink: Send + Sync {
    fn emit(&self, progress: Progress);
    fn is_cancelled(&self) -> bool;
}

/// 不需要进度时用它。测试里最常用。
pub struct NoProgress;

impl ProgressSink for NoProgress {
    fn emit(&self, _progress: Progress) {}
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// 检查取消并提前返回。放在粗粒度的边界上 —— 每张图、每页、每份文档，
/// 而不是每个字形。200ms 的取消延迟用户感觉不到，满屏的检查点却会让代码没法读。
#[macro_export]
macro_rules! bail_if_cancelled {
    ($sink:expr) => {
        if $sink.is_cancelled() {
            return Err($crate::error::CoreError::Cancelled);
        }
    };
}
