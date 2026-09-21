//! 字体子集化。
//!
//! `subsetter` 的契约（见其 crate 文档）对我们有两个关键保证：
//!   1. 它会把 SID-keyed 的 CFF 转成 CID-keyed，并对**所有**字体建立 GID→CID 的恒等映射。
//!      因此重映射后的 GID 可以直接当 CID 用，不必关心原字体是哪种。
//!   2. 它会**删掉 cmap 表**。所以 PDF 里必须自带 ToUnicode，否则文字选不中、搜不到。
//!
//! 输出仍是完整的 sfnt 容器：TrueType 输入出 `0x00010000`，CFF 输入出 `OTTO`。
//! 这决定了 PDF 里写 /FontFile2 还是 /FontFile3 + /Subtype /OpenType。

use super::{Flavor, FontFace};
use crate::error::{CoreError, Result};

pub struct SubsetFont {
    /// 子集后的完整 sfnt 字节。
    pub data: Vec<u8>,
    pub flavor: Flavor,
    /// 下标是新 GID，值是旧 GID。用来回查 hmtx 宽度。
    /// 新 GID 从 0 连续递增，且 0 恒为 .notdef。
    pub old_gids: Vec<u16>,
}

impl SubsetFont {
    pub fn num_glyphs(&self) -> u16 {
        self.old_gids.len() as u16
    }
}

/// 对字体做子集化。`used` 里不必包含 0，`.notdef` 会自动加入。
pub fn subset_font(face: &FontFace, used: &[u16]) -> Result<(SubsetFont, GidMap)> {
    if face.metrics().embedding == super::Embedding::Restricted {
        return Err(CoreError::Font(format!(
            "字体「{}」的 fsType 标志禁止内嵌，请换一个字体",
            face.name
        )));
    }

    // 用 sorted 版本：新旧 GID 同序递增，这样 /W 数组里连续 CID 的合并率最高。
    let remapper = subsetter::GlyphRemapper::new_from_glyphs_sorted(used);
    let data = subsetter::subset(face.data(), face.index(), &remapper)
        .map_err(|e| CoreError::Font(format!("字体子集化失败：{e:?}")))?;

    let old_gids: Vec<u16> = remapper.remapped_gids().collect();
    let map = GidMap { remapper };

    Ok((
        SubsetFont {
            data,
            flavor: face.metrics().flavor,
            old_gids,
        },
        map,
    ))
}

/// 旧 GID → 新 GID 的查询句柄。内容流里写的必须是新 GID。
pub struct GidMap {
    remapper: subsetter::GlyphRemapper,
}

impl GidMap {
    /// 未出现在子集里的字形回落到 0（.notdef）。
    /// 这种情况说明调用方漏收集了字形，宁可显示成方框也不要写出越界的 CID。
    pub fn new_gid(&self, old: u16) -> u16 {
        self.remapper.get(old).unwrap_or(0)
    }
}
