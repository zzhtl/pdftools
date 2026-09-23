//! 把图像写成 PDF 的 image XObject。

use pdf_writer::{Filter, Finish, Pdf, Ref};

use super::{deflate, RefAlloc};

/// 主图数据的形态。
pub enum ImageEncoding<'a> {
    /// 已经是 JPEG 字节，PDF 里直接用 `/DCTDecode`，不解码不重编码。
    Jpeg(&'a [u8]),
    /// 原始 8 位样本（未压缩），写入时用 `/FlateDecode` 无损压缩。
    Raw(&'a [u8]),
}

/// 要写进 PDF 的一张图。与图片流水线的内部类型无关，docx 里的图也走这里。
pub struct ImageData<'a> {
    pub width: u32,
    pub height: u32,
    /// 单分量灰度；否则 RGB。
    pub gray: bool,
    pub encoding: ImageEncoding<'a>,
    /// 8 位 alpha 平面，写成 `/SMask`。
    pub alpha: Option<&'a [u8]>,
}

/// 写入图像（含 alpha 的会额外写一个 `/SMask`），返回主图的对象号。
pub fn write_image(pdf: &mut Pdf, alloc: &mut RefAlloc, img: &ImageData) -> Ref {
    // alpha 单独成一个灰度 XObject，通过 /SMask 关联到主图。
    let smask_ref = img.alpha.map(|alpha| {
        let id = alloc.next_ref();
        let compressed = deflate(alpha);
        let mut x = pdf.image_xobject(id, &compressed);
        x.filter(Filter::FlateDecode);
        x.width(img.width as i32);
        x.height(img.height as i32);
        x.color_space().device_gray();
        x.bits_per_component(8);
        x.finish();
        id
    });

    let id = alloc.next_ref();
    let compressed;
    let (data, filter) = match img.encoding {
        // 关键路径：原始 JPEG 字节原封不动地写进流，PDF 用 DCTDecode 自己解。
        // 不解码、不重编码，所以是真正的零损失。
        ImageEncoding::Jpeg(bytes) => (bytes, Filter::DctDecode),
        ImageEncoding::Raw(bytes) => {
            compressed = deflate(bytes);
            (compressed.as_slice(), Filter::FlateDecode)
        }
    };
    let mut x = pdf.image_xobject(id, data);
    x.filter(filter);
    x.width(img.width as i32);
    x.height(img.height as i32);
    if img.gray {
        x.color_space().device_gray();
    } else {
        x.color_space().device_rgb();
    }
    x.bits_per_component(8);
    if let Some(m) = smask_ref {
        x.s_mask(m);
    }
    x.finish();
    id
}
