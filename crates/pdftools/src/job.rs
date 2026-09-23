//! 后台任务：进度、警告、逐个文件的结果，取消，原子落盘。
//!
//! 进度只留最新的一份，界面每帧去读：中间的状态不必一条条送过来。旧做法按 30ms
//! 丢弃进度消息，最后一条恰好被丢时进度条就停在半路。警告与逐个文件的结果则一条
//! 都不能丢，走通道按顺序送。

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use pdfcore::{Cancel, CoreError, Progress, ProgressSink, Warning};

/// 正常完成时给用户看的结果。
pub struct Done {
    /// 一句话的结果。
    pub summary: String,
    /// 产物，用于「在文件夹中显示」。批量任务的产物在逐个文件的结果里。
    pub output: Option<PathBuf>,
}

/// 任务没有正常完成：取消了，或者失败了。
#[derive(Debug)]
pub enum Stop {
    Cancelled,
    Failed(String),
}

impl From<String> for Stop {
    fn from(msg: String) -> Self {
        Stop::Failed(msg)
    }
}

impl From<&str> for Stop {
    fn from(msg: &str) -> Self {
        Stop::Failed(msg.to_string())
    }
}

impl From<CoreError> for Stop {
    fn from(e: CoreError) -> Self {
        match e {
            CoreError::Cancelled => Stop::Cancelled,
            e => Stop::Failed(e.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Running,
    Cancelling,
    Finished(String),
    /// 用户取消了。不是失败：已经做完的那些照样在结果里。
    Cancelled,
    Failed(String),
}

/// 批量任务里一个文件的结果。
#[derive(Debug, Clone)]
pub struct ItemResult {
    pub input: PathBuf,
    pub output: Option<PathBuf>,
    /// 失败的原因；成功时是 None。
    pub error: Option<String>,
    /// 成功时的一句说明（「12 页，1.2 MB」）。
    pub detail: String,
}

enum Msg {
    Warn(Warning),
    Item(ItemResult),
    Done(Result<Done, Stop>),
}

/// 最新的进度。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Latest {
    /// 0–1；不知道总量时是 None（进度条来回滚动）。
    pub fraction: Option<f32>,
    pub text: String,
}

pub struct Job {
    rx: Receiver<Msg>,
    latest: Arc<Mutex<Latest>>,
    cancel: Cancel,
    handle: Option<JoinHandle<()>>,
    pub state: State,
    pub progress: Latest,
    pub warnings: Vec<Warning>,
    pub items: Vec<ItemResult>,
    /// 批量任务一共要做几个文件（0 表示不是批量任务）。
    pub planned: usize,
    pub output: Option<PathBuf>,
}

impl Job {
    /// 启动一个后台任务。`work` 在工作线程上跑，通过 [`Worker`] 汇报进度与结果。
    /// `planned`：批量任务要处理几个文件，单个产物的任务传 0。
    pub fn spawn<F>(ctx: &egui::Context, planned: usize, work: F) -> Self
    where
        F: FnOnce(&Worker) -> Result<Done, Stop> + Send + 'static,
    {
        let (tx, rx) = channel();
        let cancel = Cancel::new();
        let latest = Arc::new(Mutex::new(Latest::default()));
        let worker = Worker {
            tx: tx.clone(),
            latest: latest.clone(),
            ctx: ctx.clone(),
            cancel: cancel.clone(),
        };

        let ctx2 = ctx.clone();
        let handle = std::thread::spawn(move || {
            // panic 也要变成一个「失败」结果送回去：没有 Done 消息，界面会永远停在
            // 「运行中」—— 进度条一直转，取消也没用，用户只能强关程序。
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&worker)))
                .unwrap_or_else(|payload| {
                    Err(Stop::Failed(format!(
                        "内部错误：{}",
                        panic_message(&*payload)
                    )))
                });
            let _ = tx.send(Msg::Done(result));
            // 必须唤醒一次，否则窗口会停在最后一帧，看上去像卡死。
            ctx2.request_repaint();
        });

