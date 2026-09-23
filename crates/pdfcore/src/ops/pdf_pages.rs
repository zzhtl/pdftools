//! PDF 页面操作：合并、拆分、删页、重排、旋转。
//!
//! 这些都归到同一件事上 —— 按给定的顺序从一份或几份 PDF 里取页，拼成一份新的
//! （[`assemble`]）：合并是依次取每份的全部页，删页是取剩下的，拆分是分几次各取一段。
//!
//! 页面原样搬：内容流、字体、图片一个字节都不改，只搬选中的页用得到的对象。页面从
//! 上级继承的属性（资源、页面尺寸、旋转）先落到页面自己身上，换了页树也不丢。
//! 书签、表单、无障碍标签是整份文档层面的东西，搬不过来，如实告诉用户。

use std::collections::{HashMap, VecDeque};

use lopdf::{dictionary, Dictionary, Document, Object, ObjectId};

use crate::error::{CoreError, Report, Result, Warning, WarningKind};
use crate::pdf::read::load_for_rewrite;

/// 新文件里的一页：取自第 `source` 份 PDF 的第 `page` 页（都从 0 开始），在原有方向上
/// 再顺时针转 `rotate` 度（90 的倍数）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PagePick {
    pub source: usize,
    pub page: usize,
    pub rotate: i32,
}

/// 一份打开的源 PDF。
pub struct Source {
    doc: Document,
    pages: Vec<ObjectId>,
}

impl Source {
    /// 加了密的、带数字签名的不接：前者解不对，后者一改签名就失效。
    pub fn open(data: &[u8]) -> Result<Self> {
        let doc = load_for_rewrite(data)?;
        let pages = doc.get_pages().into_values().collect();
        Ok(Self { doc, pages })
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }
}

/// 页面从上级 `/Pages` 继承、换了页树就会丢的属性。
const INHERITED: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];
/// 顺着 `/Parent` 最多往上找几层。页树再深也用不了这么多，这是防坏文件里的环。
const MAX_TREE_DEPTH: usize = 64;

/// 按 `picks` 的顺序拼一份新 PDF。
pub fn assemble(sources: &[Source], picks: &[PagePick]) -> Result<Report<Vec<u8>>> {
    if picks.is_empty() {
        return Err(CoreError::Unsupported("没有选中任何一页".into()));
    }
    for p in picks {
        let Some(src) = sources.get(p.source) else {
            return Err(CoreError::Pdf(format!("没有第 {} 份源文件", p.source + 1)));
        };
        if p.page >= src.page_count() {
            return Err(CoreError::Unsupported(format!(
                "没有第 {} 页（共 {} 页）",
                p.page + 1,
                src.page_count()
            )));
        }
        if p.rotate % 90 != 0 {
            return Err(CoreError::Unsupported(format!(
                "只能按 90° 的倍数旋转，收到的是 {}°",
                p.rotate
            )));
        }
    }

    let version = sources
        .iter()
        .map(|s| s.doc.version.as_str())
        .max()
        .unwrap_or("1.7")
        // 按对象流写出，至少得是 1.5。
        .max("1.5");
    let mut out = Document::with_version(version);
    let mut copier = Copier {
        out: &mut out,
        sources,
        map: HashMap::new(),
        queue: VecDeque::new(),
    };

    // 先给每一页占好新的对象号：页面之间的链接（目录页跳到正文）要指向新文件里的页。
    // 同一页选了两次的，别处引用它时指向第一次出现的那页。
    let page_ids: Vec<ObjectId> = picks.iter().map(|_| copier.out.new_object_id()).collect();
    for (pick, &id) in picks.iter().zip(&page_ids) {
        let old = sources[pick.source].pages[pick.page];
        copier.map.entry((pick.source, old)).or_insert(id);
    }

    let tree_id = copier.out.new_object_id();
    for (pick, &id) in picks.iter().zip(&page_ids) {
        let src = &sources[pick.source];
        let mut page = flatten_page(&src.doc, src.pages[pick.page])?;
        let rotate = page
            .get(b"Rotate")
            .ok()
            .and_then(|r| src.doc.dereference(r).ok())
            .and_then(|(_, r)| r.as_i64().ok())
            .unwrap_or(0);
        let rotate = (rotate + i64::from(pick.rotate)).rem_euclid(360);
        page.remove(b"Rotate");
        if rotate != 0 {
            page.set("Rotate", rotate);
        }
        // 文章线程（/B）挂在整份文档上，搬过来只剩断头的引用。
        page.remove(b"B");
        let mut page = copier.import(pick.source, Object::Dictionary(page));
        if let Object::Dictionary(d) = &mut page {
            d.set("Parent", tree_id);
        }
        copier.out.objects.insert(id, page);
    }
    copier.drain();

    out.objects.insert(
        tree_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => page_ids.iter().map(|&id| Object::Reference(id)).collect::<Vec<_>>(),
            "Count" => page_ids.len() as i64,
        }),
    );
    let catalog = out.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => tree_id,
    });
    out.trailer.set("Root", catalog);

    let mut buf = Vec::new();
    if out.save_modern(&mut buf).is_err() {
        buf.clear();
        out.save_to(&mut buf)
            .map_err(|e| CoreError::Pdf(format!("写出 PDF 失败：{e}")))?;
    }
    Ok(Report::with(buf, dropped_features(sources, picks)))
}

