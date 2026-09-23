//! 下次打开还记得的设置：停在哪个页签、各页的档位与灰度、各页上次用的文件夹。
//! 窗口大小位置由 eframe 自己记。文件列表不记 —— 隔一次启动，那些文件可能已经
//! 挪走、删掉了。
//!
//! 页签、档位按名字存，认不出来的名字（改过名、别的版本写的）只让那一项回到
//! 默认。文件夹存成字符串：不是合法 UTF-8 的路径 serde 写不出来，而一项写不出来
//! 整份设置都存不下，所以这种文件夹干脆不记。

use std::path::{Path, PathBuf};

use pdfcore::imaging::Tier;

use crate::app::{App, Tab};

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Prefs {
    tab: String,
    images: TabPrefs,
    docx: TabPrefs,
    pdf_compress: TabPrefs,
    img_compress: TabPrefs,
}

/// 各页共用一个形状，没有的项（Word 页没有档位）留默认值。
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct TabPrefs {
    tier: String,
    grayscale: bool,
    open_dir: Option<String>,
    out_dir: Option<String>,
}

pub fn save(app: &App, storage: &mut dyn eframe::Storage) {
    let dir = |d: &Option<PathBuf>| d.as_deref().and_then(Path::to_str).map(str::to_owned);
    let prefs = Prefs {
        tab: tab_key(app.tab).to_owned(),
        images: TabPrefs {
            tier: tier_key(app.images.tier).to_owned(),
            open_dir: dir(&app.images.open_dir),
            out_dir: dir(&app.images.out_dir),
            ..Default::default()
        },
        docx: TabPrefs {
            open_dir: dir(&app.docx.open_dir),
            out_dir: dir(&app.docx.out_dir),
            ..Default::default()
        },
        pdf_compress: TabPrefs {
            tier: tier_key(app.compress.tier).to_owned(),
            grayscale: app.compress.grayscale,
            open_dir: dir(&app.compress.open_dir),
            out_dir: dir(&app.compress.out_dir),
        },
        img_compress: TabPrefs {
            tier: tier_key(app.img_compress.tier).to_owned(),
            open_dir: dir(&app.img_compress.open_dir),
            out_dir: dir(&app.img_compress.out_dir),
            ..Default::default()
        },
    };
    eframe::set_value(storage, eframe::APP_KEY, &prefs);
}

pub fn load(app: &mut App, storage: &dyn eframe::Storage) {
    let Some(p) = eframe::get_value::<Prefs>(storage, eframe::APP_KEY) else {
        return;
    };
    let dir = |d: Option<String>| d.map(PathBuf::from);
    if let Some(tab) = find(Tab::ALL, tab_key, &p.tab) {
        app.tab = tab;
    }

    if let Some(t) = find(Tier::ALL, tier_key, &p.images.tier) {
        app.images.tier = t;
    }
    app.images.open_dir = dir(p.images.open_dir);
    app.images.out_dir = dir(p.images.out_dir);

    app.docx.open_dir = dir(p.docx.open_dir);
    app.docx.out_dir = dir(p.docx.out_dir);

    if let Some(t) = find(Tier::ALL, tier_key, &p.pdf_compress.tier) {
        app.compress.tier = t;
    }
    app.compress.grayscale = p.pdf_compress.grayscale;
    app.compress.open_dir = dir(p.pdf_compress.open_dir);
    app.compress.out_dir = dir(p.pdf_compress.out_dir);

    if let Some(t) = find(Tier::ALL, tier_key, &p.img_compress.tier) {
        app.img_compress.tier = t;
    }
    app.img_compress.open_dir = dir(p.img_compress.open_dir);
    app.img_compress.out_dir = dir(p.img_compress.out_dir);
}

fn find<T: Copy, const N: usize>(all: [T; N], key: fn(T) -> &'static str, name: &str) -> Option<T> {
    all.into_iter().find(|&t| key(t) == name)
}

