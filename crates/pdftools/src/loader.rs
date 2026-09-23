//! 后台按需做的活（读拍摄时间、解缩略图、打开 PDF、画页面）：几个线程一起做，
//! 同一件活只做一次，**后要的先做** —— 用户刚滚到的那些行先出来，滚过去的排到后面。
//! 结果在 `App::logic` 里取回。活用一个键来认（文件路径、某份文档的某一页）。

use std::collections::HashSet;
use std::hash::Hash;
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Condvar, Mutex};

struct Queue<K> {
    stack: Vec<K>,
    closed: bool,
}

pub struct Loader<K, T> {
    queue: Arc<(Mutex<Queue<K>>, Condvar)>,
    rx: Receiver<(K, T)>,
    pending: HashSet<K>,
}

impl<K, T> Loader<K, T>
where
    K: Clone + Eq + Hash + Send + 'static,
    T: Send + 'static,
{
    /// `threads` 个线程，每个键调一次 `work`，做完唤醒界面。
    pub fn new(
        ctx: &egui::Context,
        threads: usize,
        work: impl Fn(&K) -> T + Send + Sync + 'static,
    ) -> Self {
        let queue = Arc::new((
            Mutex::new(Queue {
                stack: Vec::new(),
                closed: false,
            }),
            Condvar::new(),
        ));
        let (tx, rx) = channel();
        let work = Arc::new(work);
        for _ in 0..threads.max(1) {
            let (queue, tx, work, ctx) = (queue.clone(), tx.clone(), work.clone(), ctx.clone());
            std::thread::spawn(move || loop {
                let key = {
                    let (lock, ready) = &*queue;
                    let mut q = lock.lock().expect("后台队列锁");
                    loop {
                        if q.closed {
                            return;
                        }
                        if let Some(k) = q.stack.pop() {
                            break k;
                        }
                        q = ready.wait(q).expect("后台队列锁");
                    }
                };
                let result = work(&key);
                if tx.send((key, result)).is_err() {
                    return;
                }
                ctx.request_repaint();
            });
        }
        Self {
            queue,
            rx,
            pending: HashSet::new(),
        }
    }

    /// 要这件活的结果。已经在做或已经排着的不重复排。
    pub fn request(&mut self, key: &K) {
        if !self.pending.insert(key.clone()) {
            return;
        }
        let (lock, ready) = &*self.queue;
        lock.lock().expect("后台队列锁").stack.push(key.clone());
        ready.notify_one();
    }

    /// 做完的结果。
    pub fn drain(&mut self) -> Vec<(K, T)> {
        let done: Vec<_> = self.rx.try_iter().collect();
        for (p, _) in &done {
            self.pending.remove(p);
        }
        done
    }

    /// 还有几个没做完。
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// 不再需要的（移出了列表）从队列里拿掉；正在做的那个照样做完。
    pub fn retain(&mut self, keep: impl Fn(&K) -> bool) {
        let (lock, _) = &*self.queue;
        let mut q = lock.lock().expect("后台队列锁");
        q.stack.retain(|p| keep(p));
        self.pending.retain(|p| keep(p));
    }
}

impl<K, T> Drop for Loader<K, T> {
    fn drop(&mut self) {
        let (lock, ready) = &*self.queue;
        if let Ok(mut q) = lock.lock() {
            q.closed = true;
        }
        ready.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    fn wait_for<T: Send + 'static>(loader: &mut Loader<PathBuf, T>, n: usize) -> Vec<(PathBuf, T)> {
        let mut got = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while got.len() < n && Instant::now() < deadline {
            got.extend(loader.drain());
            std::thread::sleep(Duration::from_millis(2));
        }
        got
    }

    /// 同一个文件只做一次；后要的先做（只有一个线程时看得出顺序）。
    #[test]
    fn each_file_once_latest_first() {
        let ctx = egui::Context::default();
        let gate = Arc::new(Mutex::new(()));
        let held = gate.lock().unwrap();
        let g = gate.clone();
        let mut loader = Loader::new(&ctx, 1, move |p: &PathBuf| {
            // 第一个请求卡在这里，后面的都排进队列。
            drop(g.lock().unwrap());
            p.to_string_lossy().into_owned()
        });
        loader.request(&PathBuf::from("a"));
        // 等工作线程取走「a」、卡在闸门上，再排后面的。
        std::thread::sleep(Duration::from_millis(50));
        for name in ["b", "c", "b"] {
            loader.request(&PathBuf::from(name));
        }
        drop(held);
        let got: Vec<String> = wait_for(&mut loader, 3)
            .into_iter()
            .map(|(_, s)| s)
            .collect();
        // 「a」已经在做；剩下的后进先出。
        assert_eq!(got, ["a", "c", "b"]);
        assert_eq!(loader.pending(), 0);
    }
}
