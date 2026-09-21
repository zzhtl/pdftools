//! Phase 3/4 验收：Word 转 PDF。
//!
//! 测试用的 docx 在这里现造，好让 CI 上没有任何外部素材也能跑。
//! 真实文书的验证靠 `examples/convert` 手工过。

use std::io::Write;
use std::path::PathBuf;

use pdfcore::ops::docx_to_pdf;
use pdfcore::{NoProgress, WarningKind};

fn tmp() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("docx");
    std::fs::create_dir_all(&d).unwrap();
    d
}

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="xml" ContentType="application/xml"/>
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

const RELS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

/// 把一段 `<w:body>` 的内容包成一个可用的 .docx。
fn make_docx(name: &str, body: &str) -> PathBuf {
    let path = tmp().join(name);
    let file = std::fs::File::create(&path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let opts: zip::write::FileOptions<'_, ()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    let doc = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:body>{body}
<w:sectPr><w:pgSz w:w="11906" w:h="16838"/>
<w:pgMar w:top="1440" w:right="1588" w:bottom="1440" w:left="1588"/></w:sectPr>
</w:body></w:document>"#
    );

    for (n, content) in [
        ("[Content_Types].xml", CONTENT_TYPES),
        ("_rels/.rels", RELS),
        ("word/document.xml", doc.as_str()),
    ] {
        zip.start_file(n, opts).unwrap();
        zip.write_all(content.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
    path
}

fn para(text: &str) -> String {
    format!(
        r#"<w:p><w:pPr><w:spacing w:line="240" w:lineRule="auto"/></w:pPr>
<w:r><w:rPr><w:rFonts w:ascii="Times New Roman" w:eastAsia="宋体"/><w:sz w:val="24"/></w:rPr>
<w:t xml:space="preserve">{text}</w:t></w:r></w:p>"#
    )
}

fn text_of(pdf: &[u8]) -> String {
    let doc = lopdf::Document::load_mem(pdf).unwrap();
    let pages: Vec<u32> = doc.get_pages().keys().copied().collect();
    let raw = doc.extract_text(&pages).unwrap_or_default();
    raw.chars().filter(|c| !c.is_whitespace()).collect()
}

fn convert(path: &std::path::Path) -> pdfcore::Report<docx_to_pdf::Outcome> {
    docx_to_pdf::run(path, &NoProgress).expect("转换失败")
}

#[test]
fn chinese_paragraphs_round_trip() {
    let body = para("示例标题") + &para("甲方某某某，合同编号 123456789012345678。");
    let path = make_docx("simple.docx", &body);
    let report = convert(&path);

    let got = text_of(&report.value.pdf);
    assert!(
        got.contains("示例标题"),
        "PDF 里抽不到标题，实际内容：{got}"
    );
    assert!(
        got.contains("123456789012345678"),
        "中西文混排的数字丢失了，实际内容：{got}"
    );
    assert!(got.contains("甲方某某某"), "正文缺失，实际内容：{got}");
}

/// `lineRule="auto"` 时 `w:line` 的单位是 **240 分之一行**，不是 twips。
/// 这条搞错的话每份文档的页数都会错，所以单独立一个测试钉住它。
#[test]
fn line_rule_auto_is_240ths_not_twips() {
    let line = |v: &str| {
        let body: String = (0..60)
            .map(|i| {
                format!(
                    r#"<w:p><w:pPr><w:spacing w:line="{v}" w:lineRule="auto"/></w:pPr>
<w:r><w:rPr><w:sz w:val="24"/></w:rPr><w:t>第{i}行文字内容</w:t></w:r></w:p>"#
                )
            })
            .collect();
        let path = make_docx(&format!("line{v}.docx"), &body);
        convert(&path).value.pages
    };

    let single = line("240"); // 1.0 倍
    let double = line("480"); // 2.0 倍

    assert!(
        double >= single * 2 - 1 && double <= single * 2 + 1,
        "240（单倍）排出 {single} 页，480（双倍）排出 {double} 页 —— \
         双倍行距应当约等于单倍的两倍。若把 480 当成 twips（24 磅固定行高），这个比例就不对了。"
    );
}

/// `w:ind w:firstLineChars` 的单位是 1/100 个字符，且**优先于** `w:firstLine`。
/// 中文公文里「首行缩进 2 字符」无处不在。
#[test]
fn first_line_chars_takes_precedence() {
    // firstLineChars=200（2 字符 = 24pt @12pt 字号），同时给一个矛盾的 firstLine=100（5pt）。
    let body = r#"<w:p><w:pPr><w:ind w:firstLineChars="200" w:firstLine="100"/></w:pPr>
<w:r><w:rPr><w:sz w:val="24"/></w:rPr><w:t>缩进测试文字</w:t></w:r></w:p>"#;
    let indented = make_docx("indent.docx", body);

    let body0 = r#"<w:p><w:r><w:rPr><w:sz w:val="24"/></w:rPr><w:t>缩进测试文字</w:t></w:r></w:p>"#;
    let plain = make_docx("noindent.docx", body0);

    let x_indented = first_text_x(&convert(&indented).value.pdf);
    let x_plain = first_text_x(&convert(&plain).value.pdf);
    let delta = x_indented - x_plain;

    // 12 磅字号下 2 个字符 = 24 磅。若错误地采用了 firstLine=100 twips，差值会是 5 磅。
    assert!(
        (delta - 24.0).abs() < 1.0,
        "首行缩进实际为 {delta:.1} 磅，应当是 24 磅（2 字符 × 12 磅）。\
         若得到约 5 磅，说明错误地优先采用了 w:firstLine 而不是 w:firstLineChars。"
    );
}

/// 从内容流里取第一个文本定位的 x 坐标。
fn first_text_x(pdf: &[u8]) -> f32 {
    let doc = lopdf::Document::load_mem(pdf).unwrap();
    let page_id = *doc.get_pages().values().next().unwrap();
    let content = doc.get_and_decode_page_content(page_id).unwrap();
    for op in &content.operations {
        if op.operator == "Tm" && op.operands.len() == 6 {
            return match &op.operands[4] {
                lopdf::Object::Real(v) => *v,
                lopdf::Object::Integer(v) => *v as f32,
                _ => continue,
            };
        }
    }
    panic!("内容流里找不到文本定位操作符");
}

/// 表格不渲染，但**必须留痕且保住文字** —— 对法律文书来说，
/// 悄悄丢一张表格是危险的。
#[test]
fn tables_are_reported_and_their_text_preserved() {
    let body = r#"<w:tbl>
<w:tr><w:tc><w:p><w:r><w:t>条目一</w:t></w:r></w:p></w:tc>
<w:tc><w:p><w:r><w:t>说明书</w:t></w:r></w:p></w:tc></w:tr>
<w:tr><w:tc><w:p><w:r><w:t>条目二</w:t></w:r></w:p></w:tc>
<w:tc><w:p><w:r><w:t>装箱单</w:t></w:r></w:p></w:tc></w:tr>
</w:tbl>"#;
    let path = make_docx("table.docx", body);
    let report = convert(&path);

    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.kind == WarningKind::UnsupportedElement && w.detail.contains("表格")),
        "表格没有被报告出来，警告列表：{:?}",
        report.warnings
    );

    let got = text_of(&report.value.pdf);
    for cell in ["条目一", "说明书", "条目二", "装箱单"] {
        assert!(got.contains(cell), "表格单元格「{cell}」的文字丢了：{got}");
    }
}