/// 页面字典，连同从上级继承来的属性。
fn flatten_page(doc: &Document, id: ObjectId) -> Result<Dictionary> {
    let mut page = doc
        .get_dictionary(id)
        .map_err(|e| CoreError::Pdf(format!("页面对象读不出来：{e}")))?
        .clone();
    let mut parent = page.get(b"Parent").and_then(Object::as_reference).ok();
    for _ in 0..MAX_TREE_DEPTH {
        let Some(node) = parent.and_then(|p| doc.get_dictionary(p).ok()) else {
            break;
        };
        for key in INHERITED {
            if !page.has(key) {
                if let Ok(v) = node.get(key) {
                    page.set(key, v.clone());
                }
            }
        }
        parent = node.get(b"Parent").and_then(Object::as_reference).ok();
    }
    page.remove(b"Parent");
    Ok(page)
}

/// 把源文件里的对象搬进新文件：遇到引用就给被引用的对象一个新号，排进队列稍后搬。
/// 用队列而不是递归，嵌得再深也不会把栈撑爆。
struct Copier<'a> {
    out: &'a mut Document,
    sources: &'a [Source],
    /// (源文件, 源对象号) → 新对象号。
    map: HashMap<(usize, ObjectId), ObjectId>,
    queue: VecDeque<(usize, ObjectId, ObjectId)>,
}

impl Copier<'_> {
    fn drain(&mut self) {
        while let Some((src, old, new)) = self.queue.pop_front() {
            let object = self.sources[src]
                .doc
                .get_object(old)
                .cloned()
                .unwrap_or(Object::Null);
            let object = self.import(src, object);
            self.out.objects.insert(new, object);
        }
    }

    /// 改写 `object` 里的引用。
    fn import(&mut self, src: usize, object: Object) -> Object {
        match object {
            Object::Reference(id) => self.reference(src, id),
            Object::Array(items) => {
                Object::Array(items.into_iter().map(|o| self.import(src, o)).collect())
            }
            Object::Dictionary(dict) => Object::Dictionary(self.import_dict(src, dict)),
            Object::Stream(mut stream) => {
                stream.dict = self.import_dict(src, std::mem::take(&mut stream.dict));
                Object::Stream(stream)
            }
            other => other,
        }
    }

    fn import_dict(&mut self, src: usize, dict: Dictionary) -> Dictionary {
        let mut out = Dictionary::new();
        for (k, v) in dict.into_iter() {
            out.set(k, self.import(src, v));
        }
        out
    }

    fn reference(&mut self, src: usize, id: ObjectId) -> Object {
        if let Some(&new) = self.map.get(&(src, id)) {
            return Object::Reference(new);
        }
        // 没选中的页、旧的页树与目录：不搬。指向它们的链接、注释里的页引用就此落空
        // （PDF 里指向不存在对象的引用按 null 处理），不会把整份原文件都拖进来。
        let doc = &self.sources[src].doc;
        let kind = doc
            .get_dictionary(id)
            .ok()
            .and_then(|d| d.get(b"Type").ok())
            .and_then(|t| t.as_name().ok());
        if matches!(kind, Some(b"Page" | b"Pages" | b"Catalog")) {
            return Object::Null;
        }
        let new = self.out.new_object_id();
        self.map.insert((src, id), new);
        self.queue.push_back((src, id, new));
        Object::Reference(new)
    }
}

/// 用到的源文件带着、拼出来的新文件带不过去的整份文档层面的东西。
fn dropped_features(sources: &[Source], picks: &[PagePick]) -> Vec<Warning> {
    let mut used: Vec<usize> = picks.iter().map(|p| p.source).collect();
    used.sort_unstable();
    used.dedup();
    // 目录里有这一项、而且不是空的（有的生成器会放一个空的书签树或空表单）。
    let has = |key: &[u8], filled: fn(&Dictionary) -> bool| {
        used.iter().any(|&i| {
            let doc = &sources[i].doc;
            doc.catalog()
                .ok()
                .and_then(|c| c.get(key).ok())
                .and_then(|v| doc.dereference(v).ok())
                .and_then(|(_, v)| v.as_dict().ok())
                .is_some_and(filled)
        })
    };
    let mut lost = Vec::new();
    if has(b"Outlines", |d| d.has(b"First")) {
        lost.push("书签");
    }
    if has(b"AcroForm", |d| {
        d.get(b"Fields")
            .and_then(Object::as_array)
            .is_ok_and(|f| !f.is_empty())
    }) {
        lost.push("可填写的表单域");
    }
    if has(b"StructTreeRoot", |d| d.has(b"K")) {
        lost.push("无障碍标签（结构树）");
    }
    if lost.is_empty() {
        return Vec::new();
    }
    vec![Warning::new(
        WarningKind::UnsupportedElement,
        format!(
            "原文件里的{}属于整份文档，没有带到新文件里；页面内容不受影响",
            lost.join("、")
        ),
    )]
}
