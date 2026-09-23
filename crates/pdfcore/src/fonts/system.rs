//! 系统字体发现。
//!
//! 这里刻意不硬编码字体文件路径：三平台的路径各不相同，而且 macOS 13 之后
//! 一部分系统字体只存在于 AssetsV2 里，写死路径必然在某些机器上落空。
//! `fontdb` 会扫描各平台的标准字体目录并解析 name 表，同时正确处理 `.ttc` 的 face index。
//!
//! 扫描与载入都很贵（几百个字体文件、单个 CJK 字体二三十 MB），所以整个进程共用一份：
//! [`SystemFonts::shared`] 第一次调用时扫描，字体文件每个只读一次，字体面按 fontdb 的 ID 缓存。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use super::{Embedding, FontFace};

/// 一次发现结果。
pub struct Found {
    pub face: Arc<FontFace>,
    /// 实际选中的族名（不是请求的名字）。字体被替换时要如实告诉用户换成了什么。
    pub family: String,
}

/// None 记的是「查过了、没有」，下次不再重查。
type Memo<K> = Mutex<HashMap<K, Option<Arc<FontFace>>>>;

pub struct SystemFonts {
    db: fontdb::Database,
    /// fontdb ID → 已载入的字体面。None 表示载入失败（坏文件、CFF2……）。
    faces: Memo<fontdb::ID>,
    /// (字符, 偏向东亚字体) → 能显示它的回退字体。None 表示回退链里都没有。
    fallbacks: Memo<(char, bool)>,
}

/// UI 显示用的中文字体优先级。macOS 上 PingFang 的渲染效果明显优于其他选择，
/// 所以它排在最前面。
pub const UI_CJK_PREFERENCE: &[&str] = &[
    "PingFang SC",      // macOS
    "Hiragino Sans GB", // macOS 旧版
    "Microsoft YaHei",  // Windows
    "微软雅黑",
    "Noto Sans CJK SC", // Linux
    "Source Han Sans SC",
    "WenQuanYi Micro Hei",
    "Droid Sans Fallback",
];

/// 嵌入 PDF 时的黑体类优先级。
///
/// 与 UI 的顺序**故意不同**：开源字体排在前面。PingFang 是 Apple 专有字体，
/// 虽然我们会读 fsType 来判断是否允许内嵌，但在有等价开源字体时优先用后者，
/// 用户把 PDF 发出去时少一层顾虑。
pub const PDF_SANS_PREFERENCE: &[&str] = &[
    "Noto Sans CJK SC",
    "Source Han Sans SC",
    "Droid Sans Fallback",
    "WenQuanYi Micro Hei",
    "PingFang SC",
    "Hiragino Sans GB",
    "Microsoft YaHei",
    "微软雅黑",
];

/// 西文衬线回退链。Liberation Serif 与 Times New Roman **度量兼容**
/// （字宽逐字符对齐），所以拿它顶替 Times New Roman 不会让行长变化。
pub const LATIN_SERIF_PREFERENCE: &[&str] = &[
    "Liberation Serif",
    "Tinos",
    "DejaVu Serif",
    "Noto Serif",
    "Times New Roman",
    "Georgia",
];

/// 西文无衬线回退链。Liberation Sans 与 Arial 度量兼容。
pub const LATIN_SANS_PREFERENCE: &[&str] = &[
    "Liberation Sans",
    "Arimo",
    "DejaVu Sans",
    "Noto Sans",
    "Arial",
    "Helvetica",
];

/// 嵌入 PDF 时的宋体类优先级。docx 里绝大多数正文写的是「宋体」。
pub const PDF_SERIF_PREFERENCE: &[&str] = &[
    "Noto Serif CJK SC",
    "Source Han Serif SC",
    "AR PL UMing CN",
    "SimSun",
    "宋体",
    "Songti SC",
    "STSong",
];

/// 主字体缺字时的回退链。先试同类（东亚/西文）字体，再试专门的符号字体 ——
/// ①、☑、✔ 这类字符多半在这里。
const SYMBOL_FALLBACK: &[&str] = &[
    "Noto Sans Symbols2",
    "Noto Sans Symbols",
    "DejaVu Sans",
    "Segoe UI Symbol",
    "Apple Symbols",
    "Symbola",
    "OpenSymbol",
];

static SHARED: OnceLock<SystemFonts> = OnceLock::new();

/// 字体文件路径 → 读进来的字节。每个文件在进程内只读一次、只驻留一份：
/// 字体面借用这些字节，它们必须活到进程结束。
static FILE_BYTES: OnceLock<Mutex<HashMap<PathBuf, &'static [u8]>>> = OnceLock::new();

fn file_bytes(path: &std::path::Path) -> Option<&'static [u8]> {
    let cache = FILE_BYTES.get_or_init(Default::default);
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(b) = cache.get(path) {
        return Some(b);
    }
    let bytes: &'static [u8] = Box::leak(std::fs::read(path).ok()?.into_boxed_slice());
    cache.insert(path.to_path_buf(), bytes);
    Some(bytes)
}

