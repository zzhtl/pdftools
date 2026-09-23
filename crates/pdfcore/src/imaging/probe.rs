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
    /// SOF 标记的第二个字节：C0 基线、C1 扩展顺序、C2 渐进式、C3 无损、C9 起为算术编码……
    pub sof: u8,
    /// 样本精度（位）。
    pub precision: u8,
}

impl JpegInfo {
    /// 能否原样放进 PDF 的 `/DCTDecode` 流。
    ///
    /// PDF 的 DCTDecode 只要求支持 8 位的 Huffman 编码顺序式与渐进式 JPEG。
    /// 12 位、无损、算术编码的 JPEG 原样塞进去，阅读器打开是空白或报错，
    /// 而且不会有任何人提前发现 —— 这类文件宁可解码重编码，或者明说处理不了。
    pub fn embeddable_in_pdf(&self) -> bool {
        matches!(self.sof, 0xC0..=0xC2) && self.precision == 8
    }
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
    Tiff,
    Other,
}

pub fn sniff(bytes: &[u8]) -> Container {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Container::Jpeg
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Container::Png
    } else if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
        Container::Tiff
    } else {
        Container::Other
    }
}

/// TIFF 里有几页（IFD 链的长度）。解码器只读第一页，其余页必须告诉用户。
pub fn tiff_page_count(bytes: &[u8]) -> Option<usize> {
    let le = match bytes.get(..4)? {
        b"II*\0" => true,
        b"MM\0*" => false,
        _ => return None,
    };
    let u16_at = |at: usize| -> Option<u16> {
        let b: [u8; 2] = bytes.get(at..at + 2)?.try_into().ok()?;
        Some(if le {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let b: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
        Some(if le {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    };
    let mut next = u32_at(4)? as usize;
    let mut pages = 0usize;
    // 上限既防恶意构造的环，也防把一个几万页的传真档案逐页数一遍。
    while next != 0 && pages < 10_000 {
        let entries = u16_at(next)? as usize;
        pages += 1;
        next = u32_at(next + 2 + entries * 12)? as usize;
    }
    Some(pages)
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
                sof: marker,
                precision: bytes[p],
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
        Container::Tiff | Container::Other => None,
    };
    native.or_else(|| exif_raw.and_then(exif_dpi))
}

/// 能解码的图片扩展名。必须与 `Cargo.toml` 里 image crate 打开的 feature 一致 ——
/// 列进来却没有解码器，用户要等到转换时才看到「解码失败」。
pub const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "gif", "bmp", "tif", "tiff", "webp", "ico", "tga", "pnm",
    "ppm", "pgm", "pbm", "pam", "qoi",
];

/// 从扩展名猜测是否是我们支持的图片。用于文件对话框过滤与拖拽筛选。
pub fn looks_like_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
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
/// 只认 `DateTimeOriginal`（按下快门的时刻）与 `DateTimeDigitized`（数字化的时刻，
/// 相机里与前者相同）。IFD0 的 `DateTime` 不算：它是文件最后修改的时刻，
/// 图片软件一保存就改写。时区取 `OffsetTimeOriginal`，没有就留空 ——
/// 硬套一个本地时区是在编造信息。全零之类不合法的值一律当作没有。
pub fn capture_time(exif_raw: &[u8]) -> Option<crate::timestamp::Timestamp> {
    let exif = exif::Reader::new().read_raw(exif_raw.to_vec()).ok()?;

    let read = |tag| {
        exif.get_field(tag, exif::In::PRIMARY)
            .and_then(|f| match &f.value {
                exif::Value::Ascii(v) => v.first().and_then(|b| exif::DateTime::from_ascii(b).ok()),
                _ => None,
            })
            .filter(|dt| crate::timestamp::Timestamp::from_exif(dt).is_plausible())
    };

    let mut dt =
        read(exif::Tag::DateTimeOriginal).or_else(|| read(exif::Tag::DateTimeDigitized))?;

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

    Some(crate::timestamp::Timestamp::from_exif(&dt)).filter(|t| t.is_plausible())
}

/// 从 XMP 里读拍摄时间。
///
/// EXIF 被剥掉之后，XMP 里常常还留着日期 —— 很多「清除元数据」的工具只处理 EXIF。
/// 按可信度依次尝试三个字段，它们的值都是 ISO 8601。
pub fn xmp_capture_time(xmp: &str) -> Option<crate::timestamp::Timestamp> {
    for key in [
        "exif:DateTimeOriginal",
        "photoshop:DateCreated",
        "xmp:CreateDate",
    ] {
        // 既可能写成属性 `key="值"`，也可能写成元素 `<key>值</key>`。
        for (open, close) in [(format!("{key}=\""), "\""), (format!("<{key}>"), "<")] {
            if let Some(i) = xmp.find(&open) {
                let rest = &xmp[i + open.len()..];
                if let Some(j) = rest.find(close) {
                    if let Some(t) = crate::timestamp::Timestamp::parse_iso8601(&rest[..j]) {
                        return Some(t);
                    }
                }
            }
        }
    }
    None
}

/// 从 IPTC-IIM 里读拍摄时间。
///
/// 结构：Photoshop 的 8BIM 资源块 `0x0404` 里装着 IPTC 数据集，
/// 每条是 `0x1C <record> <dataset> <len:u16> <data>`。
/// 我们要的是 record 2 的 55（DateCreated，`CCYYMMDD`）和 60（TimeCreated，`HHMMSS±HHMM`）。
pub fn iptc_capture_time(iptc: &[u8]) -> Option<crate::timestamp::Timestamp> {
    let mut date: Option<&[u8]> = None;
    let mut time: Option<&[u8]> = None;

    let mut i = 0usize;
    while i + 5 <= iptc.len() {
        if iptc[i] != 0x1C {
            i += 1;
            continue;
        }
        let record = iptc[i + 1];
        let dataset = iptc[i + 2];
        let len = u16::from_be_bytes([iptc[i + 3], iptc[i + 4]]) as usize;
        let start = i + 5;
        // 长度最高位置 1 表示扩展长度字段，这种极少见，遇到就放弃。
        if len & 0x8000 != 0 || start + len > iptc.len() {
            break;
        }
        if record == 2 {
            match dataset {
                55 => date = Some(&iptc[start..start + len]),
                60 => time = Some(&iptc[start..start + len]),
                _ => {}
            }
        }
        i = start + len;
    }

    let d = std::str::from_utf8(date?).ok()?;
    if d.len() < 8 {
        return None;
    }
    let num = |s: &str| s.parse::<u32>().ok();
    let (y, mo, da) = (num(&d[0..4])?, num(&d[4..6])?, num(&d[6..8])?);

    // 时间可以缺失，缺了就当 00:00:00。
    let (h, mi, se, off) = match time.and_then(|t| std::str::from_utf8(t).ok()) {
        Some(t) if t.len() >= 6 => {
            let off = if t.len() >= 11 {
                let sign = if t.as_bytes()[6] == b'-' { -1i16 } else { 1 };
                num(&t[7..9])
                    .zip(num(&t[9..11]))
                    .map(|(hh, mm)| sign * (hh as i16 * 60 + mm as i16))
            } else {
                None
            };
            (num(&t[0..2])?, num(&t[2..4])?, num(&t[4..6])?, off)
        }
        _ => (0, 0, 0, None),
    };

    Some(crate::timestamp::Timestamp {
        year: u16::try_from(y).ok()?,
        month: u8::try_from(mo).ok()?,
        day: u8::try_from(da).ok()?,
        hour: u8::try_from(h).ok()?,
        minute: u8::try_from(mi).ok()?,
        second: u8::try_from(se).ok()?,
        utc_offset_minutes: off,
    })
    .filter(|t| t.is_plausible())
}
