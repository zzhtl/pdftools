//! 把字体子集嵌进 PDF。
//!
//! 这是整个项目最容易写错、且错了最不容易发现的地方：写错的 PDF 可能在
//! Chrome / pdf.js 里显示正常，在 Acrobat 里却是空白页。两个分叉必须分别处理：
//!
//! | 轮廓 | /Subtype        | 字体程序键 | /CIDToGIDMap |
//! |------|-----------------|-----------|--------------|
//! | glyf | CIDFontType2    | /FontFile2 | /Identity    |
//! | CFF  | CIDFontType0    | /FontFile3 (/Subtype /OpenType) | **不能写** |
//!
//! `/CIDToGIDMap` 是 CIDFontType2 专有的键，写到 CIDFontType0 上属于结构错误。

use std::collections::BTreeMap;

use pdf_writer::types::{CidFontType, FontFlags, SystemInfo, UnicodeCmap};
use pdf_writer::{Filter, Finish, Name, Pdf, Rect, Ref, Str};

use super::{deflate, RefAlloc};
use crate::error::Result;
use crate::fonts::{subset_font, Flavor, FontFace, GidMap};

pub struct EmbeddedFont {
    /// 内容流的 /Font 资源里要引用的对象号（Type0 字体）。
    pub font_ref: Ref,
    /// 旧 GID → 新 GID。内容流里写的两字节码必须是新 GID。
    pub map: GidMap,
}

/// 嵌入一个字体面。
///
/// `used` 的键是整形得到的旧 GID，值是该字形对应的**原文**。
/// 原文来自 HarfBuzz 的 cluster 回查，而不是反查 cmap —— 后者在连字和
/// 多对一的 CJK 映射上是错的。
pub fn embed_font(
    pdf: &mut Pdf,
    alloc: &mut RefAlloc,
    face: &FontFace,
    used: &BTreeMap<u16, String>,
) -> Result<EmbeddedFont> {
    let gids: Vec<u16> = used.keys().copied().collect();
    let (subset, map) = subset_font(face, &gids)?;

    let type0_ref = alloc.next_ref();
    let cid_ref = alloc.next_ref();
    let descriptor_ref = alloc.next_ref();
    let font_file_ref = alloc.next_ref();
    let to_unicode_ref = alloc.next_ref();

    let metrics = face.metrics();
    let base_name = format!("{}+{}", subset_tag(&gids), sanitize_name(&face.name));

    // ---- Type0 ----
    {
        let mut f = pdf.type0_font(type0_ref);
        f.base_font(Name(base_name.as_bytes()));
        // Identity-H：两字节码直接就是 CID。subsetter 保证 CID == 新 GID。
        f.encoding_predefined(Name(b"Identity-H"));
        f.descendant_font(cid_ref);
        f.to_unicode(to_unicode_ref);
        f.finish();
    }

    // ---- CIDFont ----
    {
        let mut f = pdf.cid_font(cid_ref);
        f.subtype(match subset.flavor {
            Flavor::TrueType => CidFontType::Type2,
            Flavor::Cff => CidFontType::Type0,
        });
        f.base_font(Name(base_name.as_bytes()));
        f.system_info(SystemInfo {
            registry: Str(b"Adobe"),
            ordering: Str(b"Identity"),
            supplement: 0,
        });
        f.font_descriptor(descriptor_ref);
        f.default_width(DEFAULT_WIDTH);

        write_widths(&mut f, face, &subset);

        if subset.flavor == Flavor::TrueType {
            // 子集后 GID 连续且 CID == GID，所以是恒等映射。
            f.cid_to_gid_map_predefined(Name(b"Identity"));
        }
        f.finish();
    }

    // ---- FontDescriptor ----
    {
        let scale = |v: f32| metrics.to_pdf_units(v);
        let mut d = pdf.font_descriptor(descriptor_ref);
        d.name(Name(base_name.as_bytes()));
        // CID 字体用 Identity 编码，不对应任何标准 Latin 编码，按惯例标 SYMBOLIC。
        d.flags(FontFlags::SYMBOLIC);
        d.bbox(Rect::new(
            scale(metrics.bbox[0] as f32),
            scale(metrics.bbox[1] as f32),
            scale(metrics.bbox[2] as f32),
            scale(metrics.bbox[3] as f32),
        ));
        d.italic_angle(metrics.italic_angle);
        d.ascent(scale(metrics.ascender as f32));
        d.descent(scale(metrics.descender as f32));
        d.cap_height(scale(metrics.cap_height as f32));
        // StemV 没有任何可靠来源，所有实现都是估的。这个值不影响渲染，
        // 只在阅读器需要合成替代字体时才用得上。
        d.stem_v(10.0 + (metrics.weight.saturating_sub(400) as f32) / 10.0);
        match subset.flavor {
            Flavor::TrueType => d.font_file2(font_file_ref),
            Flavor::Cff => d.font_file3(font_file_ref),
        };
        d.finish();
    }

    // ---- 字体程序 ----
    {
        let raw_len = subset.data.len() as i32;
        let compressed = deflate(&subset.data);
        let mut s = pdf.stream(font_file_ref, &compressed);
        s.filter(Filter::FlateDecode);
        match subset.flavor {
            Flavor::TrueType => {
                // /Length1 是解压后的长度，TrueType 字体程序必须写。
                s.pair(Name(b"Length1"), raw_len);
            }
            Flavor::Cff => {
                // subsetter 输出的是完整的 OTTO 容器而不是裸 CFF，
                // 所以这里是 /OpenType 而不是 /CIDFontType0C。
                s.pair(Name(b"Subtype"), Name(b"OpenType"));
            }
        }
        s.finish();
    }

    // ---- ToUnicode ----
    // subsetter 会删掉 cmap 表，没有这个 CMap，PDF 里的中文就只是一张图：
    // 选不中、搜不到、复制出来是乱码。
    {
        let mut cmap = UnicodeCmap::<u16>::new(
            Name(b"Custom"),
            SystemInfo {
                registry: Str(b"Adobe"),
                ordering: Str(b"UCS"),
                supplement: 0,
            },
        );
        for (old_gid, text) in used {
            let new_gid = map.new_gid(*old_gid);
            let chars: Vec<char> = text.chars().collect();
            if chars.is_empty() {
                continue;
            }
            cmap.pair_with_multiple(new_gid, chars);
        }
        let data = cmap.finish();
        let compressed = deflate(data.as_ref());
        pdf.stream(to_unicode_ref, &compressed)
            .filter(Filter::FlateDecode)
            .finish();
    }

    Ok(EmbeddedFont {
        font_ref: type0_ref,
        map,
    })
}

