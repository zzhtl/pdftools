//! 一份文档的字体簿：把文档里写的字体名解析成本机真实存在的字体，
//! 解析不到就按类别回退，并把每一次替换、缺字都记下来。
//!
//! 字体本身来自进程共享的 [`SystemFonts`]；这里只管本文档用到了哪些、
//! 给它们编号（PDF 资源与子集化按编号来），以及要向用户报告什么。

use std::collections::HashMap;
use std::sync::Arc;

use super::system::{self, SystemFonts};
use super::{Embedding, FontFace};
use crate::error::{Warning, WarningKind};

/// 本文档内的字体编号。
pub type FontId = usize;

/// 一次字体请求的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved {
    pub id: FontId,
    /// 要的是粗体，字体却没有真粗体（宋体就没有）—— 绘制时要合成，Word 也是这么做的。
    pub synthetic_bold: bool,
    /// 要的是斜体，字体却没有斜体字形 —— 绘制时要合成。
    pub synthetic_italic: bool,
}

pub struct FontBook {
    system: &'static SystemFonts,
    faces: Vec<Arc<FontFace>>,
    cache: HashMap<(String, bool, bool), Option<Resolved>>,
    substituted: Vec<String>,
    /// 所选字体（含回退字体）里都没有字形的字符。
    missing: Vec<char>,
    /// 系统里连一个可用字体都找不到。
    no_font_at_all: bool,
}

/// 字体名看起来是不是衬线体。用于挑回退链。
fn looks_serif(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    is_serif_cjk(name)
        || [
            "times", "serif", "georgia", "garamond", "book", "roman", "song", "ming", "kai",
        ]
        .iter()
        .any(|k| lower.contains(k))
}

/// 常见中文字体名 → 我们的回退链类别。
fn is_serif_cjk(name: &str) -> bool {
    matches!(
        name,
        "宋体"
            | "SimSun"
            | "NSimSun"
            | "新宋体"
            | "仿宋"
            | "FangSong"
            | "仿宋_GB2312"
            | "楷体"
            | "KaiTi"
            | "楷体_GB2312"
            | "STSong"
            | "Songti SC"
            | "STFangsong"
            | "Source Han Serif SC"
            | "Noto Serif CJK SC"
    )
}

