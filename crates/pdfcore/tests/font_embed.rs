//! Phase 1 闸口：中文字体子集能不能正确嵌进 PDF 并且**文字可回抽**。
//!
//! 这个测试挡住的是整个项目最阴险的一类 bug：PDF 看上去正常，
//! 但 ToUnicode 写错，于是文字选不中、搜不到、复制出来是乱码。
//! 肉眼检查发现不了，只能靠把文字抽回来比对。

use std::collections::BTreeMap;

use pdfcore::fonts::{cluster_texts, shape_run, split_by_script, system::SystemFonts, FontFace};
use pdfcore::pdf::writer::{
    font::embed_font, text::show_text, text::TextItem, DocBuilder, PageSpec,
};

const SAMPLE: &str = "示例文字 Times 12345";

fn find_cjk_face() -> Option<FontFace> {
    let fonts = SystemFonts::load();
    fonts
        .find(pdfcore::fonts::system::PDF_SANS_PREFERENCE, false, false)
        .or_else(|| fonts.find(pdfcore::fonts::system::PDF_SERIF_PREFERENCE, false, false))
        .map(|f| f.face)
}

/// 整形整段文本，按 script 分段用对应的 script 标签，返回所有字形与它们的原文。
fn shape_all(
    face: &FontFace,
    text: &str,
) -> (Vec<pdfcore::fonts::ShapedGlyph>, BTreeMap<u16, String>) {
    let mut glyphs = Vec::new();
    let mut used: BTreeMap<u16, String> = BTreeMap::new();

    for (range, class) in split_by_script(text) {
        let segment = &text[range.clone()];
        let run = shape_run(face, segment, class.to_rustybuzz());
        for (gid, s) in cluster_texts(segment, &run.glyphs) {
            if !s.is_empty() {
                used.entry(gid).or_insert(s);
            } else {
                used.entry(gid).or_default();
            }
        }
        glyphs.extend(run.glyphs);
    }
    (glyphs, used)
}

fn build_pdf(face: &FontFace) -> Vec<u8> {
    let (glyphs, used) = shape_all(face, SAMPLE);
    assert!(!glyphs.is_empty(), "整形没有产出任何字形");
    assert!(
        glyphs.iter().all(|g| g.gid != 0),
        "有字符落到了 .notdef，说明所选字体缺少这些字形"
    );

    let mut doc = DocBuilder::new();
    // 必须用 DocBuilder 自己的分配器：另起一个分配器会发出重复的对象号。
    let embedded = {
        let (pdf, alloc) = doc.parts();
        embed_font(pdf, alloc, face, &used).expect("嵌入字体失败")
    };

    let mut page = PageSpec::new(595.0, 842.0);
    page.fonts.push(("F0".into(), embedded.font_ref));
    show_text(
        &mut page.content,
        &TextItem {
            glyphs: &glyphs,
            font_res: "F0",
            size_pt: 24.0,
            x_pt: 72.0,
            y_pt: 700.0,
            color: [0, 0, 0],
            char_spacing: 0.0,
            word_spacing: 0.0,
        },
        face,
        &embedded.map,
    );
    doc.add_page(page);
    doc.finish().expect("生成 PDF 失败")
}

#[test]
fn cjk_subset_roundtrips_through_tounicode() {
    let Some(face) = find_cjk_face() else {
        eprintln!("跳过：本机没有可用的中文字体");
        return;
    };
    eprintln!("使用字体：{}（{:?}）", face.name, face.metrics().flavor);

    let bytes = build_pdf(&face);

    let out_dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"));
    let path = out_dir.join("font_gate.pdf");
    std::fs::write(&path, &bytes).unwrap();
    eprintln!("已写出 {}", path.display());

    // 用 lopdf 作为独立于我们写入器的第三方实现来回抽文字。
    let doc = lopdf::Document::load_mem(&bytes).expect("生成的 PDF 无法被 lopdf 解析");
    assert_eq!(doc.get_pages().len(), 1);

    let extracted = doc.extract_text(&[1]).expect("抽取文字失败");
    let normalized: String = extracted.chars().filter(|c| !c.is_whitespace()).collect();
    let expected: String = SAMPLE.chars().filter(|c| !c.is_whitespace()).collect();
    assert_eq!(
        normalized, expected,
        "回抽的文字与输入不一致，说明 ToUnicode CMap 写错了"
    );
}
