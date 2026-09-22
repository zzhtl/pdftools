//! 不解码整张图的前提下，探测格式、尺寸与物理分辨率。
//!
//! 这一步必须足够轻：多图转 PDF 时要对上百个文件先做决策，
//! 全量解码一遍再决定「要不要解码」是本末倒置。

use std::path::Path;

/// JPEG 的 SOF 段信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JpegInfo {
    pub width: u32,
    pub height: u32,
    /// 1 = 灰度，3 = YCbCr，4 = CMYK/YCCK
    pub components: u8,
}

/// 物理分辨率，单位 DPI。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dpi {
    pub x: f32,
    pub y: f32,
}

impl Dpi {
    fn new(x: f32, y: f32) -> Option<Self> {
        // 0 或负数说明字段没填对，当作没有元数据处理。
        // 上限是为了挡住偶尔出现的荒谬值（见过写 720000 的）。
        (x.is_finite() && y.is_finite() && x > 1.0 && y > 1.0 && x < 10_000.0 && y < 10_000.0)
            .then_some(Self { x, y })
    }
}

/// 识别得出的图像容器格式。只区分我们需要分别处理的几类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Jpeg,
    Png,
    Other,
}

pub fn sniff(bytes: &[u8]) -> Container {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Container::Jpeg
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Container::Png
    } else {
        Container::Other
    }
}

/// 扫描 JPEG 的段结构，取出 SOF 里的尺寸与分量数。
///
/// 只读段头，不碰熵编码数据，因此对几十 MB 的图也是常数级开销。
pub fn jpeg_info(bytes: &[u8]) -> Option<JpegInfo> {
    let mut i = 2; // 跳过 SOI
    while i + 3 < bytes.len() {
        if bytes[i] != 0xFF {
            // 段边界对不上，文件有问题，不猜。
            return None;
        }
        let marker = bytes[i + 1];
        // 填充字节：连续的 FF 要跳过
        if marker == 0xFF {
            i += 1;
            continue;
        }
        // 无长度字段的标记
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        if marker == 0xD9 || marker == 0xDA {
            // EOI / SOS：到这里还没见到 SOF，说明文件不正常
            return None;
        }
        let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
        if len < 2 {
            return None;
        }
        // SOF0/1/2/3/5/6/7/9/A/B/D/E/F 都带尺寸；C4(DHT)、C8(JPG)、CC(DAC) 不是 SOF
        let is_sof = matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if is_sof {
            let p = i + 4;
            if p + 5 >= bytes.len() {
                return None;
            }
            return Some(JpegInfo {
                height: u16::from_be_bytes([bytes[p + 1], bytes[p + 2]]) as u32,
                width: u16::from_be_bytes([bytes[p + 3], bytes[p + 4]]) as u32,
                components: bytes[p + 5],
            });
        }
        i += 2 + len;
    }
    None
}

/// JFIF APP0 段里的密度信息。
fn jfif_dpi(bytes: &[u8]) -> Option<Dpi> {
    let mut i = 2;
    while i + 3 < bytes.len() {
        if bytes[i] != 0xFF {
            return None;
        }
        let marker = bytes[i + 1];
        if marker == 0xFF {
            i += 1;
            continue;
        }
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        if marker == 0xD9 || marker == 0xDA {
            return None;
        }
        let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
        if len < 2 {
            return None;
        }
        if marker == 0xE0 {
            let p = i + 4;
            if p + 11 < bytes.len() && &bytes[p..p + 5] == b"JFIF\0" {
                let units = bytes[p + 7];
                let xd = u16::from_be_bytes([bytes[p + 8], bytes[p + 9]]) as f32;
                let yd = u16::from_be_bytes([bytes[p + 10], bytes[p + 11]]) as f32;
                return match units {
                    1 => Dpi::new(xd, yd),               // dots/inch
                    2 => Dpi::new(xd * 2.54, yd * 2.54), // dots/cm
                    _ => None,                           // 0 = 只表示像素宽高比，不是物理分辨率
                };
            }
        }
        i += 2 + len;
    }
    None
}