        Self {
            rx,
            latest,
            cancel,
            handle: Some(handle),
            state: State::Running,
            progress: Latest::default(),
            warnings: Vec::new(),
            items: Vec::new(),
            planned,
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

    /// 取回工作线程送来的东西。**必须在 `App::logic` 里调用，不能放在 `App::ui`。**
    ///
    /// eframe 0.36 的文档写得很清楚：窗口被最小化时不跑 egui pass，因而不调 `ui`，
    /// 但只要有人调过 `request_repaint` 就仍会调 `logic`。
    /// 把排空放在 `ui` 里，用户一最小化，通道就会无限堆积、任务看上去像冻住了。
    pub fn pump(&mut self) {
        if let Ok(latest) = self.latest.lock() {
            if *latest != self.progress {
                self.progress = latest.clone();
            }
        }
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
                Msg::Warn(w) => self.warnings.push(w),
                Msg::Item(r) => self.items.push(r),
                Msg::Done(Ok(d)) => {
                    self.output = d.output;
                    self.state = State::Finished(d.summary);
                }
                Msg::Done(Err(Stop::Cancelled)) => self.state = State::Cancelled,
                Msg::Done(Err(Stop::Failed(e))) => self.state = State::Failed(e),
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

    /// 做成了几个文件（批量任务）。
    pub fn succeeded(&self) -> usize {
        self.items.iter().filter(|r| r.error.is_none()).count()
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // 窗口关了就别让工作线程继续写文件。
        self.cancel.cancel();
    }
}

/// 工作线程这一侧：汇报进度、警告与逐个文件的结果。也可以直接当核心层的
/// [`ProgressSink`] 用。
pub struct Worker {
    tx: Sender<Msg>,
    latest: Arc<Mutex<Latest>>,
    ctx: egui::Context,
    cancel: Cancel,
}

impl Worker {
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// 更新进度。只留最新的一份。重绘请求的是「30ms 之内」：连着来的更新合成一次
    /// 重绘，界面不会被拖垮；最后一条也一定画得出来，哪怕紧接着是一个很长的步骤。
    pub fn set(&self, fraction: Option<f32>, text: impl Into<String>) {
        if let Ok(mut latest) = self.latest.lock() {
            *latest = Latest {
                fraction,
                text: text.into(),
            };
        }
        self.ctx.request_repaint_after(Duration::from_millis(30));
    }

    pub fn warn(&self, w: Warning) {
        let _ = self.tx.send(Msg::Warn(w));
        self.ctx.request_repaint();
    }

    pub fn item(&self, r: ItemResult) {
        let _ = self.tx.send(Msg::Item(r));
        self.ctx.request_repaint();
    }

    /// 批量任务里第 `index` 个（共 `count` 个）文件 `name` 的进度汇报口：核心层报的
    /// 进度折算进整批，警告前面加上文件名。
    pub fn scoped<'a>(&'a self, index: usize, count: usize, name: &'a str) -> Scoped<'a> {
        Scoped {
            worker: self,
            index,
            count,
            name,
        }
    }
}

impl ProgressSink for Worker {
    fn emit(&self, progress: Progress) {
        match progress {
            Progress::Started { total } => self.set((total > 0).then_some(0.0), ""),
            Progress::Item { done, total, label } => self.set(
                Some(fold(0, 1, done, total)),
                format!("{done}/{total}  {label}"),
            ),
            Progress::Warn(w) => self.warn(w),
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }
}

/// 见 [`Worker::scoped`]。
pub struct Scoped<'a> {
    worker: &'a Worker,
    index: usize,
    count: usize,
    name: &'a str,
}

impl Scoped<'_> {
    fn text(&self, step: Option<&str>) -> String {
        let head = format!("{}/{}  {}", self.index + 1, self.count, self.name);
        match step {
            Some(s) if !s.is_empty() => format!("{head} · {s}"),
            _ => head,
        }
    }
}

impl ProgressSink for Scoped<'_> {
    fn emit(&self, progress: Progress) {
        match progress {
            Progress::Started { .. } => self
                .worker
                .set(Some(fold(self.index, self.count, 0, 1)), self.text(None)),
            Progress::Item { done, total, label } => self.worker.set(
                Some(fold(self.index, self.count, done, total)),
                self.text(Some(&label)),
            ),
            Progress::Warn(mut w) => {
                w.detail = format!("{}：{}", self.name, w.detail);
                self.worker.warn(w);
            }
        }
    }

    fn is_cancelled(&self) -> bool {
        self.worker.is_cancelled()
    }
}

