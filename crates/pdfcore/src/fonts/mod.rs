//! 字体：发现、度量、整形、子集化。
//!
//! 这一层同时服务于两个互不相干的消费者：
//!   - `docx::layout` 需要字形宽度来断行 —— 此时还没有任何 PDF 存在；
//!   - `pdf::writer` 需要子集字节和度量来嵌入字体。
//!
//! 所以字体**不属于** PDF 写入器。它是一个测量服务。

mod book;
pub mod pua;
mod shape;
mod subset;
pub mod system;

pub use book::{FontBook, FontId, Resolved};
pub use shape::{
    attaches_to_previous, cluster_texts, shape_run, shape_run_with, split_by_script, ScriptClass,
    ShapedGlyph, ShapedRun,
};
pub use subset::{subset_font, GidMap, SubsetFont};

use std::sync::{Arc, Mutex};

use crate::error::{CoreError, Result};

/// 轮廓格式。决定了 PDF 里写 CIDFontType0 还是 CIDFontType2 —— 两者的字典键不一样，
/// 写错了在 Chrome 里可能正常而在 Acrobat 里是白页。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    /// `glyf` 轮廓。对应 /CIDFontType2 + /FontFile2 + /CIDToGIDMap /Identity
    TrueType,
    /// `CFF ` 轮廓（sfntVersion == OTTO）。对应 /CIDFontType0 + /FontFile3 /Subtype /OpenType，
    /// 且**不能**写 /CIDToGIDMap —— 那是 CIDFontType2 专有的键。
    Cff,
}

/// 字体内嵌许可，来自 OS/2 表的 fsType 位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Embedding {
    /// 可以内嵌子集。
    Allowed,
    /// 字体作者禁止内嵌。我们尊重这个标志并换一个字体。
    Restricted,
}

/// 按（文种, 是否字距调整）缓存的整形计划。
type Plans = Mutex<Vec<((rustybuzz::Script, bool), Arc<rustybuzz::ShapePlan>)>>;

/// 一个具体的字体面（face）。`.ttc` 里有多个 face，所以 index 是必需的。
///
/// 解析好的整形器（含 GSUB/GPOS 查找表）与整形计划都缓存在这里：每整形一段文字就
/// 重新解析一遍字体，对上万字形的 CJK 字体是排版里最大的一块开销。
/// 字体面通过 `Arc` 在文档之间、线程之间共享。
pub struct FontFace {
    rb: rustybuzz::Face<'static>,
    data: &'static [u8],
    index: u32,
    metrics: Metrics,
    /// 全名，用于诊断信息。
    pub name: String,
    /// PostScript 名，写进 PDF 的 BaseFont。
    pub postscript_name: String,
    /// 按文种缓存的整形计划。
    plans: Plans,
}

// 字体面要在批量转换的工作线程之间共享。
const _: () = {
    const fn shareable<T: Send + Sync>() {}
    shareable::<FontFace>();
};

/// 字体度量。除 `italic_angle` 外都是字体单位（font units），用时除以 `upem`。
#[derive(Debug, Clone, Copy)]
pub struct Metrics {
    pub upem: u16,
    pub ascender: i16,
    pub descender: i16,
    pub line_gap: i16,
    pub cap_height: i16,
    pub italic_angle: f32,
    pub bbox: [i16; 4],
    pub weight: u16,
    pub is_italic: bool,
    pub flavor: Flavor,
    pub embedding: Embedding,
    pub num_glyphs: u16,
}

impl Metrics {
    /// 默认行高（字体单位）。用于 `w:spacing lineRule="auto"` 的倍数基准。
    ///
    /// 用 hhea 的 ascender/descender/lineGap。这是 Word 计算行距的基准，
    /// 这个数算错的话每份文档的页数都会对不上。
    pub fn default_line_height(&self) -> f32 {
        (self.ascender as f32 - self.descender as f32 + self.line_gap as f32).max(1.0)
    }

    /// 把字体单位换算成 1/1000 em —— PDF 里所有宽度都用这个单位，与 upem 无关。
    pub fn to_pdf_units(&self, v: f32) -> f32 {
        v * 1000.0 / self.upem as f32
    }
}