impl SystemFonts {
    /// 进程内共享的系统字体库。第一次调用时扫描系统字体目录，之后直接复用。
    pub fn shared() -> &'static SystemFonts {
        SHARED.get_or_init(Self::load)
    }

    /// 重新扫描一遍系统字体。一般用 [`shared`](Self::shared)；字体文件的字节缓存是全局的，
    /// 多扫几次也不会把同一个文件读进内存多份。
    pub fn load() -> Self {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        Self {
            db,
            faces: Mutex::new(HashMap::new()),
            fallbacks: Mutex::new(HashMap::new()),
        }
    }

    /// 用一个现成的字体库（测试里造的字体）代替系统扫描。
    #[cfg(test)]
    pub(crate) fn from_database(db: fontdb::Database) -> Self {
        Self {
            db,
            faces: Mutex::new(HashMap::new()),
            fallbacks: Mutex::new(HashMap::new()),
        }
    }

    /// 按给定的优先级列表找第一个存在的字体（界面显示用，不管能否内嵌）。
    pub fn find(&self, preference: &[&str], bold: bool, italic: bool) -> Option<Found> {
        preference
            .iter()
            .find_map(|family| self.query(family, bold, italic))
    }

    /// 同 [`find`](Self::find)，但跳过 fsType 禁止内嵌的字体 —— 要写进 PDF 的都走这个。
    pub fn find_embeddable(&self, preference: &[&str], bold: bool, italic: bool) -> Option<Found> {
        preference.iter().find_map(|family| {
            self.query(family, bold, italic)
                .filter(|f| f.face.metrics().embedding != Embedding::Restricted)
        })
    }

    /// 按家族名精确查询。粗体/斜体不存在时，fontdb 按 CSS 的规则退到最接近的字重与字形 ——
    /// 调用方要看返回字体的实际字重，决定要不要合成。
    pub fn query(&self, family: &str, bold: bool, italic: bool) -> Option<Found> {
        let query = fontdb::Query {
            families: &[fontdb::Family::Name(family)],
            weight: if bold {
                fontdb::Weight::BOLD
            } else {
                fontdb::Weight::NORMAL
            },
            stretch: fontdb::Stretch::Normal,
            style: if italic {
                fontdb::Style::Italic
            } else {
                fontdb::Style::Normal
            },
        };
        let id = self.db.query(&query)?;
        let face = self.face(id)?;
        let family = self
            .db
            .face(id)
            .and_then(|info| info.families.first().map(|(name, _)| name.clone()))
            .unwrap_or_else(|| family.to_string());
        Some(Found { face, family })
    }

    /// 按 PostScript 名找。PDF 里记的多半是它（`SimSun`、`MicrosoftYaHei`、`ArialMT`），
    /// 与族名（`Microsoft YaHei`）写法不一样。
    pub fn by_postscript_name(&self, name: &str) -> Option<Found> {
        let info = self.db.faces().find(|f| f.post_script_name == name)?;
        let face = self.face(info.id)?;
        let family = info
            .families
            .first()
            .map_or_else(|| name.to_string(), |(n, _)| n.clone());
        Some(Found { face, family })
    }

    fn face(&self, id: fontdb::ID) -> Option<Arc<FontFace>> {
        let mut faces = self.faces.lock().unwrap_or_else(|e| e.into_inner());
        faces
            .entry(id)
            .or_insert_with(|| {
                let (source, index) = self.db.face_source(id)?;
                let data: &'static [u8] = match source {
                    fontdb::Source::File(path) => file_bytes(&path)?,
                    // 只有从内存载入的字体才会走到这两支。我们不这么用，但也不能 panic。
                    fontdb::Source::Binary(bin) | fontdb::Source::SharedFile(_, bin) => {
                        Box::leak(bin.as_ref().as_ref().to_vec().into_boxed_slice())
                    }
                };
                FontFace::load(data, index).ok().map(Arc::new)
            })
            .clone()
    }

    /// 系统里随便找一个能用的字体。只在所有偏好链都落空时作为最后兜底 ——
    /// 有字体总比整段文字消失强。
    pub fn any_face(&self, bold: bool, italic: bool) -> Option<Found> {
        let mut families: Vec<String> = self
            .db
            .faces()
            .flat_map(|f| f.families.iter().map(|(name, _)| name.clone()))
            .collect();
        families.sort();
        families.dedup();
        families.iter().find_map(|f| {
            self.query(f, bold, italic)
                .filter(|f| f.face.metrics().embedding != Embedding::Restricted)
        })
    }

    /// 能显示 `c` 的回退字体。`east_asian` 决定先试东亚还是西文字体链。
    ///
    /// 只在固定的几条链里找，不遍历全部系统字体：为一个生僻字把几百个字体文件
    /// 都读进内存，代价远大于显示成方框（方框会被如实报告出来）。
    pub fn fallback_for(&self, c: char, east_asian: bool) -> Option<Arc<FontFace>> {
        let key = (c, east_asian);
        if let Some(hit) = self
            .fallbacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
        {
            return hit.clone();
        }
        let (first, second): (&[&[&str]], &[&[&str]]) = if east_asian {
            (
                &[PDF_SERIF_PREFERENCE, PDF_SANS_PREFERENCE],
                &[LATIN_SERIF_PREFERENCE, LATIN_SANS_PREFERENCE],
            )
        } else {
            (
                &[LATIN_SERIF_PREFERENCE, LATIN_SANS_PREFERENCE],
                &[PDF_SERIF_PREFERENCE, PDF_SANS_PREFERENCE],
            )
        };
        let found = first
            .iter()
            .chain(std::iter::once(&SYMBOL_FALLBACK))
            .chain(second.iter())
            .flat_map(|chain| chain.iter())
            .find_map(|family| {
                self.query(family, false, false)
                    .map(|f| f.face)
                    .filter(|face| {
                        face.metrics().embedding != Embedding::Restricted && face.has_glyph(c)
                    })
            });
        self.fallbacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, found.clone());
        found
    }

    /// 是否存在任何可用的中文字体。用来在启动时给出一句有用的提示，
    /// 而不是等用户转换完才发现满页豆腐块。
    pub fn has_cjk(&self) -> bool {
        self.find_embeddable(PDF_SANS_PREFERENCE, false, false)
            .is_some()
            || self
                .find_embeddable(PDF_SERIF_PREFERENCE, false, false)
                .is_some()
    }
}
