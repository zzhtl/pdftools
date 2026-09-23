//! 后台任务：进度回传、取消、原子落盘。

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
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
            // panic 也要变成一个「失败」结果送回去：没有 Done 消息，界面会永远停在
            // 「运行中」—— 进度条一直转，取消也没用，用户只能强关程序。
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&sink)))
                .unwrap_or_else(|payload| Err(format!("内部错误：{}", panic_message(&*payload))));
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
        loop {
            let msg = match self.rx.try_recv() {
                Ok(msg) => msg,
                Err(TryRecvError::Empty) => break,
                // 工作线程已经退出却没送回结果。有了 catch_unwind 理论上不会发生，
                // 但真发生时按失败收尾，不能让界面一直挂在「运行中」。
                Err(TryRecvError::Disconnected) => {
                    if self.is_active() {
                        self.state = State::Failed("后台任务异常结束".into());
                    }
                    break;
                }
            };
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

/// 原子落盘，见 [`pdfcore::fsio::write_atomic`]。
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<(), String> {
    pdfcore::fsio::write_atomic(path, data).map_err(|e| e.to_string())
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "未知错误".into())
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 工作线程 panic 时，任务必须以「失败」收尾，而不是永远停在「运行中」——
    /// 那种界面上进度条一直转、取消按钮也没用，用户只能强关程序。
    #[test]
    fn a_panicking_worker_ends_as_failed() {
        let ctx = egui::Context::default();
        let mut job = Job::spawn(&ctx, |_sink| panic!("模拟的内部错误"));
        let deadline = Instant::now() + Duration::from_secs(10);
        while job.is_active() && Instant::now() < deadline {
            job.pump();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            matches!(job.state, State::Failed(_)),
            "工作线程 panic 后任务状态是 {:?}",
            job.state
        );
    }
}
