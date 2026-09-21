//! 系统字体发现。
//!
//! 这里刻意不硬编码字体文件路径：三平台的路径各不相同，而且 macOS 13 之后
//! 一部分系统字体只存在于 AssetsV2 里，写死路径必然在某些机器上落空。
//! `fontdb` 会扫描各平台的标准字体目录并解析 name 表，同时正确处理 `.ttc` 的 face index。

use std::sync::Arc;

use super::FontFace;

/// 一次发现结果：字体文件加上它在 `.ttc` 里的 face 下标。
pub struct Found {
    pub face: FontFace,
    pub family: String,
}

pub struct SystemFonts {
    db: fontdb::Database,
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

impl SystemFonts {
    pub fn load() -> Self {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        Self { db }
    }

    /// 按给定的优先级列表找第一个能用的字体。
    pub fn find(&self, preference: &[&str], bold: bool, italic: bool) -> Option<Found> {
        for family in preference {
            if let Some(found) = self.query(family, bold, italic) {
                return Some(found);
            }
        }
        None
    }

    /// 按家族名精确查询。
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
        self.load_face(id, family)
    }

    fn load_face(&self, id: fontdb::ID, family: &str) -> Option<Found> {
        let (source, index) = self.db.face_source(id)?;
        let data: Arc<Vec<u8>> = match source {
            fontdb::Source::Binary(bin) => Arc::new(bin.as_ref().as_ref().to_vec()),
            fontdb::Source::File(path) => Arc::new(std::fs::read(path).ok()?),
            fontdb::Source::SharedFile(_, bin) => Arc::new(bin.as_ref().as_ref().to_vec()),
        };
        let face = FontFace::load(data, index).ok()?;
        Some(Found {
            face,
            family: family.to_string(),
        })
    }

    /// 是否存在任何可用的中文字体。用来在启动时给出一句有用的提示，
    /// 而不是等用户转换完才发现满页豆腐块。
    pub fn has_cjk(&self) -> bool {
        self.find(PDF_SANS_PREFERENCE, false, false).is_some()
            || self.find(PDF_SERIF_PREFERENCE, false, false).is_some()
    }
}
