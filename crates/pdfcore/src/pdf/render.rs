//! 把 PDF 页面画成位图。PDF 转图片与页面缩略图共用。
//!
//! 用 hayro 渲染：纯 Rust，没有 C 依赖，三平台行为一致。PDF 没嵌进去的中日韩字体
//! （公文里常见「宋体」只写了个名字）到系统字体里找：先按 PDF 里写的名字，找不到再
//! 按宋体类、黑体类退到中文回退链。hayro 自带的只有 14 种西文标准字体，不接这一步，
//! 没嵌字体的中文整段空白。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hayro::hayro_interpret::font::{FallbackFontQuery, FontData, FontQuery};
use hayro::hayro_interpret::hayro_cmap::CidFamily;
use hayro::hayro_interpret::{InterpreterSettings, InterpreterWarning};
use hayro::hayro_syntax::{LoadPdfError, Pdf};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{RenderCache, RenderSettings};

use crate::error::{CoreError, Result};
use crate::fonts::system::{Found, SystemFonts, PDF_SANS_PREFERENCE, PDF_SERIF_PREFERENCE};

/// 一页最多画多少像素。A4 在 600 DPI 下约 3500 万；更大的（海报、工程图）降低分辨率，
/// 不然几个线程同时画，内存要以 GB 计。
const MAX_PIXELS: f32 = 64.0e6;
/// hayro 画布的边长是 u16。
const MAX_SIDE: f32 = 65_535.0;

/// 打开的 PDF。
pub struct Document {
    pdf: Pdf,
    settings: InterpreterSettings,
    notes: Arc<Mutex<Notes>>,
}

/// 画的过程中值得告诉用户的事。
#[derive(Debug, Default, Clone)]
pub struct Notes {
    /// 没嵌进 PDF、换成系统字体显示的：PDF 里写的名字 → 实际用的字体。
    pub substituted: BTreeMap<String, String>,
    /// 没嵌进 PDF、系统里也没有能显示它的中日韩字体：这些字会是空白。
    pub missing: Vec<String>,
    /// hayro 还不支持的字体（非 Identity 编码的 CID 字体）。
    pub unsupported_font: bool,
    /// 解不开的图片，画成了空白。
    pub broken_image: bool,
}

/// 每个线程一份：解析过的字体、图片在同一个线程里跨页复用。
pub struct Cache<'a>(RenderCache<'a>);

impl Document {
    pub fn open(data: Vec<u8>) -> Result<Self> {
        let pdf = Pdf::new(data).map_err(|e| match e {
            LoadPdfError::Decryption(_) => {
                CoreError::Unsupported("这份 PDF 加了密，暂不支持".into())
            }
            LoadPdfError::Invalid => CoreError::Pdf("无法解析这份 PDF".into()),
        })?;
        let notes = Arc::new(Mutex::new(Notes::default()));
        let fonts_notes = notes.clone();
        let warn_notes = notes.clone();
        let settings = InterpreterSettings {
            font_resolver: Arc::new(move |q| resolve_font(q, &fonts_notes)),
            warning_sink: Arc::new(move |w| {
                let mut n = warn_notes.lock().unwrap_or_else(|e| e.into_inner());
                match w {
                    InterpreterWarning::UnsupportedFont => n.unsupported_font = true,
                    InterpreterWarning::ImageDecodeFailure => n.broken_image = true,
                }
            }),
            ..InterpreterSettings::default()
        };
        Ok(Self {
            pdf,
            settings,
            notes,
        })
    }

    pub fn page_count(&self) -> usize {
        self.pdf.pages().len()
    }

    /// 第 `index` 页（从 0 开始）显示出来的宽高，单位点：按裁剪框、转过 `/Rotate` 之后的。
    pub fn page_size(&self, index: usize) -> (f32, f32) {
        self.pdf.pages()[index].render_dimensions()
    }

