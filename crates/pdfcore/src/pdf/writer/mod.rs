//! PDF 写入。
//!
//! `pdf-writer` 是刻意低层的：它不替你管理任何间接引用、页树或资源字典。
//! 这层的职责就是把那些簿记工作收敛到一处，别让「忘了写 /Parent」这类错误
//! 散落到四个功能里 —— 那种 PDF 在 Chrome 里能开，在 Acrobat 里打不开。

pub mod font;
pub mod image;
pub mod text;

use pdf_writer::{Content, Finish, Name, Pdf, Rect, Ref};

use crate::error::Result;

/// 间接引用分配器。PDF 的对象编号从 1 开始。
pub struct RefAlloc {
    next: i32,
}

impl RefAlloc {
    pub fn new() -> Self {
        Self { next: 1 }
    }

    pub fn next_ref(&mut self) -> Ref {
        let r = Ref::new(self.next);
        self.next += 1;
        r
    }
}

impl Default for RefAlloc {
    fn default() -> Self {
        Self::new()
    }
}

/// 一页的内容与它引用的资源。
pub struct PageSpec {
    pub width_pt: f32,
    pub height_pt: f32,
    pub content: Content,
    /// 资源名 → XObject 对象号。资源名是内容流里 `/Name Do` 用的那个。
    pub images: Vec<(String, Ref)>,
    /// 资源名 → 字体对象号。
    pub fonts: Vec<(String, Ref)>,
}

impl PageSpec {
    pub fn new(width_pt: f32, height_pt: f32) -> Self {
        Self {
            width_pt,
            height_pt,
            content: Content::new(),
            images: Vec::new(),
            fonts: Vec::new(),
        }
    }
}

/// PDF 的文档信息字典。
///
/// 对取证类文档，`creation` 记的是**内容形成的时间**（最早一张照片的拍摄时间、
/// 或 Word 文档的创建时间），而不是「按下导出按钮的那一刻」——
/// 别人拿到 PDF 打开属性看到的应当是前者。后者记在 `modified` 里。
#[derive(Debug, Clone, Default)]
pub struct DocInfo {
    pub title: Option<String>,
    pub subject: Option<String>,
    pub creation: Option<crate::timestamp::Timestamp>,
    pub modified: Option<crate::timestamp::Timestamp>,
}

pub struct DocBuilder {
    pdf: Pdf,
    alloc: RefAlloc,
    catalog: Ref,
    page_tree: Ref,
    pages: Vec<(Ref, PageSpec)>,
    info: DocInfo,
}

impl DocBuilder {
    pub fn new() -> Self {
        let mut alloc = RefAlloc::new();
        let catalog = alloc.next_ref();
        let page_tree = alloc.next_ref();
        Self {
            pdf: Pdf::new(),
            alloc,
            catalog,
            page_tree,
            pages: Vec::new(),
            info: DocInfo::default(),
        }
    }

    pub fn set_info(&mut self, info: DocInfo) {
        self.info = info;
    }

    /// 直接访问底层写入器，用于写 XObject、字体这类独立对象。
    pub fn pdf_mut(&mut self) -> &mut Pdf {
        &mut self.pdf
    }

    pub fn alloc(&mut self) -> Ref {
        self.alloc.next_ref()
    }

    /// 同时借出写入器和分配器。
    ///
    /// 写字体、写 XObject 都需要「先要一个对象号，再往那个号里写内容」，
    /// 两者必须同时可变借用。分开成两个方法会被借用检查器拒绝。
    pub fn parts(&mut self) -> (&mut Pdf, &mut RefAlloc) {
        (&mut self.pdf, &mut self.alloc)
    }

    pub fn add_page(&mut self, spec: PageSpec) {
        let id = self.alloc.next_ref();
        self.pages.push((id, spec));
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn finish(mut self) -> Result<Vec<u8>> {
        let page_ids: Vec<Ref> = self.pages.iter().map(|(id, _)| *id).collect();

        {
            let info = std::mem::take(&mut self.info);
            let info_id = self.alloc.next_ref();
            // document_info 会自动把这个对象登记进 trailer 的 /Info。
            let mut d = self.pdf.document_info(info_id);
            d.producer(pdf_writer::TextStr(PRODUCER));
            d.creator(pdf_writer::TextStr(PRODUCER));
            if let Some(t) = &info.title {
                d.title(pdf_writer::TextStr(t));
            }
            if let Some(t) = &info.subject {
                d.subject(pdf_writer::TextStr(t));
            }
            if let Some(t) = info.creation {
                d.creation_date(t.to_pdf_date());
            }
            if let Some(t) = info.modified {
                d.modified_date(t.to_pdf_date());
            }
            d.finish();
        }

        self.pdf.catalog(self.catalog).pages(self.page_tree);
        self.pdf
            .pages(self.page_tree)
            .kids(page_ids.iter().copied())
            .count(page_ids.len() as i32);

        for (page_id, spec) in std::mem::take(&mut self.pages) {
            let content_id = self.alloc.next_ref();

            {
                let mut page = self.pdf.page(page_id);
                page.media_box(Rect::new(0.0, 0.0, spec.width_pt, spec.height_pt));
                // 忘了 /Parent 的 PDF 在部分阅读器里能开、在 Acrobat 里报错，
                // 所以它和 media_box 一样是必写项。
                page.parent(self.page_tree);
                page.contents(content_id);
                {
                    let mut resources = page.resources();
                    if !spec.images.is_empty() {
                        let mut xobjects = resources.x_objects();
                        for (name, r) in &spec.images {
                            xobjects.pair(Name(name.as_bytes()), *r);
                        }
                        xobjects.finish();
                    }
                    if !spec.fonts.is_empty() {
                        let mut fonts = resources.fonts();
                        for (name, r) in &spec.fonts {
                            fonts.pair(Name(name.as_bytes()), *r);
                        }
                        fonts.finish();
                    }
                    resources.finish();
                }
                page.finish();
            }

            let data = spec.content.finish();
            let compressed = crate::pdf::writer::deflate(&data);
            self.pdf
                .stream(content_id, &compressed)
                .filter(pdf_writer::Filter::FlateDecode)
                .finish();
        }

        Ok(self.pdf.finish())
    }
}

impl Default for DocBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// 写进 PDF 的 /Producer 与 /Creator。
const PRODUCER: &str = "pdftools";

/// zlib 压缩。PDF 的 FlateDecode 就是 zlib 流。
pub fn deflate(data: &[u8]) -> Vec<u8> {
    use flate2::write::ZlibEncoder;
    use std::io::Write;
    let mut enc = ZlibEncoder::new(Vec::new(), flate2::Compression::new(7));
    enc.write_all(data).expect("写入内存缓冲不会失败");
    enc.finish().expect("zlib 收尾不会失败")
}