impl FontBook {
    pub fn new() -> Self {
        Self {
            system: SystemFonts::shared(),
            faces: Vec::new(),
            cache: HashMap::new(),
            substituted: Vec::new(),
            missing: Vec::new(),
            no_font_at_all: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_system(system: &'static SystemFonts) -> Self {
        Self {
            system,
            ..Self::new()
        }
    }

    pub fn face(&self, id: FontId) -> &FontFace {
        &self.faces[id]
    }

    pub fn faces(&self) -> &[Arc<FontFace>] {
        &self.faces
    }

    /// 同一个字体面只编一个号：「宋体」和「SimSun」都落到 Noto Serif CJK 时，
    /// PDF 里应当只嵌一份子集。
    fn intern(&mut self, face: Arc<FontFace>) -> FontId {
        if let Some(i) = self.faces.iter().position(|f| Arc::ptr_eq(f, &face)) {
            return i;
        }
        self.faces.push(face);
        self.faces.len() - 1
    }

    /// 解析一个字体请求。`east_asian` 决定回退链，因为同一个 run 的汉字和
    /// 西文要走不同的字体（`w:rFonts` 本来就给了两个名字）。
    pub fn resolve(
        &mut self,
        family: Option<&str>,
        east_asian: bool,
        bold: bool,
        italic: bool,
    ) -> Option<Resolved> {
        let key = (
            family.unwrap_or("").to_string() + if east_asian { "|ea" } else { "|latin" },
            bold,
            italic,
        );
        if let Some(hit) = self.cache.get(&key) {
            return *hit;
        }

        // 先按原名精确找。找到就用，这是最忠实于文档的结果 —— 除非作者禁止内嵌。
        let mut restricted = false;
        let mut found = family
            .and_then(|f| self.system.query(f, bold, italic))
            .filter(|f| {
                restricted = f.face.metrics().embedding == Embedding::Restricted;
                !restricted
            });

        if found.is_none() {
            // 找不到就按类别回退，并记下这次替换 —— 用户有权知道字体被换了。
            // 哪一级都没写字体时按衬线体：Word 的缺省是 Times New Roman 与宋体，
            // LibreOffice 读 docx 时也用 Liberation Serif 与 Noto Serif CJK。
            let chain = if east_asian {
                if family.is_none_or(is_serif_cjk) {
                    system::PDF_SERIF_PREFERENCE
                } else {
                    system::PDF_SANS_PREFERENCE
                }
            } else if family.is_none_or(looks_serif) {
                system::LATIN_SERIF_PREFERENCE
            } else {
                system::LATIN_SANS_PREFERENCE
            };
            found = self.system.find_embeddable(chain, bold, italic);
            if found.is_none() {
                // 首选链全军覆没时，把其余所有链都试一遍，最后退到系统里任意一个字体。
                //
                // 这一步守的是一条底线：**绝不因为找不到字体就把文字丢掉**。
                // 哪怕最终字体缺少对应字形（显示为空白），文字也仍在 PDF 里、仍可搜索，
                // 而且缺字检测会把这件事报出来。悄悄少一整段字要严重得多。
                for alt in [
                    system::PDF_SANS_PREFERENCE,
                    system::PDF_SERIF_PREFERENCE,
                    system::LATIN_SANS_PREFERENCE,
                    system::LATIN_SERIF_PREFERENCE,
                ] {
                    found = self.system.find_embeddable(alt, bold, italic);
                    if found.is_some() {
                        break;
                    }
                }
            }
            if found.is_none() {
                found = self.system.any_face(bold, italic);
            }
            if let (Some(want), Some(got)) = (family, found.as_ref()) {
                let note = if restricted {
                    format!(
                        "字体「{want}」的作者禁止内嵌到文件里，已替换为「{}」",
                        got.family
                    )
                } else {
                    format!("字体「{want}」不可用，已替换为「{}」", got.family)
                };
                if !self.substituted.contains(&note) {
                    self.substituted.push(note);
                }
            }
        }

        let resolved = found.map(|f| {
            let (synthetic_bold, synthetic_italic) = synthesis(f.face.as_ref(), bold, italic);
            Resolved {
                id: self.intern(f.face),
                synthetic_bold,
                synthetic_italic,
            }
        });
        self.cache.insert(key, resolved);
        resolved
    }

    /// 主字体里没有 `c` 的字形时，找一个有的。结果要按请求的粗斜体再判一次是否合成。
    pub fn fallback(
        &mut self,
        c: char,
        east_asian: bool,
        bold: bool,
        italic: bool,
    ) -> Option<Resolved> {
        let face = self.system.fallback_for(c, east_asian)?;
        let (synthetic_bold, synthetic_italic) = synthesis(face.as_ref(), bold, italic);
        Some(Resolved {
            id: self.intern(face),
            synthetic_bold,
            synthetic_italic,
        })
    }

    /// 本机有没有这个字体（不管能否内嵌）。
    pub fn has_family(&self, family: &str) -> bool {
        self.system.query(family, false, false).is_some()
    }

    pub fn note_missing(&mut self, c: char) {
        if !self.missing.contains(&c) {
            self.missing.push(c);
        }
    }

    pub fn note_no_font(&mut self) {
        self.no_font_at_all = true;
    }

    pub fn take_warnings(&mut self) -> Vec<Warning> {
        let mut out: Vec<Warning> = self
            .substituted
            .drain(..)
            .map(|d| Warning::new(WarningKind::FontSubstituted, d))
            .collect();
        if self.no_font_at_all {
            out.push(Warning::new(
                WarningKind::FontSubstituted,
                "系统中找不到任何可用字体，部分内容无法排版。请安装 Noto Sans CJK 或思源黑体。",
            ));
        }
        if !self.missing.is_empty() {
            let chars: String = self.missing.drain(..).take(40).collect();
            out.push(Warning::new(
                WarningKind::FontSubstituted,
                format!("以下字符在可用字体中没有字形，PDF 里会显示为空白：{chars}"),
            ));
        }
        out
    }
}

impl Default for FontBook {
    fn default() -> Self {
        Self::new()
    }
}

/// 要不要合成粗体、斜体。fontdb 找不到粗体时会（按 CSS 的规则）退到常规字重，
/// 所以要看拿到的字体实际是什么字重、有没有斜体。
fn synthesis(face: &FontFace, bold: bool, italic: bool) -> (bool, bool) {
    let m = face.metrics();
    (
        bold && m.weight < 600,
        italic && !m.is_italic && m.italic_angle == 0.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 把一份 TrueType 字体 OS/2 表里的 fsType 改成「禁止内嵌」。
    fn restricted_copy(data: &[u8]) -> Option<Vec<u8>> {
        let mut out = data.to_vec();
        let tables = u16::from_be_bytes([data[4], data[5]]) as usize;
        let os2 = (0..tables).find_map(|i| {
            let rec = 12 + i * 16;
            (&data[rec..rec + 4] == b"OS/2")
                .then(|| u32::from_be_bytes(data[rec + 8..rec + 12].try_into().unwrap()) as usize)
        })?;
        // fsType 在 version、xAvgCharWidth、usWeightClass、usWidthClass 之后。
        out[os2 + 8..os2 + 10].copy_from_slice(&2u16.to_be_bytes());
        Some(out)
    }

    #[test]
    fn restricted_fonts_are_skipped_at_resolve_time() {
        let shared = SystemFonts::shared();
        let (Some(sans), Some(serif)) = (
            shared.query("DejaVu Sans", false, false),
            shared.query("DejaVu Serif", false, false),
        ) else {
            eprintln!("跳过：本机没有 DejaVu 字体");
            return;
        };
        let Some(locked) = restricted_copy(sans.face.data()) else {
            return;
        };
        let mut db = fontdb::Database::new();
        db.load_font_data(locked);
        db.load_font_data(serif.face.data().to_vec());
        let system: &'static SystemFonts = Box::leak(Box::new(SystemFonts::from_database(db)));
        let mut book = FontBook::with_system(system);

        let got = book
            .resolve(Some("DejaVu Sans"), false, false, false)
            .expect("应当回退到能内嵌的字体");
        assert_eq!(
            book.face(got.id).metrics().embedding,
            Embedding::Allowed,
            "选中了禁止内嵌的字体 —— 到嵌入那一步整份转换会失败"
        );
        let notes: Vec<String> = book.take_warnings().into_iter().map(|w| w.detail).collect();
        assert!(
            notes
                .iter()
                .any(|n| n.contains("DejaVu Sans") && n.contains("禁止内嵌")),
            "要说明字体因禁止内嵌被替换：{notes:?}"
        );
    }

    /// 哪一级都没写字体时按衬线体回退，与按「Times New Roman」「宋体」回退的
    /// 首选一样。
    #[test]
    fn unnamed_fonts_fall_back_to_serif() {
        let fonts = SystemFonts::shared();
        let mut book = FontBook::with_system(fonts);
        let chains = [
            (false, system::LATIN_SERIF_PREFERENCE),
            (true, system::PDF_SERIF_PREFERENCE),
        ];
        for (east_asian, chain) in chains {
            let Some(serif) = fonts.find_embeddable(chain, false, false) else {
                continue;
            };
            let got = book.resolve(None, east_asian, false, false).unwrap();
            assert!(Arc::ptr_eq(&book.faces()[got.id], &serif.face));
        }
    }
}
