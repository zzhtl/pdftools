//! PDF 写入。
//!
//! `pdf-writer` 是刻意低层的：它不替你管理任何间接引用、页树或资源字典。
//! 这层的职责就是把那些簿记工作收敛到一处，别让「忘了写 /Parent」这类错误
//! 散落到四个功能里 —— 那种 PDF 在 Chrome 里能开，在 Acrobat 里打不开。

pub mod canvas;
pub mod font;
pub mod image;

use pdf_writer::types::{ActionType, AnnotationType};
use pdf_writer::{Content, Finish, Name, Pdf, Rect, Ref, Str};

pub use canvas::{Canvas, GlyphRun};
pub use image::{ImageData, ImageEncoding};

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
    /// 可点击区域 [x0, y0, x1, y1] → 外部链接。
    pub links: Vec<([f32; 4], String)>,
}

impl PageSpec {
    pub fn new(width_pt: f32, height_pt: f32) -> Self {
        Self {
            width_pt,
            height_pt,
            content: Content::new(),
            images: Vec::new(),
            fonts: Vec::new(),
            links: Vec::new(),
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

    /// 写入一张图，返回 XObject 的对象号。
    ///
    /// 这里不做去重：同一张图被多处引用时（页眉里的 logo），由调用方按确切的来源
    /// （比如 docx 里的媒体部件名）缓存对象号。按内容哈希去重哪怕碰撞概率再低，
    /// 也可能把一张证据图悄悄换成另一张。
    pub fn add_image(&mut self, img: &ImageData) -> Ref {
        image::write_image(&mut self.pdf, &mut self.alloc, img)
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
        let mut file_id = FileId::new();

        {
            let info = std::mem::take(&mut self.info);
            file_id.feed(format!("{:?}", info).as_bytes());
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
                d.pair(
                    Name(b"CreationDate"),
                    pdf_writer::Str(t.to_pdf_string().as_bytes()),
                );
            }
            if let Some(t) = info.modified {
                d.pair(
                    Name(b"ModDate"),
                    pdf_writer::Str(t.to_pdf_string().as_bytes()),
                );
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
            let link_ids: Vec<Ref> = spec.links.iter().map(|_| self.alloc.next_ref()).collect();

            {
                let mut page = self.pdf.page(page_id);
                page.media_box(Rect::new(0.0, 0.0, spec.width_pt, spec.height_pt));
                // 忘了 /Parent 的 PDF 在部分阅读器里能开、在 Acrobat 里报错，
                // 所以它和 media_box 一样是必写项。
                page.parent(self.page_tree);
                page.contents(content_id);
                if !link_ids.is_empty() {
                    page.annotations(link_ids.iter().copied());
                }
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

            for ((rect, uri), id) in spec.links.iter().zip(&link_ids) {
                let mut a = self.pdf.annotation(*id);
                a.subtype(AnnotationType::Link);
                a.rect(Rect::new(rect[0], rect[1], rect[2], rect[3]));
                // 不画边框：Word 导出的链接只有文字样式，没有方框。
                a.border(0.0, 0.0, 0.0, None);
                a.action()
                    .action_type(ActionType::Uri)
                    .uri(Str(uri_ascii(uri).as_bytes()));
                a.finish();
            }

            let data = spec.content.finish();
            file_id.feed(&data);
            let compressed = crate::pdf::writer::deflate(&data);
            self.pdf
                .stream(content_id, &compressed)
                .filter(pdf_writer::Filter::FlateDecode)
                .finish();
        }

        let id = file_id.digest();
        // 新建的文件，两个标识相同；改版时第二个才会变。
        self.pdf.set_file_id((id.clone(), id));
        Ok(self.pdf.finish())
    }
}

/// PDF 的 `/URI` 只允许 7 位 ASCII，其余字节按 URL 的规矩百分号编码。
fn uri_ascii(uri: &str) -> String {
    let mut out = String::with_capacity(uri.len());
    for b in uri.bytes() {
        if b.is_ascii_graphic() {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// trailer 的 `/ID`：由页面内容、文档信息与生成时刻派生的 16 字节。
///
/// 只需要「不同文件大概率不同」，不需要抗碰撞，所以不引入哈希库。
struct FileId {
    a: u64,
    b: u64,
}

impl FileId {
    fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let mut me = Self {
            a: 0xcbf2_9ce4_8422_2325,
            b: 0x6c62_272e_07bb_0142,
        };
        me.feed(&nanos.to_le_bytes());
        me
    }

    fn feed(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.a = (self.a ^ *byte as u64).wrapping_mul(0x0000_0100_0000_01b3);
            self.b = (self.b ^ *byte as u64).wrapping_mul(0x0000_0100_0000_0193);
        }
    }

    fn digest(&self) -> Vec<u8> {
        let mut out = self.a.to_be_bytes().to_vec();
        out.extend_from_slice(&self.b.to_be_bytes());
        out
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
