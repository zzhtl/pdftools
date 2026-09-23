//! 缩略图缓存。
//!
//! 不能用 `egui::Image::from_uri` 加载用户的照片：egui_extras 的位图 loader
//! 会忽略 `SizeHint` 按原分辨率解码，100 张 1200 万像素的照片足以把显存吃光。
//! 所以自己解码、自己缩到显示尺寸，再上传纹理。
//!
//! 解码在后台几个线程里做（见 [`Loader`]），只解看得见的那些行；JPEG 先用 EXIF 自带
//! 的小图，不必解开整张大图。纹理最多留 [`MAX_TEXTURES`] 张，最久没看的先释放。
//! PDF 页面的缩略图（见 `tabs::pdf_pages`）走同一套，只是画法不同。

use std::collections::HashMap;
use std::hash::Hash;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::loader::Loader;

const THUMB_PX: u32 = 160;
/// 最多留几张缩略图的纹理。一张约 100 KB 显存，上千张的列表也只占几十 MB。
const MAX_TEXTURES: usize = 300;

#[derive(Clone)]
pub enum Thumb {
    Pending,
    Ready(egui::TextureHandle),
    Failed,
}

struct Entry {
    thumb: Thumb,
    /// 最近一次被画出来是第几帧。
    used: u64,
}

pub struct ThumbCache<K> {
    loader: Loader<K, Option<egui::ColorImage>>,
    map: HashMap<K, Entry>,
    frame: u64,
    /// 纹理的名字只用于调试，编个号就行。
    uploaded: u64,
}

/// 图片文件的缩略图。
pub fn images(ctx: &egui::Context) -> ThumbCache<PathBuf> {
    ThumbCache::new(ctx, |p: &PathBuf| decode_thumb(p))
}

impl<K: Clone + Eq + Hash + Send + 'static> ThumbCache<K> {
    /// `draw` 在后台线程上把一个键画成缩略图；画不出来返回 None。
    pub fn new(
        ctx: &egui::Context,
        draw: impl Fn(&K) -> Option<egui::ColorImage> + Send + Sync + 'static,
    ) -> Self {
        let threads = std::thread::available_parallelism()
            .map_or(2, |n| n.get() / 2)
            .clamp(2, 4);
        Self {
            loader: Loader::new(ctx, threads, draw),
            map: HashMap::new(),
            frame: 0,
            uploaded: 0,
        }
    }

    /// 在 `App::logic` 里调用：把解码好的图上传成纹理，超出上限的释放掉。
    pub fn pump(&mut self, ctx: &egui::Context) {
        self.frame += 1;
        for (key, img) in self.loader.drain() {
            let thumb = match img {
                Some(img) => {
                    self.uploaded += 1;
                    let name = format!("thumb-{}", self.uploaded);
                    Thumb::Ready(ctx.load_texture(name, img, egui::TextureOptions::LINEAR))
                }
                None => Thumb::Failed,
            };
            let used = self.map.get(&key).map_or(self.frame, |e| e.used);
            self.map.insert(key, Entry { thumb, used });
        }
        let ready = self
            .map
            .values()
            .filter(|e| matches!(e.thumb, Thumb::Ready(_)))
            .count();
        if ready > MAX_TEXTURES {
            let mut old: Vec<(u64, K)> = self
                .map
                .iter()
                .filter(|(_, e)| matches!(e.thumb, Thumb::Ready(_)) && e.used + 1 < self.frame)
                .map(|(k, e)| (e.used, k.clone()))
                .collect();
            old.sort_by_key(|(used, _)| *used);
            for (_, k) in old.into_iter().take(ready - MAX_TEXTURES) {
                self.map.remove(&k);
            }
        }
    }

    /// 这个键的缩略图；还没有就排进后台去画。
    pub fn get(&mut self, key: &K) -> Thumb {
        if let Some(e) = self.map.get_mut(key) {
            e.used = self.frame;
            return e.thumb.clone();
        }
        self.map.insert(
            key.clone(),
            Entry {
                thumb: Thumb::Pending,
                used: self.frame,
            },
        );
        self.loader.request(key);
        Thumb::Pending
    }

    /// 移出列表的释放对应纹理，还没画的也不画了。
    pub fn retain(&mut self, keep: impl Fn(&K) -> bool) {
        self.map.retain(|k, _| keep(k));
        self.loader.retain(keep);
    }
}

fn decode_thumb(path: &Path) -> Option<egui::ColorImage> {
    // JPEG 的 EXIF 在开头；自带的小图够预览用。方向要在缩略图上也施加，否则列表里
    // 的照片是躺着的，而导出的 PDF 里是正的，用户会以为程序转错了。
    let mut head = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(1 << 20)
        .read_to_end(&mut head)
        .ok()?;
    let exif = image::ImageReader::new(std::io::Cursor::new(&head))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_decoder().ok())
        .and_then(|mut d| {
            use image::ImageDecoder;
            d.exif_metadata().ok().flatten()
        });
    let orientation = exif
        .as_deref()
        .and_then(image::metadata::Orientation::from_exif_chunk)
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let small = exif
        .as_deref()
        .and_then(pdfcore::imaging::probe::exif_thumbnail)
        .and_then(|jpeg| image::load_from_memory(&jpeg).ok());
    let mut decoded = match small {
        Some(img) => img,
        None => {
            let mut reader = image::ImageReader::open(path)
                .ok()?
                .with_guessed_format()
                .ok()?;
            reader.no_limits();
            reader.decode().ok()?
        }
    };
    decoded.apply_orientation(orientation);
    let thumb = decoded.thumbnail(THUMB_PX, THUMB_PX);
    let rgba = thumb.to_rgba8();
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        rgba.as_raw(),
    ))
}
