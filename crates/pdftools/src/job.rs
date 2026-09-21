//! 后台任务：进度回传、取消、原子落盘。

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use pdfcore::{Cancel, Progress, ProgressSink, Warning};

pub enum Msg {
    Progress(Progress),
    Done(Result<Done, String>),
}

pub struct Done {
    /// 给用户看的一句话结果。
    pub summary: String,
    /// 产物落地位置，用于「打开所在文件夹」。
    pub output: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Running,
    Cancelling,
    Finished(String),
    Failed(String),
}

pub struct Job {
    rx: Receiver<Msg>,
    cancel: Cancel,
    handle: Option<JoinHandle<()>>,
    pub state: State,
    pub done: usize,
    pub total: usize,
    pub label: String,
    pub warnings: Vec<Warning>,
    pub output: Option<PathBuf>,
}

impl Job {
    /// 启动一个后台任务。`work` 在工作线程上跑，通过 sink 汇报进度。
    pub fn spawn<F>(ctx: &egui::Context, work: F) -> Self
    where
        F: FnOnce(&dyn ProgressSink) -> Result<Done, String> + Send + 'static,
    {
        let (tx, rx) = channel();
        let cancel = Cancel::new();

        let sink = UiSink {
            tx: tx.clone(),
            ctx: ctx.clone(),
            cancel: cancel.clone(),
            last_emit: Mutex::new(Instant::now() - Duration::from_secs(1)),
        };

        let ctx2 = ctx.clone();
        let handle = std::thread::spawn(move || {
            let result = work(&sink);
            let _ = tx.send(Msg::Done(result));
            // 必须唤醒一次，否则窗口会停在最后一帧，看上去像卡死。
            ctx2.request_repaint();
        });

        Self {
            rx,
            cancel,
            handle: Some(handle),
            state: State::Running,
            done: 0,
            total: 0,
            label: String::new(),
            warnings: Vec::new(),
            output: None,
        }
    }

    pub fn request_cancel(&mut self) {
        self.cancel.cancel();
        if self.state == State::Running {
            self.state = State::Cancelling;
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(self.state, State::Running | State::Cancelling)
    }

    /// 排空通道。**必须在 `App::logic` 里调用，不能放在 `App::ui`。**
    ///
    /// eframe 0.36 的文档写得很清楚：窗口被最小化时不跑 egui pass，因而不调 `ui`，
    /// 但只要有人调过 `request_repaint` 就仍会调 `logic`。
    /// 把排空放在 `ui` 里，用户一最小化，通道就会无限堆积、任务看上去像冻住了。
    pub fn pump(&mut self) {
        for msg in self.rx.try_iter() {
            match msg {
                Msg::Progress(Progress::Started { total }) => {
                    self.total = total;
                    self.done = 0;
                }
                Msg::Progress(Progress::Item { done, total, label }) => {
                    self.done = done;
                    self.total = total;
                    self.label = label;
                }
                Msg::Progress(Progress::Warn(w)) => self.warnings.push(w),
                Msg::Done(Ok(d)) => {
                    self.output = d.output;
                    self.state = State::Finished(d.summary);
                }
                Msg::Done(Err(e)) => self.state = State::Failed(e),
            }
        }
        // 收尾后 join 一下，让工作线程的 panic 能浮出来而不是被静默吞掉。
        if !self.is_active() {
            if let Some(h) = self.handle.take() {
                if h.join().is_err() {
                    self.state = State::Failed("后台任务异常退出".into());
                }
            }
        }
    }

    pub fn fraction(&self) -> Option<f32> {
        (self.total > 0).then(|| self.done as f32 / self.total as f32)
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // 窗口关了就别让工作线程继续写文件。
        self.cancel.cancel();
    }
}

struct UiSink {
    tx: Sender<Msg>,
    ctx: egui::Context,
    cancel: Cancel,
    last_emit: Mutex<Instant>,
}

impl ProgressSink for UiSink {
    fn emit(&self, progress: Progress) {
        // 警告一条都不能丢；进度则限流，否则一批小文件会以每秒上万次的频率
        // 触发重绘，反而把界面拖垮。
        let is_warn = matches!(progress, Progress::Warn(_));
        if !is_warn {
            let mut last = self.last_emit.lock().expect("进度时间戳锁");
            if last.elapsed() < Duration::from_millis(30) {
                return;
            }
            *last = Instant::now();
        }
        let _ = self.tx.send(Msg::Progress(progress));
        self.ctx.request_repaint();
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }
}

/// 原子落盘：先写 `.part`，成功后再改名。
///
/// 进程被杀、磁盘写满、用户强退 —— 任何一种情况都不该在用户选定的路径上
/// 留下一个残缺的 PDF，那比什么都没有更糟，因为用户会以为它是好的。
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension(format!(
        "{}.part",
        path.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
    ));
    std::fs::write(&tmp, data).map_err(|e| format!("写入 {} 失败：{e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("重命名到 {} 失败：{e}", path.display())
    })
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i + 1 < UNITS.len() {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}
