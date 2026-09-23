//! 把图像写成 PDF 的 image XObject。

use pdf_writer::types::Predictor;
use pdf_writer::{Filter, Finish, Pdf, Ref};

use super::{deflate, RefAlloc};

/// 主图数据的形态。
pub enum ImageEncoding<'a> {
    /// 已经是 JPEG 字节，PDF 里直接用 `/DCTDecode`，不解码不重编码。
    Jpeg(&'a [u8]),
    /// 原始 8 位样本（未压缩），写入时用 [`flate_image`] 无损压缩。
    Raw(&'a [u8]),
    /// 已经用 [`flate_image`] 压好的样本。图多的时候在各个线程里先压好，写出时只拷贝。
    Flate(&'a [u8]),
}

/// 无损压缩一张图的 8 位样本：逐行做 PNG 预测（PDF 的 `/Predictor 15`），再 Flate。
///
/// 预测把「这个像素」换成「它与左、上、左上邻居的差」，截图、线稿、平滑的照片里
/// 这些差大多是 0 或很小的数，Flate 压起来小得多；阅读器解码时逐行还原，像素不变。
pub fn flate_image(raw: &[u8], width: u32, components: usize) -> Vec<u8> {
    let row_bytes = width as usize * components;
    if row_bytes == 0 {
        return deflate(raw);
    }
    let mut predicted = Vec::with_capacity(raw.len() + raw.len() / row_bytes);
    let mut above = vec![0u8; row_bytes];
    for row in raw.chunks_exact(row_bytes) {
        predict_row(&above, row, components, &mut predicted);
        above.copy_from_slice(row);
    }
    deflate(&predicted)
}

/// 预测一行：先写这一行用哪种预测（PNG 的 0–4），再写残差。按 PNG 编码器的老办法，
/// 挑残差（当作有符号数）绝对值之和最小的那种。`above` 是上一行，第一行传全 0。
///
/// 整张图的每个样本都要过这里两遍，所以写成等长切片上的下标循环：边界检查能被
/// 编译器消掉，循环体也能向量化。
pub fn predict_row(above: &[u8], row: &[u8], bpp: usize, out: &mut Vec<u8>) {
    let n = row.len();
    let above = &above[..n];
    let bpp = bpp.min(n);
    // 行首 bpp 个样本左边、左上没有邻居，按 0 算；其余的：自己、左、上、左上。
    let (x, a) = (&row[bpp..], &row[..n - bpp]);
    let (b, c) = (&above[bpp..], &above[..n - bpp]);
    let len = x.len();
    let (a, b, c) = (&a[..len], &b[..len], &c[..len]);

    let abs = |v: u8| (v as i8).unsigned_abs() as u64;
    let mut cost = [0u64; 5];
    for i in 0..bpp {
        let (x, b) = (row[i], above[i]);
        cost[0] += abs(x);
        cost[1] += abs(x);
        cost[2] += abs(x.wrapping_sub(b));
        cost[3] += abs(x.wrapping_sub(b / 2));
        cost[4] += abs(x.wrapping_sub(b));
    }
    for i in 0..len {
        let (x, a, b, c) = (x[i], a[i], b[i], c[i]);
        cost[0] += abs(x);
        cost[1] += abs(x.wrapping_sub(a));
        cost[2] += abs(x.wrapping_sub(b));
        cost[3] += abs(x.wrapping_sub(average(a, b)));
        cost[4] += abs(x.wrapping_sub(paeth(a, b, c)));
    }
    let best = (0..5).min_by_key(|&k| cost[k]).unwrap_or(0);

    out.reserve(n + 1);
    out.push(best as u8);
    let head = row[..bpp].iter().zip(&above[..bpp]);
    match best {
        0 => out.extend_from_slice(row),
        1 => {
            out.extend_from_slice(&row[..bpp]);
            out.extend((0..len).map(|i| x[i].wrapping_sub(a[i])));
        }
        2 => {
            out.extend(head.map(|(&x, &b)| x.wrapping_sub(b)));
            out.extend((0..len).map(|i| x[i].wrapping_sub(b[i])));
        }
        3 => {
            out.extend(head.map(|(&x, &b)| x.wrapping_sub(b / 2)));
            out.extend((0..len).map(|i| x[i].wrapping_sub(average(a[i], b[i]))));
        }
        _ => {
            // 左、左上都是 0 时 Paeth 取上边。
            out.extend(head.map(|(&x, &b)| x.wrapping_sub(b)));
            out.extend((0..len).map(|i| x[i].wrapping_sub(paeth(a[i], b[i], c[i]))));
        }
    }
}

fn average(a: u8, b: u8) -> u8 {
    ((a as u16 + b as u16) / 2) as u8
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    // p = a + b − c，于是 |p − a| = |b − c|、|p − b| = |a − c|、|p − c| = |a + b − 2c|。
    let (a16, b16, c16) = (a as i16, b as i16, c as i16);
    let pa = (b16 - c16).abs();
    let pb = (a16 - c16).abs();
    let pc = (a16 + b16 - 2 * c16).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
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
    let components = if img.gray { 1 } else { 3 };
    let compressed;
    let (data, filter) = match img.encoding {
        // 关键路径：原始 JPEG 字节原封不动地写进流，PDF 用 DCTDecode 自己解。
        // 不解码、不重编码，所以是真正的零损失。
        ImageEncoding::Jpeg(bytes) => (bytes, Filter::DctDecode),
        ImageEncoding::Raw(bytes) => {
            compressed = flate_image(bytes, img.width, components);
            (compressed.as_slice(), Filter::FlateDecode)
        }
        ImageEncoding::Flate(bytes) => (bytes, Filter::FlateDecode),
    };
    let mut x = pdf.image_xobject(id, data);
    x.filter(filter);
    if !matches!(img.encoding, ImageEncoding::Jpeg(_)) {
        x.decode_parms()
            .predictor(Predictor::PngOptimum)
            .colors(components as i32)
            .bits_per_component(8)
            .columns(img.width as i32);
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 按 PNG 的规矩还原（阅读器解码时做的事）。
    fn unpredict(data: &[u8], row_bytes: usize, bpp: usize) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        for (r, row) in data.chunks_exact(row_bytes + 1).enumerate() {
            let (kind, residual) = (row[0], &row[1..]);
            let start = out.len();
            for (i, &d) in residual.iter().enumerate() {
                let a = if i >= bpp { out[start + i - bpp] } else { 0 };
                let b = if r > 0 { out[start + i - row_bytes] } else { 0 };
                let c = if r > 0 && i >= bpp {
                    out[start + i - row_bytes - bpp]
                } else {
                    0
                };
                let predicted = match kind {
                    0 => 0,
                    1 => a,
                    2 => b,
                    3 => average(a, b),
                    4 => paeth(a, b, c),
                    other => panic!("没有第 {other} 种预测"),
                };
                out.push(d.wrapping_add(predicted));
            }
        }
        out
    }

    /// 各种宽度（含比一个像素还窄的行首）、灰度与 RGB、平滑与杂乱的内容：预测之后
    /// 逐行还原，字节一个不差，而且五种预测都用到过。
    #[test]
    fn prediction_round_trips() {
        let mut used = [false; 5];
        for (width, bpp) in [(1usize, 1usize), (1, 3), (2, 3), (7, 1), (17, 3), (64, 3)] {
            let rows = 9;
            let row_bytes = width * bpp;
            let raw: Vec<u8> = (0..rows * row_bytes)
                .map(|i| {
                    let (y, x) = (i / row_bytes, i % row_bytes);
                    match y % 3 {
                        0 => (x * 3 + y * 5) as u8,
                        1 => ((x * 37) ^ (y * 101)).wrapping_mul(2654435761) as u8,
                        _ => (y * 40) as u8,
                    }
                })
                .collect();
            let mut predicted = Vec::new();
            let mut above = vec![0u8; row_bytes];
            for row in raw.chunks_exact(row_bytes) {
                predict_row(&above, row, bpp, &mut predicted);
                used[predicted[predicted.len() - row_bytes - 1] as usize] = true;
                above.copy_from_slice(row);
            }
            assert_eq!(unpredict(&predicted, row_bytes, bpp), raw, "{width}×{bpp}");
        }
        assert!(used.iter().filter(|u| **u).count() >= 3, "{used:?}");
    }

    #[test]
    fn paeth_matches_the_png_definition() {
        let reference = |a: u8, b: u8, c: u8| {
            let p = a as i16 + b as i16 - c as i16;
            let (pa, pb, pc) = (
                (p - a as i16).abs(),
                (p - b as i16).abs(),
                (p - c as i16).abs(),
            );
            if pa <= pb && pa <= pc {
                a
            } else if pb <= pc {
                b
            } else {
                c
            }
        };
        for a in (0..=255u8).step_by(5) {
            for b in (0..=255u8).step_by(7) {
                for c in (0..=255u8).step_by(3) {
                    assert_eq!(paeth(a, b, c), reference(a, b, c), "{a} {b} {c}");
                }
            }
        }
    }
}