    pub fn cache(&self) -> Cache<'_> {
        Cache(RenderCache::new())
    }

    /// 把第 `index` 页画成白底 RGB，`scale` = 1 时 1 点 1 像素（72 DPI）。页面太大时
    /// 实际用的比例会降下来，见 [`fit_scale`]。
    pub fn render<'a>(&'a self, index: usize, scale: f32, cache: &Cache<'a>) -> image::RgbImage {
        let page = &self.pdf.pages()[index];
        let (w, h) = page.render_dimensions();
        let scale = fit_scale((w, h), scale);
        // 画布尺寸自己定：退化的页面（宽或高为 0）也至少给一个像素，不让编码器报错。
        let px = |v: f32| (v * scale).floor().clamp(1.0, MAX_SIDE) as u16;
        let settings = RenderSettings {
            x_scale: scale,
            y_scale: scale,
            width: Some(px(w)),
            height: Some(px(h)),
            bg_color: WHITE,
        };
        let pix = hayro::render(page, &cache.0, &self.settings, &settings);
        let (pw, ph) = (u32::from(pix.width()), u32::from(pix.height()));
        // 白底不透明，预乘与否结果一样，直接去掉 alpha。
        let mut rgb = Vec::with_capacity(pw as usize * ph as usize * 3);
        for p in pix.data_as_u8_slice().as_chunks::<4>().0 {
            rgb.extend_from_slice(&p[..3]);
        }
        image::RgbImage::from_raw(pw, ph, rgb).expect("像素数与画布尺寸一致")
    }

    /// 到目前为止画过的页里遇到的事。
    pub fn notes(&self) -> Notes {
        self.notes.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// 页面按 `scale` 画会不会太大：太大就返回降下来的比例，否则原样返回。
pub fn fit_scale((w, h): (f32, f32), scale: f32) -> f32 {
    let (w, h) = (w.max(1.0), h.max(1.0));
    let by_area = (MAX_PIXELS / (w * h)).sqrt();
    let by_side = MAX_SIDE / w.max(h);
    scale.min(by_area).min(by_side)
}

/// hayro 找不到字体时来问。标准 14 字体用它自带的；其余的先按名字在系统里找，
/// 中日韩字体再退到宋体或黑体的回退链，西文字体退到 hayro 自带的标准字体。
fn resolve_font(query: &FontQuery, notes: &Mutex<Notes>) -> Option<(FontData, u32)> {
    let q = match query {
        FontQuery::Standard(font) => return Some(font.get_font_data()),
        FontQuery::Fallback(q) => q,
    };
    let fonts = SystemFonts::shared();
    let name = requested_name(q);
    let cjk = is_cjk(q, &name);
    let found = by_name(fonts, q).or_else(|| {
        if !cjk {
            return None;
        }
        let (first, second) = if looks_serif(&name, q.is_serif) {
            (PDF_SERIF_PREFERENCE, PDF_SANS_PREFERENCE)
        } else {
            (PDF_SANS_PREFERENCE, PDF_SERIF_PREFERENCE)
        };
        fonts
            .find(first, q.is_bold, q.is_italic)
            .or_else(|| fonts.find(second, q.is_bold, q.is_italic))
    });
    let mut notes = notes.lock().unwrap_or_else(|e| e.into_inner());
    match found {
        Some(found) => {
            if normalized(&found.family) != normalized(&name) {
                notes.substituted.insert(name, found.family.clone());
            }
            let data: FontData = Arc::new(found.face.data());
            Some((data, found.face.index()))
        }
        None if cjk => {
            if !notes.missing.contains(&name) {
                notes.missing.push(name);
            }
            None
        }
        None => Some(q.pick_standard_font().get_font_data()),
    }
}

/// PDF 里写的字体名，去掉子集前缀之后的。
fn requested_name(q: &FallbackFontQuery) -> String {
    [&q.post_script_name, &q.font_name, &q.font_family]
        .into_iter()
        .flatten()
        .find(|n| !n.is_empty())
        .cloned()
        .unwrap_or_else(|| "（无名字体）".into())
}

/// 按 PDF 里写的名字找系统字体：PostScript 名、族名，以及去掉 `,Bold`、`-Bold`
/// 这类后缀的基名。
fn by_name(fonts: &SystemFonts, q: &FallbackFontQuery) -> Option<Found> {
    let names: Vec<&str> = [&q.post_script_name, &q.font_name, &q.font_family]
        .into_iter()
        .flatten()
        .map(String::as_str)
        .filter(|n| !n.is_empty())
        .collect();
    names
        .iter()
        .find_map(|n| fonts.by_postscript_name(n))
        .or_else(|| {
            names.iter().find_map(|n| {
                let base = n.split([',', '-']).next().unwrap_or(n);
                fonts
                    .query(n, q.is_bold, q.is_italic)
                    .or_else(|| fonts.query(base, q.is_bold, q.is_italic))
            })
        })
}

/// 中日韩字体：CID 字符集是中日韩的，或者名字看得出来。
fn is_cjk(q: &FallbackFontQuery, name: &str) -> bool {
    const HINTS: &[&str] = &[
        "simsun",
        "simhei",
        "simkai",
        "simfang",
        "kaiti",
        "fangsong",
        "yahei",
        "dengxian",
        "stsong",
        "stheiti",
        "stkaiti",
        "stfangsong",
        "songti",
        "heiti",
        "mingliu",
        "mincho",
        "msgothic",
        "batang",
        "gulim",
        "dotum",
        "malgun",
        "hiragino",
        "pingfang",
        "cjk",
        "sourcehan",
        "wenquanyi",
    ];
    let collection = q.character_collection.as_ref().is_some_and(|cc| {
        matches!(
            cc.family,
            CidFamily::AdobeGB1
                | CidFamily::AdobeCNS1
                | CidFamily::AdobeJapan1
                | CidFamily::AdobeKorea1
        )
    });
    let n = normalized(name);
    collection || !name.is_ascii() || HINTS.iter().any(|h| n.contains(h))
}

/// 该按宋体类（衬线）还是黑体类找替代：名字里看得出来就按名字，否则看字体描述里的
/// 衬线标志。
fn looks_serif(name: &str, flagged_serif: bool) -> bool {
    let n = normalized(name);
    if ["hei", "gothic", "sans", "dengxian", "pingfang"]
        .iter()
        .any(|k| n.contains(k))
        || name.contains('黑')
    {
        return false;
    }
    if ["song", "sun", "ming", "kai", "fang", "serif", "batang"]
        .iter()
        .any(|k| n.contains(k))
        || ['宋', '楷', '明'].iter().any(|c| name.contains(*c))
    {
        return true;
    }
    flagged_serif
}

/// 比较字体名用：小写，去掉空格、连字符、下划线。
fn normalized(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, ' ' | '-' | '_'))
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn huge_pages_are_scaled_down() {
        // A4 在 600 DPI 下不用降。
        let a4 = (595.0, 842.0);
        assert_eq!(fit_scale(a4, 600.0 / 72.0), 600.0 / 72.0);
        // A0 在 600 DPI 下超过像素上限：按面积降到上限以内。
        let a0 = (2384.0, 3370.0);
        let s = fit_scale(a0, 600.0 / 72.0);
        assert!(s < 600.0 / 72.0);
        assert!(a0.0 * s * a0.1 * s <= MAX_PIXELS * 1.001);
        // 细长条：按边长降。
        let strip = (14_400.0, 10.0);
        assert!(14_400.0 * fit_scale(strip, 10.0) <= MAX_SIDE);
    }

    #[test]
    fn song_and_hei_are_told_apart_by_name() {
        assert!(looks_serif("SimSun", false));
        assert!(looks_serif("FangSong_GB2312", false));
        assert!(looks_serif("宋体", false));
        assert!(!looks_serif("SimHei", true));
        assert!(!looks_serif("MicrosoftYaHei", true));
        assert!(looks_serif("Unknown", true));
        assert!(!looks_serif("Unknown", false));
    }

    #[test]
    fn cjk_fonts_are_recognised_by_name_or_collection() {
        let q = |name: &str| FallbackFontQuery {
            post_script_name: Some(name.into()),
            ..Default::default()
        };
        assert!(is_cjk(&q("SimSun"), "SimSun"));
        assert!(is_cjk(&q("MicrosoftYaHei-Bold"), "MicrosoftYaHei-Bold"));
        assert!(is_cjk(&q("方正小标宋简体"), "方正小标宋简体"));
        assert!(!is_cjk(&q("ArialMT"), "ArialMT"));
    }
}