impl FontFace {
    /// 从一份活得和进程一样长的字体字节载入。
    ///
    /// 系统字体由 [`system::SystemFonts`] 统一读入：每个字体文件在进程内只读一次、
    /// 只驻留一份，所以这里的 `'static` 不会随转换次数增长。
    pub fn load(data: &'static [u8], index: u32) -> Result<Self> {
        let face = ttf_parser::Face::parse(data, index)
            .map_err(|e| CoreError::Font(format!("解析字体失败：{e}")))?;

        let flavor = if face.tables().cff.is_some() {
            Flavor::Cff
        } else if face.tables().glyf.is_some() {
            Flavor::TrueType
        } else {
            // CFF2 或纯位图字体。subsetter 会把 CFF2 转成 TrueType，但我们不冒这个险。
            return Err(CoreError::Font(
                "字体既无 glyf 也无 CFF 轮廓，无法嵌入".into(),
            ));
        };

        let embedding = match face.tables().os2.and_then(|t| t.permissions()) {
            Some(ttf_parser::Permissions::Restricted) => Embedding::Restricted,
            _ => Embedding::Allowed,
        };

        let upem = face.units_per_em();
        let bbox = face.global_bounding_box();
        // name 表里同一个名字常有好几条记录（Mac、Windows 平台各一份）。
        // 取第一条「解得出来」的，而不是第一条 —— Mac 平台的记录 ttf-parser 解不了，
        // 曾因此把 Liberation Serif 写成「Unknown」。
        let name_of = |id: u16| {
            face.names()
                .into_iter()
                .filter(|n| n.name_id == id)
                .find_map(|n| n.to_string())
        };
        let name = name_of(ttf_parser::name_id::FULL_NAME)
            .or_else(|| name_of(ttf_parser::name_id::FAMILY))
            .unwrap_or_else(|| "Unknown".to_string());
        let postscript_name =
            name_of(ttf_parser::name_id::POST_SCRIPT_NAME).unwrap_or_else(|| name.replace(' ', ""));

        let metrics = Metrics {
            upem,
            ascender: face.ascender(),
            descender: face.descender(),
            line_gap: face.line_gap(),
            // capital_height 很多 CJK 字体没有，退化用 ascender 的 0.7 倍，
            // 这个值只影响 FontDescriptor 的观感，不影响排版。
            cap_height: face
                .capital_height()
                .unwrap_or((face.ascender() as f32 * 0.7) as i16),
            italic_angle: face.italic_angle(),
            bbox: [bbox.x_min, bbox.y_min, bbox.x_max, bbox.y_max],
            weight: face.weight().to_number(),
            is_italic: face.is_italic(),
            flavor,
            embedding,
            num_glyphs: face.number_of_glyphs(),
        };

        Ok(Self {
            rb: rustybuzz::Face::from_face(face),
            data,
            index,
            metrics,
            name,
            postscript_name,
            plans: Mutex::new(Vec::new()),
        })
    }

    /// 从一段字节载入（测试、界面内置字体用）。字节会被留存到进程结束，
    /// 所以不要对同一份字体反复调用 —— 系统字体请走 [`system::SystemFonts`]。
    pub fn from_bytes(data: Vec<u8>, index: u32) -> Result<Self> {
        Self::load(Box::leak(data.into_boxed_slice()), index)
    }

    pub fn data(&self) -> &'static [u8] {
        self.data
    }

    pub fn index(&self) -> u32 {
        self.index
    }

    pub fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    pub fn ttf(&self) -> &ttf_parser::Face<'static> {
        &self.rb
    }

    pub(crate) fn shaper(&self) -> &rustybuzz::Face<'static> {
        &self.rb
    }

    /// 某个文种的整形计划（从左到右）。第一次用到时编排，之后复用。
    /// `kern` 为假时关掉字距调整（OpenType 的 `kern` 特性，连同旧式 kern 表）。
    pub(crate) fn plan(&self, script: rustybuzz::Script, kern: bool) -> Arc<rustybuzz::ShapePlan> {
        let mut plans = self.plans.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, p)) = plans.iter().find(|(k, _)| *k == (script, kern)) {
            return p.clone();
        }
        let no_kern = [rustybuzz::Feature::new(
            rustybuzz::ttf_parser::Tag::from_bytes(b"kern"),
            0,
            ..,
        )];
        let plan = Arc::new(rustybuzz::ShapePlan::new(
            &self.rb,
            rustybuzz::Direction::LeftToRight,
            Some(script),
            None,
            if kern { &[] } else { &no_kern },
        ));
        plans.push(((script, kern), plan.clone()));
        plan
    }

    /// hmtx 里的水平步进，**不是**整形后的 x_advance。
    ///
    /// 这个区别是要命的：PDF 的 /W 数组定义的就是绘制操作符施加的步进，
    /// GPOS 的 kerning 调整必须通过 TJ 的数字偏移表达。
    /// 把 GPOS 烘进 /W，该字形在所有上下文里都会被重复施加一次调整。
    pub fn advance(&self, gid: u16) -> u16 {
        self.ttf()
            .glyph_hor_advance(ttf_parser::GlyphId(gid))
            .unwrap_or(0)
    }

    pub fn has_glyph(&self, c: char) -> bool {
        self.ttf().glyph_index(c).is_some()
    }
}