/// PDF 里所有字形宽度都以 1/1000 em 为单位，与字体的 upem 无关。
/// CJK 字体绝大多数字形正好是一个全角宽，所以把 DW 定在 1000 能让 /W 数组只剩零头。
const DEFAULT_WIDTH: f32 = 1000.0;

fn write_widths(
    f: &mut pdf_writer::writers::CidFont,
    face: &FontFace,
    subset: &crate::fonts::SubsetFont,
) {
    let metrics = face.metrics();

    // 收集所有与 DW 不同的宽度。等于 DW 的直接省略 —— 对 2000 字的中文子集，
    // 这是 20KB 和 2KB 的差别。
    let entries: Vec<(u16, f32)> = subset
        .old_gids
        .iter()
        .enumerate()
        .filter_map(|(new_gid, old_gid)| {
            let w = metrics.to_pdf_units(face.advance(*old_gid) as f32).round();
            (w != DEFAULT_WIDTH).then_some((new_gid as u16, w))
        })
        .collect();

    if entries.is_empty() {
        return;
    }

    let mut widths = f.widths();
    let mut i = 0;
    while i < entries.len() {
        // 先看能否用 `c_first c_last w` 形式：连续 CID 且宽度相同。
        let mut run = i + 1;
        while run < entries.len()
            && entries[run].0 == entries[run - 1].0 + 1
            && entries[run].1 == entries[i].1
        {
            run += 1;
        }
        // 三个以下用区间形式反而更长，那就走数组形式。
        if run - i >= 4 {
            widths.same(entries[i].0, entries[run - 1].0, entries[i].1);
            i = run;
            continue;
        }

        // `c [w1 w2 ...]` 形式：吃掉一段连续 CID，但遇到值得用区间形式的地方就停。
        let start = i;
        let mut end = i + 1;
        while end < entries.len() && entries[end].0 == entries[end - 1].0 + 1 {
            let mut r = end + 1;
            while r < entries.len()
                && entries[r].0 == entries[r - 1].0 + 1
                && entries[r].1 == entries[end].1
            {
                r += 1;
            }
            if r - end >= 4 {
                break;
            }
            end += 1;
        }
        widths.consecutive(entries[start].0, entries[start..end].iter().map(|e| e.1));
        i = end;
    }
    widths.finish();
}

/// PDF 规范要求子集字体的 BaseFont 带一个 6 个大写字母的前缀。
/// 由字形集合派生，保证同样的输入得到同样的输出（构建可复现）。
fn subset_tag(gids: &[u16]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for g in gids {
        hash ^= *g as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (0..6)
        .map(|i| (b'A' + ((hash >> (i * 5)) & 0x19) as u8) as char)
        .collect()
}

/// PDF 的 Name 对象不能含空格等分隔符。
fn sanitize_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect()
}
