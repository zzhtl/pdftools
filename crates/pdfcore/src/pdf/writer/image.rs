//! 把准备好的图像写成 PDF 的 image XObject。

use pdf_writer::{Filter, Finish, Pdf, Ref};

use super::{deflate, RefAlloc};
use crate::imaging::{ColorData, PreparedImage};

/// 写入图像（含 alpha 的会额外写一个 `/SMask`），返回主图的对象号。
pub fn write_image(pdf: &mut Pdf, alloc: &mut RefAlloc, img: &PreparedImage) -> Ref {
    // alpha 单独成一个灰度 XObject，通过 /SMask 关联到主图。
    let smask_ref = img.alpha.as_ref().map(|alpha| {
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
    match &img.color {
        ColorData::Jpeg { bytes, gray } => {
            // 关键路径：原始 JPEG 字节原封不动地写进流，PDF 用 DCTDecode 自己解。
            // 不解码、不重编码，所以是真正的零损失。
            let mut x = pdf.image_xobject(id, bytes);
            x.filter(Filter::DctDecode);
            x.width(img.width as i32);
            x.height(img.height as i32);
            if *gray {
                x.color_space().device_gray();
            } else {
                x.color_space().device_rgb();
            }
            x.bits_per_component(8);
            if let Some(m) = smask_ref {
                x.s_mask(m);
            }
            x.finish();
        }
        ColorData::Raw { bytes, gray } => {
            let compressed = deflate(bytes);
            let mut x = pdf.image_xobject(id, &compressed);
            x.filter(Filter::FlateDecode);
            x.width(img.width as i32);
            x.height(img.height as i32);
            if *gray {
                x.color_space().device_gray();
            } else {
                x.color_space().device_rgb();
            }
            x.bits_per_component(8);
            if let Some(m) = smask_ref {
                x.s_mask(m);
            }
            x.finish();
        }
    }
    id
}