fn tab_key(tab: Tab) -> &'static str {
    match tab {
        Tab::ImagesToPdf => "img2pdf",
        Tab::DocxToPdf => "docx2pdf",
        Tab::PdfCompress => "pdf_compress",
        Tab::ImagesCompress => "img_compress",
    }
}

fn tier_key(tier: Tier) -> &'static str {
    match tier {
        Tier::Lossless => "lossless",
        Tier::HighQuality => "high",
        Tier::Balanced => "balanced",
        Tier::Extreme => "extreme",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::Storage;
    use std::collections::HashMap;

    #[derive(Default)]
    struct Memory(HashMap<String, String>);

    impl Storage for Memory {
        fn get_string(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
        fn set_string(&mut self, key: &str, value: String) {
            self.0.insert(key.to_owned(), value);
        }
        fn remove_string(&mut self, key: &str) {
            self.0.remove(key);
        }
        fn flush(&mut self) {}
    }

    fn fresh(ctx: &egui::Context, storage: Option<&dyn eframe::Storage>) -> App {
        App::new(ctx, None, storage)
    }

    #[test]
    fn settings_survive_a_restart() {
        let ctx = egui::Context::default();
        let mut app = fresh(&ctx, None);
        app.tab = Tab::PdfCompress;
        app.images.tier = Tier::HighQuality;
        app.compress.tier = Tier::Extreme;
        app.compress.grayscale = true;
        app.img_compress.tier = Tier::Lossless;
        app.docx.open_dir = Some(PathBuf::from("/data/公文"));
        app.images.out_dir = Some(PathBuf::from("/data/out"));
        let mut mem = Memory::default();
        save(&app, &mut mem);

        let again = fresh(&ctx, Some(&mem));
        assert_eq!(again.tab, Tab::PdfCompress);
        assert_eq!(again.images.tier, Tier::HighQuality);
        assert_eq!(again.compress.tier, Tier::Extreme);
        assert!(again.compress.grayscale);
        assert_eq!(again.img_compress.tier, Tier::Lossless);
        assert_eq!(again.docx.open_dir, Some(PathBuf::from("/data/公文")));
        assert_eq!(again.images.out_dir, Some(PathBuf::from("/data/out")));
        assert_eq!(again.compress.open_dir, None);
    }

    /// 认不出来的名字只让那一项回到默认；没存过、存坏了的就整体用默认。
    #[test]
    fn unknown_values_fall_back_one_by_one() {
        let ctx = egui::Context::default();
        let mut mem = Memory::default();
        mem.set_string(
            eframe::APP_KEY,
            r#"(tab: "pdf_pages", pdf_compress: (tier: "ultra", grayscale: true), img_compress: (tier: "extreme"), future: 3)"#
                .to_owned(),
        );
        let app = fresh(&ctx, Some(&mem));
        let defaults = fresh(&ctx, None);
        assert_eq!(app.tab, defaults.tab);
        assert_eq!(app.compress.tier, defaults.compress.tier);
        assert!(app.compress.grayscale);
        assert_eq!(app.img_compress.tier, Tier::Extreme);

        mem.set_string(eframe::APP_KEY, "not ron".to_owned());
        let app = fresh(&ctx, Some(&mem));
        assert_eq!(app.images.tier, defaults.images.tier);
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_folder_is_dropped_not_the_whole_save() {
        use std::os::unix::ffi::OsStrExt;
        let ctx = egui::Context::default();
        let mut app = fresh(&ctx, None);
        app.compress.grayscale = true;
        app.compress.out_dir = Some(PathBuf::from(std::ffi::OsStr::from_bytes(b"/data/\xff")));
        let mut mem = Memory::default();
        save(&app, &mut mem);

        let again = fresh(&ctx, Some(&mem));
        assert!(again.compress.grayscale);
        assert_eq!(again.compress.out_dir, None);
    }
}