/// 第 `index` 个（共 `count` 个）条目做到了 `done / total`：整批做到了多少（0–1）。
pub fn fold(index: usize, count: usize, done: usize, total: usize) -> f32 {
    let inner = if total == 0 {
        0.0
    } else {
        (done as f32 / total as f32).min(1.0)
    };
    ((index as f32 + inner) / count.max(1) as f32).min(1.0)
}

/// 批量处理的汇总。
pub struct Batch {
    pub succeeded: usize,
    pub failed: usize,
    pub last_output: Option<PathBuf>,
}

/// 逐个处理 `files`：`each` 返回产物与一句说明；失败的记下原因接着做后面的，一个
/// 坏文件不该让整批作废。取消时停下（已经做完的照样算数）；全部失败时整个任务算失败，
/// 原因取第一个。
pub fn run_batch(
    worker: &Worker,
    files: &[PathBuf],
    mut each: impl FnMut(&Path, &Scoped) -> Result<(PathBuf, String), Stop>,
) -> Result<Batch, Stop> {
    let mut batch = Batch {
        succeeded: 0,
        failed: 0,
        last_output: None,
    };
    let mut first_error = None;
    for (i, path) in files.iter().enumerate() {
        if worker.is_cancelled() {
            return Err(Stop::Cancelled);
        }
        let name = file_label(path);
        let scoped = worker.scoped(i, files.len(), &name);
        scoped.emit(Progress::Started { total: 0 });
        let (output, error, detail) = match each(path, &scoped) {
            Ok((out, detail)) => (Some(out), None, detail),
            Err(Stop::Cancelled) => return Err(Stop::Cancelled),
            Err(Stop::Failed(e)) => (None, Some(e), String::new()),
        };
        match &error {
            None => {
                batch.succeeded += 1;
                batch.last_output = output.clone();
            }
            Some(e) => {
                batch.failed += 1;
                first_error.get_or_insert_with(|| format!("{name}：{e}"));
            }
        }
        worker.item(ItemResult {
            input: path.clone(),
            output,
            error,
            detail,
        });
    }
    if batch.succeeded == 0 {
        if let Some(e) = first_error {
            return Err(Stop::Failed(e));
        }
    }
    Ok(batch)
}

/// 同 [`run_batch`]，但几个文件同时做（最多 4 个线程）。`each` 拿到文件的序号、路径
/// 与汇报口，会在几个线程上同时调用。
///
/// 文件内部的步骤不报进度 —— 几个文件的步骤交错着来，进度条会来回跳 —— 整批进度按
/// 做完的个数算；逐个文件的结果按做完的先后报。全部失败时原因取排在最前面的那个，
/// 与线程谁先谁后无关。
pub fn run_batch_parallel(
    worker: &Worker,
    files: &[PathBuf],
    each: impl Fn(usize, &Path, &dyn ProgressSink) -> Result<(PathBuf, String), Stop> + Sync,
) -> Result<Batch, Stop> {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let total = files.len();
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .clamp(1, 4)
        .min(total.max(1));
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let batch = Mutex::new(Batch {
        succeeded: 0,
        failed: 0,
        last_output: None,
    });
    let first_error: Mutex<Option<(usize, String)>> = Mutex::new(None);
    worker.set(Some(0.0), format!("0/{total}"));

    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= total || worker.is_cancelled() {
                    return;
                }
                let path = &files[i];
                let name = file_label(path);
                let sink = Quiet {
                    worker,
                    name: &name,
                };
                let (output, error, detail) = match each(i, path, &sink) {
                    Ok((out, detail)) => (Some(out), None, detail),
                    // 取消由外面统一收尾；这一个不算成也不算败。
                    Err(Stop::Cancelled) => return,
                    Err(Stop::Failed(e)) => (None, Some(e), String::new()),
                };
                {
                    let mut b = batch.lock().expect("批量汇总锁");
                    match &error {
                        None => {
                            b.succeeded += 1;
                            b.last_output = output.clone();
                        }
                        Some(e) => {
                            b.failed += 1;
                            let mut first = first_error.lock().expect("批量汇总锁");
                            if first.as_ref().is_none_or(|(k, _)| i < *k) {
                                *first = Some((i, format!("{name}：{e}")));
                            }
                        }
                    }
                }
                worker.item(ItemResult {
                    input: path.clone(),
                    output,
                    error,
                    detail,
                });
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                worker.set(Some(n as f32 / total as f32), format!("已完成 {n}/{total}"));
            });
        }
    });

    if worker.is_cancelled() {
        return Err(Stop::Cancelled);
    }
    let batch = batch.into_inner().expect("批量汇总锁");
    if batch.succeeded == 0 {
        if let Some((_, e)) = first_error.into_inner().expect("批量汇总锁") {
            return Err(Stop::Failed(e));
        }
    }
    Ok(batch)
}

