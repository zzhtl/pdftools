//! 缩略图缓存。
//!
//! 不能用 `egui::Image::from_uri` 加载用户的照片：egui_extras 的位图 loader
//! 会忽略 `SizeHint` 按原分辨率解码，100 张 1200 万像素的照片足以把显存吃光。
//! 所以自己解码、自己缩到显示尺寸，再上传纹理。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};

const THUMB_PX: u32 = 160;

enum Reply {
    Ready(PathBuf, egui::ColorImage),
    Failed(PathBuf),
}

#[derive(Clone)]
pub enum Thumb {
    Pending,
    Ready(egui::TextureHandle),
    Failed,
}

pub struct ThumbCache {
    map: HashMap<PathBuf, Thumb>,
    tx: Sender<PathBuf>,
    rx: Receiver<Reply>,
}

impl ThumbCache {
    pub fn new(ctx: &egui::Context) -> Self {
        let (req_tx, req_rx) = channel::<PathBuf>();
        let (rep_tx, rep_rx) = channel::<Reply>();
        let ctx = ctx.clone();

        std::thread::spawn(move || {
            // 后进先出：用户滚动列表时，最新可见的那批先解码，手感才跟得上。
            let mut queue: Vec<PathBuf> = Vec::new();
            loop {
                if queue.is_empty() {
                    match req_rx.recv() {
                        Ok(p) => queue.push(p),
                        Err(_) => break, // 主线程已退出
                    }
                }
                queue.extend(req_rx.try_iter());
                let Some(path) = queue.pop() else { continue };

                let reply = match decode_thumb(&path) {
                    Some(img) => Reply::Ready(path, img),
                    None => Reply::Failed(path),
                };
                if rep_tx.send(reply).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });

        Self {
            map: HashMap::new(),
            tx: req_tx,
            rx: rep_rx,
        }
    }

    /// 在 `App::logic` 里调用，把解码好的图上传成纹理。
    pub fn pump(&mut self, ctx: &egui::Context) {
        for reply in self.rx.try_iter() {
            match reply {
                Reply::Ready(path, img) => {
                    let name = path.to_string_lossy().to_string();
                    let handle = ctx.load_texture(name, img, egui::TextureOptions::LINEAR);
                    self.map.insert(path, Thumb::Ready(handle));
                }
                Reply::Failed(path) => {
                    self.map.insert(path, Thumb::Failed);
                }
            }
        }
    }

    pub fn get(&mut self, path: &PathBuf) -> Thumb {
        if let Some(t) = self.map.get(path) {
            return t.clone();
        }
        self.map.insert(path.clone(), Thumb::Pending);
        let _ = self.tx.send(path.clone());
        Thumb::Pending
    }

    /// 文件被移出列表后释放对应纹理，免得长期占着显存。
    pub fn retain(&mut self, keep: &[PathBuf]) {
        self.map.retain(|k, _| keep.contains(k));
    }
}

fn decode_thumb(path: &std::path::Path) -> Option<egui::ColorImage> {
    let mut img = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?;
    img.no_limits();
    let mut decoded = img.decode().ok()?;
    // EXIF 方向要在缩略图上也施加，否则列表里的照片是躺着的，
    // 而导出的 PDF 里是正的，用户会以为程序转错了。
    if let Ok(reader) = image::ImageReader::open(path).and_then(|r| r.with_guessed_format()) {
        if let Ok(mut dec) = reader.into_decoder() {
            use image::ImageDecoder;
            if let Ok(Some(exif)) = dec.exif_metadata() {
                if let Some(o) = image::metadata::Orientation::from_exif_chunk(&exif) {
                    decoded.apply_orientation(o);
                }
            }
        }
    }
    let thumb = decoded.thumbnail(THUMB_PX, THUMB_PX);
    let rgba = thumb.to_rgba8();
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        rgba.as_raw(),
    ))
}