/// PNG 的 pHYs 块。它不属于 EXIF，exif crate 读不到，只能自己扫。
fn png_dpi(bytes: &[u8]) -> Option<Dpi> {
    let mut i = 8; // 跳过签名
    while i + 8 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[i..i + 4].try_into().ok()?) as usize;
        let kind = &bytes[i + 4..i + 8];
        if kind == b"pHYs" {
            let p = i + 8;
            if p + 9 > bytes.len() {
                return None;
            }
            let px = u32::from_be_bytes(bytes[p..p + 4].try_into().ok()?) as f32;
            let py = u32::from_be_bytes(bytes[p + 4..p + 8].try_into().ok()?) as f32;
            // unit 1 = 米；0 表示只有宽高比，没有物理含义
            return (bytes[p + 8] == 1).then(|| Dpi::new(px * 0.0254, py * 0.0254))?;
        }
        if kind == b"IDAT" || kind == b"IEND" {
            return None; // pHYs 必须在 IDAT 之前，走到这里说明没有
        }
        i += 12 + len; // len + type(4) + data + crc(4)
    }
    None
}

/// 从 EXIF 字节里读 XResolution / YResolution / ResolutionUnit。
fn exif_dpi(raw: &[u8]) -> Option<Dpi> {
    let exif = exif::Reader::new().read_raw(raw.to_vec()).ok()?;
    let get = |tag| {
        exif.get_field(tag, exif::In::PRIMARY)
            .and_then(|f| match &f.value {
                exif::Value::Rational(v) => v.first().map(|r| r.to_f64() as f32),
                _ => None,
            })
    };
    let x = get(exif::Tag::XResolution)?;
    let y = get(exif::Tag::YResolution).unwrap_or(x);
    // ResolutionUnit: 2 = 英寸，3 = 厘米。缺省按英寸。
    let unit = exif
        .get_field(exif::Tag::ResolutionUnit, exif::In::PRIMARY)
        .and_then(|f| f.value.get_uint(0))
        .unwrap_or(2);
    match unit {
        3 => Dpi::new(x * 2.54, y * 2.54),
        _ => Dpi::new(x, y),
    }
}

/// 按「容器原生字段 → EXIF」的顺序取 DPI。
///
/// 顺序是有意的：JFIF/pHYs 是容器自己的字段，比 EXIF 里那份更可能被写图的工具正确维护。
pub fn dpi(bytes: &[u8], exif_raw: Option<&[u8]>) -> Option<Dpi> {
    let native = match sniff(bytes) {
        Container::Jpeg => jfif_dpi(bytes),
        Container::Png => png_dpi(bytes),
        Container::Other => None,
    };
    native.or_else(|| exif_raw.and_then(exif_dpi))
}

/// 从扩展名猜测是否是我们支持的图片。用于文件对话框过滤与拖拽筛选。
pub fn looks_like_image(path: &Path) -> bool {
    const EXTS: &[&str] = &[
        "jpg", "jpeg", "png", "gif", "bmp", "tif", "tiff", "webp", "avif", "ico", "tga", "pnm",
        "ppm", "pgm", "pbm", "qoi", "dds", "ff", "exr", "hdr",
    ];
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// HEIC/HEIF 我们不支持，但要能认出来，好给用户一句有用的提示而不是「解码失败」。
pub fn is_heif(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("heic" | "heif" | "hif")
    )
}

/// 从 EXIF 里读拍摄时间。
///
/// 优先级 `DateTimeOriginal`（按下快门的时刻）→ `DateTimeDigitized`（数字化时刻）
/// → `DateTime`（文件最后修改，最不可信）。时区取 `OffsetTimeOriginal`，没有就留空 ——
/// 硬套一个本地时区是在编造信息。
pub fn capture_time(exif_raw: &[u8]) -> Option<crate::timestamp::Timestamp> {
    let exif = exif::Reader::new().read_raw(exif_raw.to_vec()).ok()?;

    let read = |tag| {
        exif.get_field(tag, exif::In::PRIMARY)
            .and_then(|f| match &f.value {
                exif::Value::Ascii(v) => v.first().and_then(|b| exif::DateTime::from_ascii(b).ok()),
                _ => None,
            })
    };

    let mut dt = read(exif::Tag::DateTimeOriginal)
        .or_else(|| read(exif::Tag::DateTimeDigitized))
        .or_else(|| read(exif::Tag::DateTime))?;

    // 时区是独立的一个 tag，DateTime::from_ascii 拿不到。
    if let Some(offset) = exif
        .get_field(exif::Tag::OffsetTimeOriginal, exif::In::PRIMARY)
        .or_else(|| exif.get_field(exif::Tag::OffsetTime, exif::In::PRIMARY))
        .and_then(|f| match &f.value {
            exif::Value::Ascii(v) => v.first().cloned(),
            _ => None,
        })
    {
        let _ = dt.parse_offset(&offset);
    }

    Some(crate::timestamp::Timestamp::from_exif(&dt))
}