/// 老式 .doc 是 CFB 复合文档而不是 zip。要给一句有用的话，
/// 而不是「不是有效的 zip」。
#[test]
fn legacy_doc_format_is_refused_clearly() {
    let path = tmp().join("legacy.doc");
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1])
        .unwrap();
    f.write_all(&[0u8; 512]).unwrap();
    drop(f);

    let err = docx_to_pdf::run(&path, &NoProgress).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains(".doc") && msg.contains("另存为"),
        "对 .doc 的提示不够有用：{msg}"
    );
}

/// 回归测试：全角标点紧跟在西文之后时，不能被判给西文字体。
///
/// `；`（U+FF1B）的 Unicode script 属性是 Common，若按「继承前一个强字符」
/// 的规则归给拉丁字体，而该字体没有全角标点字形，就会落到 `.notdef`。
/// 更糟的是多个缺字会塌缩到同一个 GID 0，把 ToUnicode 也串掉 ——
/// 现象是导出的 PDF 里 `；` 被抽成了 `，`。
#[test]
fn fullwidth_punctuation_after_latin_uses_cjk_font() {
    let body = para("请下载PDF；文件较大（约3MB）：请耐心等待。");
    let path = make_docx("punct.docx", &body);
    let report = convert(&path);

    let got = text_of(&report.value.pdf);
    for ch in ['；', '（', '）', '：', '。'] {
        assert!(
            got.contains(ch),
            "全角标点「{ch}」在输出里丢失或被替换了，实际抽回：{got}"
        );
    }
    assert!(
        !report
            .warnings
            .iter()
            .any(|w| w.detail.contains("没有字形")),
        "出现了缺字警告，说明标点被判给了没有该字形的字体：{:?}",
        report.warnings
    );
}