/// 并行批量里单个文件的汇报口：只转发警告（前面加文件名）。
struct Quiet<'a> {
    worker: &'a Worker,
    name: &'a str,
}

impl ProgressSink for Quiet<'_> {
    fn emit(&self, progress: Progress) {
        if let Progress::Warn(mut w) = progress {
            w.detail = format!("{}：{}", self.name, w.detail);
            self.worker.warn(w);
        }
    }

    fn is_cancelled(&self) -> bool {
        self.worker.is_cancelled()
    }
}

/// 原子落盘，见 [`pdfcore::fsio::write_atomic`]。
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<(), String> {
    pdfcore::fsio::write_atomic(path, data).map_err(|e| e.to_string())
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
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
    use std::time::Instant;

    fn wait(job: &mut Job) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while job.is_active() && Instant::now() < deadline {
            job.pump();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// 工作线程 panic 时，任务必须以「失败」收尾，而不是永远停在「运行中」——
    /// 那种界面上进度条一直转、取消按钮也没用，用户只能强关程序。
    #[test]
    fn a_panicking_worker_ends_as_failed() {
        let ctx = egui::Context::default();
        let mut job = Job::spawn(&ctx, 0, |_w| panic!("模拟的内部错误"));
        wait(&mut job);
        assert!(
            matches!(job.state, State::Failed(_)),
            "工作线程 panic 后任务状态是 {:?}",
            job.state
        );
    }

    /// 批量里第 i 个文件做到一半：整批的进度在第 i 格的中间。
    #[test]
    fn progress_folds_into_the_batch() {
        assert_eq!(fold(0, 4, 0, 10), 0.0);
        assert_eq!(fold(1, 4, 5, 10), 0.375);
        assert_eq!(fold(3, 4, 10, 10), 1.0);
        // 不知道总量、报多了、没有条目，都不越界。
        assert_eq!(fold(2, 4, 3, 0), 0.5);
        assert_eq!(fold(0, 2, 7, 5), 0.5);
        assert_eq!(fold(0, 0, 0, 0), 0.0);
    }

    /// 最后一条进度不会丢：限流只管重绘，不管状态。
    #[test]
    fn the_last_progress_is_never_dropped() {
        let ctx = egui::Context::default();
        let mut job = Job::spawn(&ctx, 0, |w| {
            for i in 0..=1000 {
                w.emit(Progress::Item {
                    done: i,
                    total: 1000,
                    label: format!("第 {i} 个"),
                });
            }
            std::thread::sleep(Duration::from_millis(50));
            Ok(Done {
                summary: "好了".into(),
                output: None,
            })
        });
        wait(&mut job);
        assert_eq!(job.progress.fraction, Some(1.0));
        assert!(
            job.progress.text.ends_with("第 1000 个"),
            "{:?}",
            job.progress
        );
    }

    /// 被限流挡掉的那次重绘要补上：紧接着开始一个长步骤时，界面不能一直停在上一条
    /// 进度上。
    #[test]
    fn a_throttled_update_is_still_painted() {
        let ctx = egui::Context::default();
        let (step_tx, step_rx) = channel::<()>();
        let (go_tx, go_rx) = channel::<()>();
        let mut job = Job::spawn(&ctx, 0, move |w| {
            w.set(Some(0.1), "第一步");
            step_tx.send(()).unwrap();
            go_rx.recv().unwrap();
            // 离上一次重绘不到 30ms。
            w.set(Some(0.2), "第二步：要很久");
            step_tx.send(()).unwrap();
            go_rx.recv().unwrap();
            Ok(Done {
                summary: String::new(),
                output: None,
            })
        });
        step_rx.recv().unwrap();
        // 界面把第一步画完，手上没有待画的了。
        for _ in 0..10 {
            if !ctx.has_requested_repaint() {
                break;
            }
            // 没有真的去画：纹理的增量丢掉就好。
            ctx.run_ui(Default::default(), |_| {})
                .textures_delta
                .clear();
        }
        assert!(!ctx.has_requested_repaint());
        go_tx.send(()).unwrap();
        step_rx.recv().unwrap();
        assert!(ctx.has_requested_repaint(), "第二步的进度没有排上重绘");
        go_tx.send(()).unwrap();
        wait(&mut job);
    }

    /// 批量：坏了一个接着做后面的，每个都有结果；取消是取消，不是失败。
    #[test]
    fn a_batch_keeps_going_and_cancelling_is_not_failing() {
        let ctx = egui::Context::default();
        let files: Vec<PathBuf> = ["a.docx", "bad.docx", "c.docx"]
            .iter()
            .map(PathBuf::from)
            .collect();
        let inputs = files.clone();
        let mut job = Job::spawn(&ctx, files.len(), move |w| {
            let b = run_batch(w, &inputs, |p, _| {
                if p.ends_with("bad.docx") {
                    Err(Stop::Failed("打不开".into()))
                } else {
                    Ok((p.with_extension("pdf"), "1 页".into()))
                }
            })?;
            Ok(Done {
                summary: format!("{} 成 {} 败", b.succeeded, b.failed),
                output: b.last_output,
            })
        });
        wait(&mut job);
        assert_eq!(job.state, State::Finished("2 成 1 败".into()));
        assert_eq!(job.items.len(), 3);
        assert_eq!(job.items[1].error.as_deref(), Some("打不开"));
        assert_eq!(job.succeeded(), 2);

        let mut job = Job::spawn(&ctx, files.len(), move |w| {
            run_batch(w, &files, |p, _| {
                // 第一个做完就有人按了取消。
                w.cancel.cancel();
                Ok((p.with_extension("pdf"), String::new()))
            })?;
            unreachable!("取消以后不该走到这里");
        });
        wait(&mut job);
        assert_eq!(job.state, State::Cancelled);
        assert_eq!(job.succeeded(), 1);

        // 全都失败：整个任务算失败，原因取第一个。
        let mut job = Job::spawn(&ctx, 2, |w| {
            let files = [PathBuf::from("x.pdf"), PathBuf::from("y.pdf")];
            run_batch(w, &files, |_, _| Err(Stop::Failed("坏了".into())))?;
            unreachable!()
        });
        wait(&mut job);
        assert_eq!(job.state, State::Failed("x.pdf：坏了".into()));
    }

    /// 并行的批量与逐个做的规矩一样：坏的记下接着做，每个都有结果；全坏了按排在
    /// 最前面的报；取消就停。
    #[test]
    fn a_parallel_batch_keeps_the_same_rules() {
        let ctx = egui::Context::default();
        let files: Vec<PathBuf> = (0..40)
            .map(|i| PathBuf::from(format!("{i}.docx")))
            .collect();
        let inputs = files.clone();
        let mut job = Job::spawn(&ctx, files.len(), move |w| {
            let b = run_batch_parallel(w, &inputs, |i, p, _| {
                // 做得有快有慢，完成的先后与序号无关。
                std::thread::sleep(Duration::from_millis((i % 3) as u64));
                if i % 10 == 7 {
                    Err(Stop::Failed("打不开".into()))
                } else {
                    Ok((p.with_extension("pdf"), String::new()))
                }
            })?;
            Ok(Done {
                summary: format!("{} 成 {} 败", b.succeeded, b.failed),
                output: None,
            })
        });
        wait(&mut job);
        assert_eq!(job.state, State::Finished("36 成 4 败".into()));
        assert_eq!(job.items.len(), 40);
        assert_eq!(job.progress.fraction, Some(1.0));

        let inputs = files.clone();
        let mut job = Job::spawn(&ctx, 40, move |w| {
            run_batch_parallel(w, &inputs, |i, _, _| {
                std::thread::sleep(Duration::from_millis(((40 - i) % 4) as u64));
                Err(Stop::Failed(format!("坏了 {i}")))
            })?;
            unreachable!()
        });
        wait(&mut job);
        assert_eq!(job.state, State::Failed("0.docx：坏了 0".into()));

        let mut job = Job::spawn(&ctx, 40, move |w| {
            run_batch_parallel(w, &files, |i, p, _| {
                if i == 5 {
                    w.cancel.cancel();
                }
                Ok((p.with_extension("pdf"), String::new()))
            })?;
            unreachable!("取消以后不该走到这里");
        });
        wait(&mut job);
        assert_eq!(job.state, State::Cancelled);
        assert!(job.succeeded() < 40);
    }
}
