//! Phase 3/4 验收：Word 转 PDF。
//!
//! 测试用的 docx 在这里现造，好让 CI 上没有任何外部素材也能跑。
//! 与 LibreOffice 的逐坐标比对、真实文书的回归闸门见 `tests/oracle.rs`。

mod common;

use std::io::Write;
use std::path::PathBuf;

use common::docx::{para, DocxBuilder};
use common::{require_cjk_font, tmp};
use pdfcore::ops::docx_to_pdf;
use pdfcore::{NoProgress, WarningKind};

/// 把一段 `<w:body>` 的内容包成一个可用的 .docx。
fn make_docx(name: &str, body: &str) -> PathBuf {
    DocxBuilder::new().body(body).build(name)
}

/// 造一个可以指定 sectPr 额外内容的 docx。
fn make_docx_with_sect(name: &str, body: &str, sect_extra: &str) -> PathBuf {
    DocxBuilder::new()
        .body(body)
        .sect_extra(sect_extra)
        .build(name)
}

fn text_of(pdf: &[u8]) -> String {
    let doc = lopdf::Document::load_mem(pdf).unwrap();
    let pages: Vec<u32> = doc.get_pages().keys().copied().collect();
    let raw = doc.extract_text(&pages).unwrap_or_default();
    raw.chars().filter(|c| !c.is_whitespace()).collect()
}

/// 转换，并顺带确认新引擎按旧规则排出来与重写前的引擎一模一样。
fn convert(path: &std::path::Path) -> pdfcore::Report<docx_to_pdf::Outcome> {
    common::assert_same_as_legacy(path);
    docx_to_pdf::run(path, &NoProgress).expect("转换失败")
}

#[test]
fn chinese_paragraphs_round_trip() {
    if !require_cjk_font() {
        return;
    }
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
    if !require_cjk_font() {
        return;
    }
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
    let path = tmp("docx").join("legacy.doc");
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
    if !require_cjk_font() {
        return;
    }
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

/// 取第一页内容流里所有文本定位（Tm）的 (x, y)。
fn text_origins(pdf: &[u8]) -> Vec<(f32, f32)> {
    let doc = lopdf::Document::load_mem(pdf).unwrap();
    let page_id = *doc.get_pages().values().next().unwrap();
    let content = doc.get_and_decode_page_content(page_id).unwrap();
    let num = |o: &lopdf::Object| match o {
        lopdf::Object::Real(v) => *v,
        lopdf::Object::Integer(v) => *v as f32,
        _ => f32::NAN,
    };
    content
        .operations
        .iter()
        .filter(|op| op.operator == "Tm" && op.operands.len() == 6)
        .map(|op| (num(&op.operands[4]), num(&op.operands[5])))
        .collect()
}

/// 行网格（`w:docGrid`）必须生效。
///
/// 这是中文文档排版最要命的一处：网格生效时，单倍行高要**向上吸附到 linePitch
/// 的整数倍**，倍数再乘在这之上。忽略它，整篇的行密度会高出近一倍 ——
/// 实测曾经导致 8 份真实文书的页数只有 LibreOffice 参照的 60%。
///
/// 12pt 正文用 Noto Serif CJK 时自然行高约 17.2pt，在 15.6pt（312 twips）的网格上
/// 占满 2 格 = 31.2pt；用 Windows 上真正的宋体时自然行高只有约 12pt，只占 1 格。
/// 所以期望值按本机实际字体的自然行高推出来，检验的是规则而不是某个字体的度量。
#[test]
fn doc_grid_snaps_line_height_up_to_the_grid() {
    if !require_cjk_font() {
        return;
    }
    let body = (0..12)
        .map(|_| {
            r#"<w:p><w:pPr><w:spacing w:line="240" w:lineRule="auto"/></w:pPr>
<w:r><w:rPr><w:rFonts w:eastAsia="宋体"/><w:sz w:val="24"/></w:rPr>
<w:t>测试文字测试文字测试文字测试文字测试文字测试文字测试文字测试文字测试文字</w:t></w:r></w:p>"#
                .to_string()
        })
        .collect::<String>();

    let spacing_of = |name: &str, sect: &str| {
        let p = make_docx_with_sect(name, &body, sect);
        let pdf = convert(&p).value.pdf;
        let mut ys: Vec<f32> = text_origins(&pdf).iter().map(|(_, y)| *y).collect();
        ys.sort_by(|a, b| b.partial_cmp(a).unwrap());
        ys.dedup();
        (ys[0] - ys[1]).abs()
    };

    let plain = spacing_of("grid_off.docx", "");
    let grid = spacing_of(
        "grid_on.docx",
        r#"<w:docGrid w:type="lines" w:linePitch="312"/>"#,
    );

    // 无网格时就是字体的自然行高。中文字体的行高落在 1.0～1.6 em 之间。
    assert!(
        (12.0..=19.5).contains(&plain),
        "无网格时应当是字体自然行高（12pt 字号约 12～19pt），实际 {plain:.2}"
    );
    let expected = (plain / 15.6).ceil().max(1.0) * 15.6;
    assert!(
        (grid - expected).abs() < 0.6,
        "有网格时应当把自然行高 {plain:.2} 向上吸附到 15.6 的整数倍 {expected:.2}，实际 {grid:.2}"
    );
}

/// `w:docGrid w:type="default"` 不吸附 —— Word 常写这种形式，一律吸附会把行距撑大一倍。
#[test]
fn doc_grid_type_default_does_not_snap() {
    if !require_cjk_font() {
        return;
    }
    let body = (0..6)
        .map(|_| {
            r#"<w:p><w:r><w:rPr><w:rFonts w:eastAsia="宋体"/><w:sz w:val="24"/></w:rPr>
<w:t>测试文字测试文字</w:t></w:r></w:p>"#
                .to_string()
        })
        .collect::<String>();
    let p = make_docx_with_sect(
        "grid_default.docx",
        &body,
        r#"<w:docGrid w:type="default" w:linePitch="312"/>"#,
    );
    let pdf = convert(&p).value.pdf;
    let mut ys: Vec<f32> = text_origins(&pdf).iter().map(|(_, y)| *y).collect();
    ys.sort_by(|a, b| b.partial_cmp(a).unwrap());
    ys.dedup();
    let gap = (ys[0] - ys[1]).abs();
    assert!(
        gap < 20.0,
        "type=\"default\" 不该吸附到网格，行距应当约 17.2pt，实际 {gap:.2}"
    );
}

/// 中日韩文字与西文/数字之间要自动插入间距。
///
/// 没有它，「第9条」会挤成一团。Word 与 LibreOffice 默认都会加
/// （Word 的开关是 `w:autoSpaceDE`/`w:autoSpaceDN`）。
#[test]
fn cjk_and_latin_get_automatic_spacing() {
    if !require_cjk_font() {
        return;
    }
    // 「中文」+「9」+「中文」：三段各自定位，从 Tm 的 x 差值能直接量出间距。
    let body = r#"<w:p><w:r><w:rPr><w:rFonts w:ascii="Times New Roman" w:eastAsia="宋体"/>
<w:sz w:val="24"/></w:rPr><w:t>中文9中文</w:t></w:r></w:p>"#;
    let with = convert(&make_docx_with_sect("space_on.docx", body, ""))
        .value
        .pdf;

    let xs: Vec<f32> = {
        let mut v: Vec<f32> = text_origins(&with).iter().map(|(x, _)| *x).collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v
    };
    assert!(xs.len() >= 3, "应当切成中文/数字/中文三段，实际 {xs:?}");

    // 第一段「中文」= 2 × 12pt = 24pt。数字段的起点减去它，剩下的就是间距。
    let gap = xs[1] - xs[0] - 24.0;
    assert!(
        (gap - 0.2 * 12.0).abs() < 0.5,
        "中西文间距应当约 2.4pt（0.2em @12pt），实际 {gap:.2}pt"
    );

    // 关掉开关就不该有间距。
    let body_off = r#"<w:p><w:pPr><w:autoSpaceDE w:val="0"/><w:autoSpaceDN w:val="0"/></w:pPr>
<w:r><w:rPr><w:rFonts w:ascii="Times New Roman" w:eastAsia="宋体"/>
<w:sz w:val="24"/></w:rPr><w:t>中文9中文</w:t></w:r></w:p>"#;
    let without = convert(&make_docx_with_sect("space_off.docx", body_off, ""))
        .value
        .pdf;
    let mut xs2: Vec<f32> = text_origins(&without).iter().map(|(x, _)| *x).collect();
    xs2.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let gap2 = xs2[1] - xs2[0] - 24.0;
    assert!(
        gap2.abs() < 0.5,
        "autoSpaceDE/DN 关闭时不该有间距，实际 {gap2:.2}pt"
    );
}

/// ASCII 字符（含数字）必须用西文字体，不能跟着相邻汉字走。
///
/// Word 的 `w:rFonts w:ascii` 管的就是 0x00-0x7F 这一段。归错了不仅字体不对，
/// 中西文之间的自动间距也会因为识别不到边界而完全失效。
#[test]
fn ascii_digits_use_the_latin_font() {
    if !require_cjk_font() {
        return;
    }
    let body = r#"<w:p><w:r><w:rPr><w:rFonts w:ascii="Times New Roman" w:eastAsia="宋体"/>
<w:sz w:val="24"/></w:rPr><w:t>编号123456789012345678完</w:t></w:r></w:p>"#;
    let pdf = convert(&make_docx_with_sect("ascii_font.docx", body, ""))
        .value
        .pdf;

    // 三段（中文 / 数字 / 中文）意味着数字被单独切出来交给了西文字体。
    let origins = text_origins(&pdf);
    assert!(
        origins.len() >= 3,
        "数字应当被切成独立的一段用西文字体渲染，实际只有 {} 段",
        origins.len()
    );

    let doc = lopdf::Document::load_mem(&pdf).unwrap();
    let pages: Vec<u32> = doc.get_pages().keys().copied().collect();
    let text: String = doc
        .extract_text(&pages)
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    assert!(
        text.contains("123456789012345678"),
        "长数字串应当完整保留，实际抽回 {text}"
    );
}

/// 段落**最后一行**的倍数行距，额外部分按**吸附前**的自然行高算。
///
/// 实测自 LibreOffice（1.0 / 1.3 / 1.5 / 2.0 四个倍数全部吻合）：
///   段内行距     = 吸附后行高 × 倍数
///   段落末行贡献 = 吸附后行高 + (倍数 − 1) × 吸附前自然行高
///
/// 不区分这一条，每个段落边界都会多出 (倍数−1) × 吸附增量。8 份真实文书上
/// 累积的结果是平白多出一整页；区分之后页数与参照 8/8 完全一致。
#[test]
fn last_line_of_paragraph_uses_unsnapped_extra_leading() {
    if !require_cjk_font() {
        return;
    }
    let grid = r#"<w:docGrid w:type="lines" w:linePitch="312"/>"#;
    let para = |text: &str| {
        format!(
            r#"<w:p><w:pPr><w:spacing w:after="0" w:line="312" w:lineRule="auto"/></w:pPr>
<w:r><w:rPr><w:rFonts w:eastAsia="宋体"/><w:sz w:val="24"/></w:rPr><w:t>{text}</w:t></w:r></w:p>"#
        )
    };

    let gap_of = |name: &str, body: String| {
        let p = make_docx_with_sect(name, &body, grid);
        let pdf = convert(&p).value.pdf;
        let mut ys: Vec<f32> = text_origins(&pdf).iter().map(|(_, y)| *y).collect();
        ys.sort_by(|a, b| b.partial_cmp(a).unwrap());
        ys.dedup();
        (ys[0] - ys[1]).abs()
    };

    // 自然行高：无网格、单倍行距下相邻基线的距离。
    let natural = {
        let body: String = (0..6)
            .map(|_| {
                r#"<w:p><w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/></w:pPr>
<w:r><w:rPr><w:rFonts w:eastAsia="宋体"/><w:sz w:val="24"/></w:rPr><w:t>测试文字</w:t></w:r></w:p>"#
            })
            .collect();
        let pdf = convert(&make_docx_with_sect("tail_natural.docx", &body, ""))
            .value
            .pdf;
        let mut ys: Vec<f32> = text_origins(&pdf).iter().map(|(_, y)| *y).collect();
        ys.sort_by(|a, b| b.partial_cmp(a).unwrap());
        ys.dedup();
        (ys[0] - ys[1]).abs()
    };
    let snapped = (natural / 15.6).ceil().max(1.0) * 15.6;

    // 多个单行段落：相邻基线之间跨的是「段落边界」。
    let between_paragraphs = gap_of(
        "tail_single.docx",
        (0..6).map(|_| para("甲方：某某某")).collect(),
    );
    // 一个长段落：相邻基线之间跨的是「段内换行」。
    let within_paragraph = gap_of("tail_multi.docx", para(&"测试文字".repeat(40)));

    // Noto Serif CJK 下即 31.2 × 1.3 = 40.56 与 31.2 + 0.3 × 17.2 ≈ 36.4。
    let within_expected = snapped * 1.3;
    let between_expected = snapped + 0.3 * natural;
    assert!(
        (within_paragraph - within_expected).abs() < 0.8,
        "段内行距应当是 吸附后行高 {snapped:.2} × 1.3 = {within_expected:.2}，实际 {within_paragraph:.2}"
    );
    assert!(
        (between_paragraphs - between_expected).abs() < 1.0,
        "段落边界应当是 {snapped:.2} + 0.3 × 自然行高 {natural:.2} = {between_expected:.2}，实际 {between_paragraphs:.2}"
    );
    // 两者之差就是 0.3 × 吸附增量。字体的自然行高恰好接近网格整数倍时差很小，
    // 这条就不构成检验，所以只在吸附增量明显时断言。
    if snapped - natural > 2.0 {
        assert!(
            within_paragraph - between_paragraphs > 0.5,
            "段内行距必须大于段落边界的推进量，否则说明没有区分末行"
        );
    }
}

/// 自动编号：编号文字按级别格式生成，编号后的制表符跳到悬挂缩进处；右对齐的编号
/// 末端贴着首行起点；空格后缀。指向不存在的编号定义的段落照常排正文、不带编号。
#[test]
fn list_numbers_are_drawn() {
    if !require_cjk_font() {
        return;
    }
    let lvl = |i: u8, fmt: &str, text: &str, left: u32, extra: &str| {
        format!(
            r#"<w:lvl w:ilvl="{i}"><w:start w:val="1"/><w:numFmt w:val="{fmt}"/>{extra}<w:lvlText w:val="{text}"/><w:pPr><w:ind w:left="{left}" w:hanging="420"/></w:pPr></w:lvl>"#
        )
    };
    let numbering = format!(
        r#"<w:abstractNum w:abstractNumId="0">{}{}</w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
<w:abstractNum w:abstractNumId="1">{}</w:abstractNum><w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num>
<w:abstractNum w:abstractNumId="2">{}</w:abstractNum><w:num w:numId="3"><w:abstractNumId w:val="2"/></w:num>"#,
        lvl(0, "chineseCounting", "%1、", 420, ""),
        lvl(1, "decimal", "%2.", 840, ""),
        lvl(0, "decimal", "%1.", 840, r#"<w:lvlJc w:val="right"/>"#),
        lvl(0, "upperRoman", "%1.", 0, r#"<w:suff w:val="space"/>"#),
    );
    let para = |num: u32, ilvl: u8, text: &str| {
        format!(
            r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="{ilvl}"/><w:numId w:val="{num}"/></w:numPr></w:pPr><w:r><w:rPr><w:rFonts w:ascii="Times New Roman" w:eastAsia="宋体"/><w:sz w:val="24"/></w:rPr><w:t>{text}</w:t></w:r></w:p>"#
        )
    };
    let body = [
        para(1, 0, "甲项"),
        para(1, 1, "乙项"),
        para(1, 1, "丙项"),
        para(1, 0, "丁项"),
        para(2, 0, "戊项"),
        para(3, 0, "己项"),
        para(9, 0, "庚项"),
    ]
    .concat();
    let path = DocxBuilder::new()
        .numbering(&numbering)
        .body(&body)
        .build("list_numbers.docx");
    let report = convert(&path);
    assert!(
        !report.warnings.iter().any(|w| w.detail.contains("编号")),
        "{:?}",
        report.warnings
    );
    let pages = common::pdftext::extract(&report.value.pdf);
    let lines = &pages[0].lines;
    let texts: Vec<String> = lines
        .iter()
        .map(|l| common::pdftext::norm(&l.text))
        .collect();
    assert_eq!(
        texts,
        [
            "一、甲项",
            "1.乙项",
            "2.丙项",
            "二、丁项",
            "1.戊项",
            "I.己项",
            "庚项"
        ]
    );
    // 左边距 79.4pt。编号后的制表符跳到左缩进（悬挂缩进的位置）。
    let x_of = |line: &common::pdftext::Line, needle: &str| {
        line.frags
            .iter()
            .flat_map(|f| &f.glyphs)
            .find(|(t, _)| t == needle)
            .map(|(_, x)| *x)
            .unwrap_or_else(|| panic!("找不到「{needle}」：{line:?}"))
    };
    assert!((x_of(&lines[0], "一") - 79.4).abs() < 0.01);
    assert!((x_of(&lines[0], "甲") - (79.4 + 21.0)).abs() < 0.01);
    assert!((x_of(&lines[1], "乙") - (79.4 + 42.0)).abs() < 0.01);
    // 右对齐：编号末端（「.」之后）正好在首行起点 79.4 + 42 - 21 处。
    let dot_end = lines[4].frags[0].x + lines[4].frags[0].width;
    assert!((dot_end - (79.4 + 21.0)).abs() < 0.01, "{:?}", lines[4]);
    // 空格后缀：正文紧跟在「I.」与一个空格之后，不跳制表位。
    assert!(x_of(&lines[5], "己") < 79.4 + 36.0);
    assert!((x_of(&lines[6], "庚") - 79.4).abs() < 0.01);
}

/// 页眉页脚不渲染，但要报出来。
#[test]
fn header_and_footer_are_reported() {
    if !require_cjk_font() {
        return;
    }
    let path = DocxBuilder::new()
        .body(r#"<w:p><w:r><w:rPr><w:rFonts w:eastAsia="宋体"/><w:sz w:val="24"/></w:rPr><w:t>正文</w:t></w:r></w:p>"#)
        .header("default", "<w:p><w:r><w:t>页眉</w:t></w:r></w:p>")
        .build("headref.docx");

    let report = convert(&path);
    assert!(
        report.warnings.iter().any(|w| w.detail.contains("页眉")),
        "引用了页眉却没有报出来：{:?}",
        report.warnings
    );
}

/// 中西文边界上已经有空格时，不再叠加自动间距。
///
/// 叠加会让行变宽并提前折行 —— 实测曾导致「正文第 1 段。」重复 6 次的段落
/// 从 1 行变成 2 行，整篇页数多出 50%。
///
/// 判据与字体无关：边界上已有空格时，开着自动间距排出来的每个片段的位置，
/// 必须和关掉自动间距时完全一样。
#[test]
fn no_extra_gap_when_a_real_space_already_separates() {
    if !require_cjk_font() {
        return;
    }
    let origins = |name: &str, ppr: &str| {
        let body = format!(
            r#"<w:p><w:pPr>{ppr}</w:pPr><w:r><w:rPr><w:rFonts w:ascii="Times New Roman" w:eastAsia="宋体"/>
<w:sz w:val="24"/></w:rPr><w:t xml:space="preserve">正文第 1 段。</w:t></w:r></w:p>"#
        );
        let pdf = convert(&make_docx_with_sect(name, &body, "")).value.pdf;
        let mut xs: Vec<f32> = text_origins(&pdf).iter().map(|(x, _)| *x).collect();
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        xs
    };

    let on = origins("spaced_on.docx", "");
    let off = origins(
        "spaced_off.docx",
        r#"<w:autoSpaceDE w:val="0"/><w:autoSpaceDN w:val="0"/>"#,
    );
    assert!(on.len() >= 3, "应当切成中文/数字/中文三段，实际 {on:?}");
    assert_eq!(on.len(), off.len());
    for (a, b) in on.iter().zip(&off) {
        assert!(
            (a - b).abs() < 0.01,
            "边界上已有空格，却又加了自动间距：开 {on:?} / 关 {off:?}"
        );
    }
}

fn base_fonts(pdf: &[u8]) -> Vec<String> {
    let doc = lopdf::Document::load_mem(pdf).unwrap();
    doc.objects
        .values()
        .filter_map(|o| o.as_dict().ok())
        .filter(|d| d.get(b"Type").and_then(lopdf::Object::as_name).ok() == Some(b"Font".as_ref()))
        .filter(|d| {
            d.get(b"Subtype").and_then(lopdf::Object::as_name).ok() == Some(b"Type0".as_ref())
        })
        .filter_map(|d| d.get(b"BaseFont").and_then(lopdf::Object::as_name).ok())
        .map(|n| String::from_utf8_lossy(n).into_owned())
        .collect()
}

/// 嵌入字体的 BaseFont 要用字体自己的 PostScript 名。
///
/// 曾经取的是 name 表里第一条「全名」记录，那条碰巧是 Mac 平台编码、解不出来时
/// 就写成了「Unknown」—— 在阅读器的字体列表里根本看不出嵌的是什么字体。
#[test]
fn embedded_fonts_are_named_by_postscript_name() {
    if !require_cjk_font() {
        return;
    }
    let body = r#"<w:p><w:r><w:rPr><w:rFonts w:ascii="Liberation Serif" w:hAnsi="Liberation Serif" w:eastAsia="Noto Serif CJK SC"/><w:sz w:val="24"/></w:rPr><w:t>Font name 字体名称</w:t></w:r></w:p>"#;
    let names = base_fonts(&convert(&make_docx("psname.docx", body)).value.pdf);
    assert!(!names.is_empty());
    for n in &names {
        let (tag, rest) = n.split_once('+').expect("子集字体要带六个字母的前缀");
        assert!(
            tag.len() == 6 && tag.chars().all(|c| c.is_ascii_uppercase()),
            "{n}"
        );
        assert!(!rest.is_empty() && rest != "Unknown", "BaseFont 是 {n}");
    }
    // 本机有 Liberation Serif 时，它的 PostScript 名必须原样出现。
    if let Some(found) =
        pdfcore::fonts::system::SystemFonts::shared().query("Liberation Serif", false, false)
    {
        let face = ttf_parser::Face::parse(found.face.data(), found.face.index()).unwrap();
        let ps = face
            .names()
            .into_iter()
            .filter(|n| n.name_id == ttf_parser::name_id::POST_SCRIPT_NAME)
            .find_map(|n| n.to_string())
            .unwrap();
        assert!(
            names.iter().any(|n| n.ends_with(&format!("+{ps}"))),
            "{names:?} 里没有 {ps}"
        );
    }
}

/// trailer 里要有 /ID。PDF/A 要求它，部分阅读器与签名工具靠它识别文件。
#[test]
fn pdf_has_a_file_identifier() {
    let pdf = convert(&make_docx("fileid.docx", &para("文件标识")))
        .value
        .pdf;
    let doc = lopdf::Document::load_mem(&pdf).unwrap();
    let id = doc
        .trailer
        .get(b"ID")
        .expect("trailer 里没有 /ID")
        .as_array()
        .unwrap();
    assert_eq!(id.len(), 2);
    for part in id {
        assert_eq!(part.as_str().unwrap().len(), 16);
    }
}

fn all_content_ops(pdf: &[u8]) -> Vec<lopdf::content::Operation> {
    let doc = lopdf::Document::load_mem(pdf).unwrap();
    doc.get_pages()
        .values()
        .flat_map(|&p| doc.get_and_decode_page_content(p).unwrap().operations)
        .collect()
}

/// 所选字体没有的字（☑、☐ 不在中文字体里），要借回退字体画出来，而且能原样抽回。
#[test]
fn missing_glyphs_fall_back_to_a_font_that_has_them() {
    if !require_cjk_font() {
        return;
    }
    let report = convert(&make_docx("fallback.docx", &para("同意☑不同意☐")));
    let text = text_of(&report.value.pdf);
    assert!(
        text.contains("同意☑不同意☐"),
        "回退字体没有生效，抽回：{text}"
    );
    assert!(
        !report
            .warnings
            .iter()
            .any(|w| w.detail.contains("没有字形")),
        "有回退字体可用时不该报缺字：{:?}",
        report.warnings
    );
}

/// 「②」紧跟在新 run 开头、后面是数字时，会被判给西文字体，而西文字体没有它。
/// 曾经因此和别的缺字一起落到 .notdef，全被抽成了「℃」。
///
/// 西文字体没有的字，画它的应当是这个 run 自己的中文字体，而不是系统回退链里
/// 随便一个有它的字体。西文字体本身有的（Windows 的 Times New Roman 就有 ℃）照常用它。
#[test]
fn circled_digits_at_a_run_start_are_not_lost() {
    if !require_cjk_font() {
        return;
    }
    let run = |t: &str| {
        format!(
            r#"<w:r><w:rPr><w:rFonts w:ascii="Liberation Serif" w:hAnsi="Liberation Serif" w:eastAsia="宋体"/><w:sz w:val="24"/></w:rPr><w:t>{t}</w:t></w:r>"#
        )
    };
    let body = format!(
        "<w:p>{}{}{}</w:p>",
        run("第一项；"),
        run("②3月5日提交清单；"),
        run("③25℃")
    );
    let report = convert(&make_docx("circled.docx", &body));
    let pdf = &report.value.pdf;
    let text = text_of(pdf);
    assert!(
        text.contains("第一项；②3月5日提交清单；③25℃"),
        "抽回：{text}"
    );

    let frags: Vec<common::pdftext::Frag> = common::pdftext::extract(pdf)
        .into_iter()
        .flat_map(|p| p.lines)
        .flat_map(|l| l.frags)
        .collect();
    let font_of = |c: char| {
        frags
            .iter()
            .find(|f| f.text.contains(c))
            .map(|f| f.font.clone())
            .unwrap_or_else(|| panic!("找不到「{c}」：{frags:?}"))
    };
    let (cjk, latin) = (font_of('第'), font_of('2'));
    for c in ['②', '③', '℃'] {
        let font = font_of(c);
        assert!(
            font == cjk || font == latin,
            "「{c}」用了 {font}，应当用 run 自己的字体（{latin} 或 {cjk}）"
        );
    }
    assert!(
        !report
            .warnings
            .iter()
            .any(|w| w.detail.contains("没有字形")),
        "{:?}",
        report.warnings
    );
}

/// 真的哪个字体都没有的字（这里用 Unicode 尚未分配的码位），不同的缺字
/// 不能被抽成同一个字 —— 它们共用 .notdef，ToUnicode 只能给一个原文。
#[test]
fn distinct_missing_glyphs_are_not_merged_into_one_character() {
    let report = convert(&make_docx("notdef.docx", &para("甲\u{0378}乙\u{0379}丙")));
    let text = text_of(&report.value.pdf);
    assert_eq!(
        text, "甲\u{FFFD}乙\u{FFFD}丙",
        "两个不同的缺字应当都抽成 U+FFFD，而不是都变成其中一个"
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.detail.contains("没有字形")),
        "真缺字要报出来"
    );
}

/// 没有粗体字形的字体（宋体就是），要求加粗时要合成 —— 否则标题和正文一样细。
#[test]
fn bold_without_a_bold_face_is_synthesized() {
    let family = "WenQuanYi Zen Hei";
    let Some(found) = pdfcore::fonts::system::SystemFonts::shared().query(family, true, false)
    else {
        eprintln!("跳过：本机没有 {family}");
        return;
    };
    if found.face.metrics().weight >= 600 {
        eprintln!("跳过：{family} 有真粗体");
        return;
    }
    let body = format!(
        r#"<w:p><w:r><w:rPr><w:rFonts w:eastAsia="{family}"/><w:b/><w:sz w:val="24"/></w:rPr><w:t>加粗的标题</w:t></w:r></w:p>"#
    );
    let ops = all_content_ops(&convert(&make_docx("synth_bold.docx", &body)).value.pdf);
    assert!(
        ops.iter()
            .any(|o| o.operator == "Tr" && o.operands[0].as_i64().ok() == Some(2)),
        "要用描边（Tr 2）合成粗体"
    );
}

/// Wingdings 画的勾选框：本机没有 Wingdings 时，私用区码位要换成真正的「☑」。
#[test]
fn wingdings_checkbox_becomes_a_real_checkbox_without_the_font() {
    if pdfcore::fonts::system::SystemFonts::shared()
        .query("Wingdings", false, false)
        .is_some()
    {
        eprintln!("跳过：本机装了 Wingdings，原样显示就是对的");
        return;
    }
    let body = format!(
        r#"<w:p><w:r><w:rPr><w:rFonts w:ascii="Wingdings" w:hAnsi="Wingdings"/><w:sz w:val="24"/></w:rPr><w:t>{}</w:t></w:r><w:r><w:rPr><w:rFonts w:ascii="Liberation Serif" w:hAnsi="Liberation Serif" w:eastAsia="宋体"/><w:sz w:val="24"/></w:rPr><w:t>已阅读</w:t></w:r></w:p>"#,
        '\u{F0FE}'
    );
    let text = text_of(&convert(&make_docx("wingdings.docx", &body)).value.pdf);
    assert!(text.contains("☑已阅读"), "抽回：{text:?}");
}

/// 内容流里不能出现 Tc / Tw：Tw 对双字节编码不起作用，Tc 不复位会串到后面的文字。
#[test]
fn no_char_or_word_spacing_operators_in_output() {
    if !require_cjk_font() {
        return;
    }
    let body = (0..6)
        .map(|_| {
            r#"<w:p><w:pPr><w:jc w:val="both"/></w:pPr><w:r><w:rPr><w:rFonts w:ascii="Liberation Serif" w:hAnsi="Liberation Serif" w:eastAsia="宋体"/><w:sz w:val="24"/></w:rPr><w:t>两端对齐的中文段落需要把行内剩余的空间平均分给每一个字符间隔这样右边才能对齐 and some English words here too</w:t></w:r></w:p>"#
        })
        .collect::<String>();
    let ops = all_content_ops(&convert(&make_docx("no_tc.docx", &body)).value.pdf);
    assert!(!ops.iter().any(|o| o.operator == "Tc" || o.operator == "Tw"));
}

/// `w:br` 换行符不是拿来画的：不能为它去找回退字体（多嵌一个字体），也不能报缺字。
#[test]
fn line_breaks_neither_pull_in_fallback_fonts_nor_count_as_missing() {
    if !require_cjk_font() {
        return;
    }
    let body = r#"<w:p><w:r><w:rPr><w:rFonts w:ascii="Liberation Serif" w:hAnsi="Liberation Serif" w:eastAsia="宋体"/></w:rPr><w:t>第一行</w:t><w:br/><w:t>第二行 two</w:t><w:br/><w:t>第三行</w:t></w:r></w:p>"#;
    let report = convert(&make_docx("br_fonts.docx", body));
    let fonts: std::collections::BTreeSet<String> = common::pdftext::extract(&report.value.pdf)
        .into_iter()
        .flat_map(|p| p.lines)
        .flat_map(|l| l.frags)
        .map(|f| f.font)
        .collect();
    assert_eq!(fonts.len(), 2, "只该用到中文、西文两个字体：{fonts:?}");
    assert!(
        !report
            .warnings
            .iter()
            .any(|w| w.detail.contains("没有字形")),
        "换行符被当成了缺字：{:?}",
        report.warnings
    );
}

/// 连续两个 `w:br` 之间的空行照样占一行高 —— 换行符不绘制，但行高仍要由它撑起来。
#[test]
fn an_empty_line_between_two_breaks_keeps_its_height() {
    if !require_cjk_font() {
        return;
    }
    let baselines = |name: &str, body: &str| -> Vec<f32> {
        let pdf = convert(&make_docx(name, body)).value.pdf;
        common::pdftext::extract(&pdf)
            .into_iter()
            .flat_map(|p| p.lines)
            .map(|l| l.y)
            .collect()
    };
    let three = baselines(
        "br_three.docx",
        "<w:p><w:r><w:t>第一行</w:t><w:br/><w:t>第二行</w:t><w:br/><w:t>第三行</w:t></w:r></w:p>",
    );
    let gap = baselines(
        "br_gap.docx",
        "<w:p><w:r><w:t>第一行</w:t><w:br/><w:br/><w:t>第三行</w:t></w:r></w:p>",
    );
    assert_eq!((three.len(), gap.len()), (3, 2), "{three:?} {gap:?}");
    let pitch = three[0] - three[1];
    assert!(
        ((gap[0] - gap[1]) - 2.0 * pitch).abs() < 0.01,
        "空行应当占一整行：行距 {pitch}，隔一空行的两行相距 {}",
        gap[0] - gap[1]
    );
}

/// 哪一级都没写 `w:sz` 时按 10pt 排：OOXML 的缺省值，也是 LibreOffice 的实测结果。
#[test]
fn text_without_any_size_is_10pt() {
    if !require_cjk_font() {
        return;
    }
    let path = DocxBuilder::new()
        .styles(
            r#"<w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Times New Roman" w:eastAsia="宋体"/></w:rPr></w:rPrDefault></w:docDefaults>
<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>"#,
        )
        .body("<w:p><w:r><w:t>没有字号的正文 text</w:t></w:r></w:p>")
        .build("default_size.docx");
    let pdf = convert(&path).value.pdf;
    let sizes: Vec<f32> = common::pdftext::extract(&pdf)
        .into_iter()
        .flat_map(|p| p.lines)
        .flat_map(|l| l.frags)
        .map(|f| f.size)
        .collect();
    assert!(!sizes.is_empty());
    assert!(
        sizes.iter().all(|s| (s - 10.0).abs() < 1e-3),
        "字号应当都是 10pt：{sizes:?}"
    );
}

/// 空段落是只有段落标记的一行：行高与同一字体、同一字号的一行西文相同，
/// 有行网格时同样吸附到整格。
#[test]
fn an_empty_paragraph_is_as_tall_as_a_line_of_its_mark_font() {
    if !require_cjk_font() {
        return;
    }
    let fonts = r#"<w:rFonts w:ascii="Times New Roman" w:hAnsi="Times New Roman" w:eastAsia="宋体"/><w:sz w:val="32"/>"#;
    let marker = |tag: &str| {
        format!(
            r#"<w:p><w:r><w:rPr><w:rFonts w:ascii="Times New Roman" w:eastAsia="宋体"/><w:sz w:val="24"/></w:rPr><w:t>标记{tag}行</w:t></w:r></w:p>"#
        )
    };
    let empty = format!(r#"<w:p><w:pPr><w:rPr>{fonts}</w:rPr></w:pPr></w:p>"#);
    let latin = format!(r#"<w:p><w:r><w:rPr>{fonts}</w:rPr><w:t>Mm</w:t></w:r></w:p>"#);
    let body = marker("甲")
        + &empty.repeat(3)
        + &marker("乙")
        + &marker("丙")
        + &latin.repeat(3)
        + &marker("丁");
    for (name, sect) in [
        ("empty_mark.docx", ""),
        (
            "empty_mark_grid.docx",
            r#"<w:docGrid w:type="lines" w:linePitch="312"/>"#,
        ),
    ] {
        let path = DocxBuilder::new().body(&body).sect_extra(sect).build(name);
        let pages = common::pdftext::extract(&convert(&path).value.pdf);
        let gap = |a, b| common::calib::gap(&pages, a, b).expect("标记行不在同一页");
        let (empties, latins) = (gap("甲", "乙"), gap("丙", "丁"));
        assert!(
            (empties - latins).abs() < 0.01,
            "{name}：三个空段落占 {empties}pt，三行同字号西文占 {latins}pt"
        );
    }
}

/// 空段落也是一行：页面放不下就换页，而不是越过页底、把后面的内容整体往下推。
#[test]
fn empty_paragraphs_that_do_not_fit_go_to_the_next_page() {
    if !require_cjk_font() {
        return;
    }
    let empty = r#"<w:p><w:pPr><w:rPr><w:rFonts w:ascii="Times New Roman"/><w:sz w:val="24"/></w:rPr></w:pPr></w:p>"#;
    let body = empty.repeat(120) + &para("最后一段");
    let report = convert(&make_docx("many_empties.docx", &body));
    // 120 个 12pt 空段落约 1650pt，内容区一页约 697pt：最后一段应当在第 3 页。
    let pages = common::pdftext::extract(&report.value.pdf);
    let last_page = pages
        .iter()
        .position(|p| p.text().contains("最后一段"))
        .expect("找不到最后一段");
    assert_eq!(last_page, 2, "共 {} 页", pages.len());
}

/// 上一段的段后距与下一段的段前距：文档没有设置 `doNotUseHTMLParagraphAutoSpacing`
/// 时取较大值（LibreOffice 实测），设置了才相加。
#[test]
fn paragraph_spacing_collapses_unless_the_document_opts_out() {
    if !require_cjk_font() {
        return;
    }
    let line = |tag: &str, before: u32, after: u32| {
        format!(
            r#"<w:p><w:pPr><w:spacing w:before="{before}" w:after="{after}"/></w:pPr><w:r><w:rPr><w:rFonts w:ascii="Times New Roman" w:eastAsia="宋体"/><w:sz w:val="24"/></w:rPr><w:t>标记{tag}行</w:t></w:r></w:p>"#
        )
    };
    let body = line("甲", 0, 240) + &line("乙", 360, 0) + &line("丙", 0, 0) + &line("丁", 0, 0);
    let extra = |name: &str, settings: &str| {
        let path = DocxBuilder::new()
            .body(&body)
            .settings(settings)
            .build(name);
        let pages = common::pdftext::extract(&convert(&path).value.pdf);
        let gap = |a, b| common::calib::gap(&pages, a, b).expect("标记行不在同一页");
        gap("甲", "乙") - gap("丙", "丁")
    };
    let collapsed = extra("spacing_collapse.docx", "");
    let summed = extra(
        "spacing_sum.docx",
        "<w:compat><w:doNotUseHTMLParagraphAutoSpacing/></w:compat>",
    );
    assert!(
        (collapsed - 18.0).abs() < 0.01,
        "段后 12 + 段前 18 应当取 18：{collapsed}"
    );
    assert!(
        (summed - 30.0).abs() < 0.01,
        "设置了兼容选项应当相加：{summed}"
    );
}

/// 行尾的半角空格悬挂在右边距外：右对齐时可见文字贴着右边距，空格伸出去。
#[test]
fn trailing_spaces_hang_past_the_right_margin() {
    if !require_cjk_font() {
        return;
    }
    let words = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu ".repeat(4);
    let body = format!(
        r#"<w:p><w:pPr><w:jc w:val="right"/></w:pPr><w:r><w:rPr><w:rFonts w:ascii="Times New Roman" w:hAnsi="Times New Roman"/><w:sz w:val="24"/></w:rPr><w:t xml:space="preserve">{words}</w:t></w:r></w:p>"#
    );
    let path = make_docx("trailing_space.docx", &body);
    let pages = common::pdftext::extract(&convert(&path).value.pdf);
    let first = &pages[0].lines[0];
    let right = first
        .frags
        .iter()
        .map(|f| f.x + f.width)
        .fold(f32::MIN, f32::max);
    // DocxBuilder 的右边距是 1588 twips。
    let past = right - (pages[0].width - 1588.0 / 20.0);
    assert!(
        (1.0..6.0).contains(&past),
        "首行（含行尾空格）应当越过右边距一个空格宽，实际 {past}pt"
    );
}

/// 行尾的句读标点可以伸出右边距：36 个 12pt 汉字正好排满一行（436.5pt），后面的
/// 逗号留在本行；后引号不行，它不能出现在行首，只好带着前一个字换行。
#[test]
fn sentence_punctuation_hangs_past_the_right_margin() {
    if !require_cjk_font() {
        return;
    }
    let first_line_len = |name: &str, punct: char, ppr: &str| {
        let text = format!("{}{punct}{}", "测".repeat(36), "测".repeat(10));
        let body = format!(
            r#"<w:p><w:pPr>{ppr}</w:pPr><w:r><w:rPr><w:rFonts w:ascii="Times New Roman" w:eastAsia="宋体"/><w:sz w:val="24"/></w:rPr><w:t>{text}</w:t></w:r></w:p>"#
        );
        let pages = common::pdftext::extract(&convert(&make_docx(name, &body)).value.pdf);
        pages[0].lines[0].text.chars().count()
    };
    assert_eq!(first_line_len("hang_comma.docx", '，', ""), 37);
    assert_eq!(
        first_line_len("hang_justify.docx", '。', r#"<w:jc w:val="both"/>"#),
        37
    );
    assert_eq!(first_line_len("hang_quote.docx", '”', ""), 35);
    // 段落禁止标点溢出时，逗号也只能带着前一个字换行。
    assert_eq!(
        first_line_len("hang_off.docx", '，', r#"<w:overflowPunct w:val="0"/>"#),
        35
    );
}

/// 两端对齐：有半角空格的行只把空格拉开；没有空格的行拉开汉字前后的间隙，
/// 西文词内部不拉开。两种行的可见文字都排满到右边距。
#[test]
fn justification_stretches_spaces_or_cjk_gaps_but_not_inside_words() {
    if !require_cjk_font() {
        return;
    }
    // 同一段文字排两遍：以「甲」开头的左对齐、以「乙」开头的两端对齐，逐字形相减。
    let deltas = |name: &str, text: &str| -> (Vec<(String, f32)>, f32) {
        let para = |first: &str, jc: &str| {
            format!(
                r#"<w:p><w:pPr><w:jc w:val="{jc}"/></w:pPr><w:r><w:rPr><w:rFonts w:ascii="Times New Roman" w:hAnsi="Times New Roman" w:eastAsia="宋体"/><w:sz w:val="24"/></w:rPr><w:t xml:space="preserve">{first}{text}</w:t></w:r></w:p>"#
            )
        };
        let body = para("甲", "left") + &para("乙", "both");
        let pages = common::pdftext::extract(&convert(&make_docx(name, &body)).value.pdf);
        let line = |first: &str| {
            pages[0]
                .lines
                .iter()
                .find(|l| l.text.starts_with(first))
                .unwrap()
                .clone()
        };
        let (left, just) = (line("甲"), line("乙"));
        let glyphs = |l: &common::pdftext::Line| -> Vec<(String, f32)> {
            l.frags.iter().flat_map(|f| f.glyphs.clone()).collect()
        };
        let (a, b) = (glyphs(&left), glyphs(&just));
        // 第 i 个字形之后多出来的推进。
        let d: Vec<(String, f32)> = (1..a.len().min(b.len()))
            .map(|i| {
                (
                    a[i - 1].0.clone(),
                    (b[i].1 - a[i].1) - (b[i - 1].1 - a[i - 1].1),
                )
            })
            .collect();
        // 可见文字的右缘（去掉行尾空格）离右边距多远。
        let right = just
            .frags
            .iter()
            .flat_map(|f| {
                f.glyphs
                    .iter()
                    .zip(f.glyphs.iter().skip(1).map(|g| g.1).chain([f.x + f.width]))
                    .filter(|(g, _)| g.0 != " ")
                    .map(|(_, end)| end)
                    .collect::<Vec<_>>()
            })
            .fold(f32::MIN, f32::max);
        (d, pages[0].width - 1588.0 / 20.0 - right)
    };

    let (d, gap) = deltas(
        "justify_spaces.docx",
        &"两端对齐 word 与 space 的分配规则 ".repeat(8),
    );
    for (after, extra) in &d {
        if after != " " {
            assert!(
                extra.abs() < 0.01,
                "「{after}」之后不该拉开，多了 {extra}pt"
            );
        }
    }
    assert!(d.iter().any(|(a, e)| a == " " && *e > 0.1), "空格应当拉开");
    assert!(gap.abs() < 0.05, "可见文字应当排满到右边距，差 {gap}pt");

    let (d, gap) = deltas("justify_words.docx", &"两端对齐Word分配规则ABC".repeat(8));
    let latin = |s: &str| s.chars().all(|c| c.is_ascii_alphanumeric());
    for w in d.windows(2) {
        if latin(&w[0].0) && latin(&w[1].0) {
            assert!(
                w[0].1.abs() < 0.01,
                "西文词内不该拉开：「{}」之后多了 {}pt",
                w[0].0,
                w[0].1
            );
        }
    }
    assert!(gap.abs() < 0.05, "可见文字应当排满到右边距，差 {gap}pt");
}

/// `w:br w:type="page"`：之后的文字从下一页的正文顶开始；紧跟着的段前分页
/// 不再多出一张空白页。
#[test]
fn page_breaks_start_a_new_page() {
    if !require_cjk_font() {
        return;
    }
    let run = |inner: &str| {
        format!(
            r#"<w:r><w:rPr><w:rFonts w:ascii="Times New Roman" w:eastAsia="宋体"/><w:sz w:val="24"/></w:rPr>{inner}</w:r>"#
        )
    };
    let body = format!(
        r#"<w:p>{}</w:p><w:p><w:pPr><w:pageBreakBefore/></w:pPr>{}</w:p>"#,
        run(r#"<w:t>第一页</w:t><w:br w:type="page"/><w:t>第二页</w:t><w:br w:type="page"/>"#),
        run("<w:t>第三页</w:t>")
    );
    let pages = common::pdftext::extract(&convert(&make_docx("page_break.docx", &body)).value.pdf);
    let texts: Vec<String> = pages.iter().map(|p| p.text()).collect();
    assert_eq!(texts, ["第一页", "第二页", "第三页"]);
    let top = |i: usize| pages[i].lines[0].y;
    assert!(
        (top(0) - top(1)).abs() < 0.01 && (top(1) - top(2)).abs() < 0.01,
        "每页的首行应当在同一高度：{} {} {}",
        top(0),
        top(1),
        top(2)
    );
}

/// 制表位：默认位按 `w:defaultTabStop` 从左边距起算；右对齐、小数点位把文字往回让；
/// 前导符画成一串点。
#[test]
fn tabs_jump_to_their_stops() {
    if !require_cjk_font() {
        return;
    }
    let para = |tabs: &str, runs: &str| {
        format!(
            r#"<w:p><w:pPr><w:tabs>{tabs}</w:tabs></w:pPr><w:r><w:rPr><w:rFonts w:ascii="Times New Roman" w:hAnsi="Times New Roman" w:eastAsia="宋体"/><w:sz w:val="24"/></w:rPr>{runs}</w:r></w:p>"#
        )
    };
    let body = para("", "<w:t>甲</w:t><w:tab/><w:t>乙</w:t>")
        + &para(
            r#"<w:tab w:val="right" w:leader="dot" w:pos="6000"/>"#,
            "<w:t>丙</w:t><w:tab/><w:t>丁丁丁</w:t>",
        )
        + &para(
            r#"<w:tab w:val="decimal" w:pos="5000"/>"#,
            "<w:t>戊</w:t><w:tab/><w:t>1234.50</w:t>",
        );
    let path = DocxBuilder::new()
        .body(&body)
        .settings(r#"<w:defaultTabStop w:val="420"/>"#)
        .build("tabs.docx");
    let pages = common::pdftext::extract(&convert(&path).value.pdf);
    let margin = 1588.0 / 20.0;
    let glyphs = |first: char| -> Vec<(String, f32)> {
        pages[0]
            .lines
            .iter()
            .find(|l| l.text.starts_with(first))
            .unwrap_or_else(|| panic!("找不到以「{first}」开头的行"))
            .frags
            .iter()
            .flat_map(|f| f.glyphs.clone())
            .collect()
    };
    let x = |g: &[(String, f32)], c: &str| g.iter().find(|(t, _)| t == c).unwrap().1 - margin;

    let g = glyphs('甲');
    assert!(
        (x(&g, "乙") - 21.0).abs() < 0.01,
        "默认制表位：{}",
        x(&g, "乙")
    );

    let line = pages[0]
        .lines
        .iter()
        .find(|l| l.text.starts_with('丙'))
        .unwrap();
    assert!(
        (line.x1 - margin - 300.0).abs() < 0.01,
        "右对齐制表位：文字右缘在 {}",
        line.x1 - margin
    );
    let g = glyphs('丙');
    let dots: Vec<f32> = g
        .iter()
        .filter(|(t, _)| t == ".")
        .map(|(_, x)| *x - margin)
        .collect();
    assert!(dots.len() > 10, "前导点只有 {} 个", dots.len());
    assert!(dots[0] >= 12.0 - 0.01 && *dots.last().unwrap() < x(&g, "丁"));

    let g = glyphs('戊');
    assert!(
        (x(&g, ".") - 250.0).abs() < 0.01,
        "小数点对齐：{}",
        x(&g, ".")
    );
}

/// 悬挂缩进：首行从「左缩进 − 悬挂」开始，续行从左缩进开始。
#[test]
fn hanging_indent_outdents_the_first_line() {
    if !require_cjk_font() {
        return;
    }
    let body = format!(
        r#"<w:p><w:pPr><w:ind w:left="840" w:hanging="420"/></w:pPr><w:r><w:rPr><w:rFonts w:ascii="Times New Roman" w:eastAsia="宋体"/><w:sz w:val="24"/></w:rPr><w:t>{}</w:t></w:r></w:p>"#,
        "悬挂缩进的段落首行往左伸出".repeat(8)
    );
    let pages = common::pdftext::extract(&convert(&make_docx("hanging.docx", &body)).value.pdf);
    let margin = 1588.0 / 20.0;
    let starts: Vec<f32> = pages[0].lines.iter().map(|l| l.x0 - margin).collect();
    assert!(starts.len() >= 2, "{starts:?}");
    assert!((starts[0] - 21.0).abs() < 0.01, "首行起点 {}", starts[0]);
    assert!((starts[1] - 42.0).abs() < 0.01, "续行起点 {}", starts[1]);
}

/// 一整串不可断的内容比一行还宽时，在字符边界上断开，不越过右边距。
#[test]
fn unbreakable_runs_are_split_at_character_boundaries() {
    if !require_cjk_font() {
        return;
    }
    let body = format!(
        r#"<w:p><w:r><w:rPr><w:rFonts w:ascii="Times New Roman" w:hAnsi="Times New Roman"/><w:sz w:val="24"/></w:rPr><w:t>https://example.com/{}</w:t></w:r></w:p>"#,
        "abcdefghij0123456789".repeat(12)
    );
    let pages = common::pdftext::extract(&convert(&make_docx("long_url.docx", &body)).value.pdf);
    let right = pages[0].width - 1588.0 / 20.0;
    let lines = &pages[0].lines;
    assert!(lines.len() >= 3, "应当断成多行，实际 {} 行", lines.len());
    for l in lines {
        assert!(
            l.x1 <= right + 0.01,
            "有一行越过了右边距：{} > {right}",
            l.x1
        );
    }
    let text: String = lines.iter().map(|l| l.text.as_str()).collect();
    assert!(text.ends_with("0123456789"), "字断丢了：{text}");
}

/// 字符间距（`w:rPr/w:spacing`）加在每个字后面；隐藏文字（`w:vanish`）不显示也不占位置。
#[test]
fn character_spacing_and_hidden_text() {
    if !require_cjk_font() {
        return;
    }
    let run = |extra: &str, text: &str| {
        format!(
            r#"<w:r><w:rPr><w:rFonts w:ascii="Times New Roman" w:eastAsia="宋体"/>{extra}<w:sz w:val="24"/></w:rPr><w:t>{text}</w:t></w:r>"#
        )
    };
    let body = format!(
        "<w:p>{}</w:p><w:p>{}{}{}</w:p>",
        run(r#"<w:spacing w:val="100"/>"#, "甲测试"),
        run("", "乙"),
        run("<w:vanish/>", "隐藏的字"),
        run("", "丙")
    );
    let pages =
        common::pdftext::extract(&convert(&make_docx("spacing_vanish.docx", &body)).value.pdf);
    let margin = 1588.0 / 20.0;
    let line = |first: char| {
        pages[0]
            .lines
            .iter()
            .find(|l| l.text.starts_with(first))
            .unwrap()
            .frags
            .iter()
            .flat_map(|f| f.glyphs.clone())
            .collect::<Vec<_>>()
    };
    let g = line('甲');
    let x = |c: &str| g.iter().find(|(t, _)| t == c).unwrap().1 - margin;
    assert!((x("测") - 17.0).abs() < 0.01, "「测」在 {}", x("测"));
    assert!((x("试") - 34.0).abs() < 0.01, "「试」在 {}", x("试"));

    let g = line('乙');
    let text: String = g.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(text, "乙丙", "隐藏文字不该出现");
    assert!((g[1].1 - g[0].1 - 12.0).abs() < 0.01, "隐藏文字不该占位置");
}

/// `w:sym` 用 Wingdings 画的勾选框：本机没有 Wingdings 时换成真正的「☑」。
#[test]
fn sym_elements_become_real_symbols_without_the_font() {
    if pdfcore::fonts::system::SystemFonts::shared()
        .query("Wingdings", false, false)
        .is_some()
    {
        eprintln!("跳过：本机装了 Wingdings，原样显示就是对的");
        return;
    }
    if !require_cjk_font() {
        return;
    }
    let body = r#"<w:p><w:r><w:rPr><w:rFonts w:ascii="Times New Roman" w:eastAsia="宋体"/><w:sz w:val="24"/></w:rPr><w:sym w:font="Wingdings" w:char="F0FE"/><w:t>同意</w:t><w:sym w:font="Wingdings" w:char="A8"/><w:t>不同意</w:t></w:r></w:p>"#;
    let text = text_of(&convert(&make_docx("sym.docx", body)).value.pdf);
    assert!(text.contains("☑同意◻不同意"), "抽回：{text}");
}

/// 突出显示画在文字底下；点线下划线带虚线样式；双下划线是两条、用下划线自己的颜色。
#[test]
fn highlight_and_underline_styles_are_drawn() {
    if !require_cjk_font() {
        return;
    }
    let run = |rpr: &str, text: &str| {
        format!(
            r#"<w:r><w:rPr><w:rFonts w:ascii="Times New Roman" w:eastAsia="宋体"/>{rpr}<w:sz w:val="24"/></w:rPr><w:t>{text}</w:t></w:r>"#
        )
    };
    let fill_of = |ops: &[lopdf::content::Operation], i: usize| -> Option<[f32; 3]> {
        // 往回找最近的一个 rg。
        ops[..i].iter().rev().find(|o| o.operator == "rg").map(|o| {
            let n = |k: usize| o.operands[k].as_float().unwrap();
            [n(0), n(1), n(2)]
        })
    };

    let body = format!(
        "<w:p>{}</w:p>",
        run(r#"<w:highlight w:val="yellow"/>"#, "突出显示")
    );
    let ops = all_content_ops(&convert(&make_docx("highlight.docx", &body)).value.pdf);
    let first_text = ops.iter().position(|o| o.operator == "BT").unwrap();
    assert!(
        ops[..first_text]
            .iter()
            .enumerate()
            .any(|(i, o)| o.operator == "re" && fill_of(&ops, i) == Some([1.0, 1.0, 0.0])),
        "黄色底色要在文字之前画"
    );

    let body = format!(
        "<w:p>{}</w:p>",
        run(r#"<w:u w:val="dotted"/>"#, "点线下划线")
    );
    let ops = all_content_ops(&convert(&make_docx("dotted.docx", &body)).value.pdf);
    assert!(
        ops.iter()
            .any(|o| o.operator == "d" && o.operands[0].as_array().is_ok_and(|a| !a.is_empty())),
        "点线下划线要设虚线样式"
    );

    let body = format!(
        "<w:p>{}</w:p>",
        run(r#"<w:u w:val="double" w:color="FF0000"/>"#, "双下划线")
    );
    let ops = all_content_ops(&convert(&make_docx("double_u.docx", &body)).value.pdf);
    let red_rects = ops
        .iter()
        .enumerate()
        .filter(|(i, o)| o.operator == "re" && fill_of(&ops, *i) == Some([1.0, 0.0, 0.0]))
        .count();
    assert_eq!(red_rects, 2, "红色双下划线应当是两条");
}

/// 上下标画小一号并抬高或压低；`w:position` 按半磅精确升降；全部大写、小型大写。
#[test]
fn superscript_position_and_caps() {
    if !require_cjk_font() {
        return;
    }
    let para = |rpr: &str, text: &str| {
        format!(
            r#"<w:p><w:r><w:rPr><w:rFonts w:ascii="Times New Roman" w:hAnsi="Times New Roman" w:eastAsia="宋体"/><w:sz w:val="24"/></w:rPr><w:t>甲甲</w:t></w:r><w:r><w:rPr><w:rFonts w:ascii="Times New Roman" w:hAnsi="Times New Roman" w:eastAsia="宋体"/>{rpr}<w:sz w:val="24"/></w:rPr><w:t>{text}</w:t></w:r></w:p>"#
        )
    };
    let body = para(r#"<w:vertAlign w:val="superscript"/>"#, "2")
        + &para(r#"<w:position w:val="6"/>"#, "4")
        + &para("<w:caps/>", "Caps")
        + &para("<w:smallCaps/>", "Small");
    let pages = common::pdftext::extract(&convert(&make_docx("sup_caps.docx", &body)).value.pdf);
    let frags: Vec<&common::pdftext::Frag> = pages[0].lines.iter().flat_map(|l| &l.frags).collect();
    let find = |t: &str| {
        *frags
            .iter()
            .find(|f| f.text == t)
            .unwrap_or_else(|| panic!("找不到「{t}」：{frags:?}"))
    };
    let base = |t: &str| {
        // 同一段的「甲甲」：y 离它最近的那个（被抬高的字会被抽成单独一行，不能按顺序找）。
        let y = find(t).y;
        frags
            .iter()
            .filter(|f| f.text == "甲甲")
            .map(|f| f.y)
            .min_by(|a, b| (a - y).abs().total_cmp(&(b - y).abs()))
            .unwrap()
    };

    let sup = find("2");
    assert!(
        (sup.size - 12.0 * 0.58).abs() < 0.01,
        "上标字号 {}",
        sup.size
    );
    assert!(
        sup.y - base("2") > 2.0,
        "上标没有抬高：{}",
        sup.y - base("2")
    );

    let raised = find("4");
    assert!(
        (raised.y - base("4") - 3.0).abs() < 0.01,
        "w:position 应当抬高 3pt"
    );

    assert!(frags.iter().any(|f| f.text == "CAPS"), "全部大写");
    let small = frags
        .iter()
        .find(|f| f.text == "MALL")
        .expect("小型大写的小写字母");
    assert!(
        (small.size - 9.6).abs() < 0.01,
        "小型大写字号 {}",
        small.size
    );
}

/// 外部超链接写成 PDF 的 Link 注释：网址原样，点击区域盖住链接文字。
#[test]
fn hyperlinks_become_clickable() {
    if !require_cjk_font() {
        return;
    }
    let url = "https://example.com/path?a=1&b=2";
    let mut doc = DocxBuilder::new();
    let rid = doc.hyperlink(url);
    let run = |text: &str| {
        format!(
            r#"<w:r><w:rPr><w:rFonts w:ascii="Times New Roman" w:eastAsia="宋体"/><w:sz w:val="24"/></w:rPr><w:t>{text}</w:t></w:r>"#
        )
    };
    let body = format!(
        r#"<w:p>{}<w:hyperlink r:id="{rid}">{}{}</w:hyperlink>{}</w:p>"#,
        run("详见"),
        run("链接"),
        run("文字"),
        run("。")
    );
    let path = doc.body(&body).build("hyperlink.docx");
    let pdf = convert(&path).value.pdf;

    let lo = lopdf::Document::load_mem(&pdf).unwrap();
    let page = *lo.get_pages().values().next().unwrap();
    let annots = lo.get_page_annotations(page).expect("第一页应当有注释");
    assert_eq!(annots.len(), 1, "同一个链接的两段应当合成一块");
    let a = annots[0];
    let action = a.get(b"A").unwrap().as_dict().unwrap();
    assert_eq!(
        action.get(b"URI").unwrap().as_str().unwrap(),
        url.as_bytes()
    );

    // 点击区域的横向范围要盖住「链接文字」四个字。
    let rect: Vec<f32> = a
        .get(b"Rect")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o.as_float().unwrap())
        .collect();
    let glyphs: Vec<(String, f32)> = common::pdftext::extract(&pdf)[0].lines[0]
        .frags
        .iter()
        .flat_map(|f| f.glyphs.clone())
        .collect();
    let x = |c: &str| glyphs.iter().find(|(t, _)| t == c).unwrap().1;
    assert!(
        rect[0] <= x("链") + 0.01 && rect[2] >= x("字") + 11.99,
        "{rect:?}"
    );
    assert!(rect[2] <= x("。") + 0.01, "不该盖到链接后面的字：{rect:?}");
}

/// 版流控制。行高一律用 20pt 固定行距，与字体无关：版心 697.9pt 放得下 34 行。
#[test]
fn keep_next_keep_lines_widows_and_contextual_spacing() {
    if !require_cjk_font() {
        return;
    }
    // `after` 是段后距（twip）。
    let para_after = |after: u32, ppr: &str, text: &str| {
        format!(
            r#"<w:p><w:pPr><w:spacing w:after="{after}" w:line="400" w:lineRule="exact"/>{ppr}</w:pPr><w:r><w:rPr><w:rFonts w:ascii="Times New Roman" w:eastAsia="宋体"/><w:sz w:val="24"/></w:rPr><w:t>{text}</w:t></w:r></w:p>"#
        )
    };
    let para = |ppr: &str, text: &str| para_after(0, ppr, text);
    let filler = |n: usize| {
        (0..n)
            .map(|i| para("", &format!("填充行{i}")))
            .collect::<String>()
    };
    // 三行的段落：每行 36 个字排满。
    let three_lines = "三行段落的文字".repeat(15);
    let page_of = |name: &str, body: &str, needle: &str| -> usize {
        let pages = common::pdftext::extract(&convert(&make_docx(name, body)).value.pdf);
        pages
            .iter()
            .position(|p| p.text().contains(needle))
            .unwrap_or_else(|| panic!("{name}：找不到「{needle}」"))
    };

    // 33 行之后：标题（第 34 行）放得下，下一段的第一行放不下 —— 标题跟着下一段换页。
    let body = filler(33) + &para("<w:keepNext/>", "标题") + &para("", "正文");
    assert_eq!(page_of("keep_next.docx", &body, "标题"), 1);
    let body = filler(33) + &para("", "标题") + &para("", "正文");
    assert_eq!(page_of("no_keep_next.docx", &body, "标题"), 0);

    // 32 行之后，三行的段落只放得下两行：段中不分页就整段挪走。
    let body = filler(32) + &para(r#"<w:keepLines/><w:widowControl w:val="0"/>"#, &three_lines);
    assert_eq!(page_of("keep_lines.docx", &body, "三行段落"), 1);
    // 孤行控制（缺省开着）：两行留下、一行落到下一页会成孤行，往下挪一行又成了
    // 页底孤行 —— 整段挪走。
    let body = filler(32) + &para("", &three_lines);
    assert_eq!(page_of("widow.docx", &body, "三行段落"), 1);
    // 明确关掉时照常拆开。
    let body = filler(32) + &para(r#"<w:widowControl w:val="0"/>"#, &three_lines);
    assert_eq!(page_of("no_widow.docx", &body, "三行段落"), 0);

    // 同一样式的相邻段落之间不加段距。
    let spaced = |ctx: &str| {
        (0..3)
            .map(|i| para_after(400, ctx, &format!("标记{i}行")))
            .collect::<String>()
    };
    let gap = |name: &str, ctx: &str| {
        let pages = common::pdftext::extract(&convert(&make_docx(name, &spaced(ctx))).value.pdf);
        pages[0].lines[0].y - pages[0].lines[1].y
    };
    assert!((gap("spaced.docx", "") - 40.0).abs() < 0.01);
    assert!((gap("contextual.docx", "<w:contextualSpacing/>") - 20.0).abs() < 0.01);
}

/// 在 PDF 里找含 `needle` 的第一个文字片段，返回它的字体（BaseFont）。
fn font_of(frags: &[common::pdftext::Frag], needle: &str) -> String {
    frags
        .iter()
        .find(|f| f.text.contains(needle))
        .map(|f| f.font.clone())
        .unwrap_or_else(|| panic!("找不到「{needle}」：{frags:?}"))
}

fn frags_of(pdf: &[u8]) -> Vec<common::pdftext::Frag> {
    common::pdftext::extract(pdf)
        .into_iter()
        .flat_map(|p| p.lines)
        .flat_map(|l| l.frags)
        .collect()
}

/// 主题字体：docDefaults 只写了主题字体时按主题部件落实成字体名；同一个
/// `w:rFonts` 里主题字体优先于字体名。两个西文字体从本机现有的里挑，各平台都能跑。
#[test]
fn theme_fonts_come_from_the_theme_part() {
    use pdfcore::fonts::system::{SystemFonts, LATIN_SANS_PREFERENCE, LATIN_SERIF_PREFERENCE};
    let fonts = SystemFonts::shared();
    let (Some(serif), Some(sans)) = (
        fonts.find(LATIN_SERIF_PREFERENCE, false, false),
        fonts.find(LATIN_SANS_PREFERENCE, false, false),
    ) else {
        eprintln!("跳过：本机找不到衬线、无衬线两种西文字体");
        return;
    };
    let (serif, sans) = (serif.family, sans.family);
    let para = |rfonts: &str, text: &str| {
        format!(
            r#"<w:p><w:r><w:rPr>{rfonts}<w:sz w:val="24"/></w:rPr><w:t>{text}</w:t></w:r></w:p>"#
        )
    };
    let body = [
        para("", "Minor"),
        para(
            r#"<w:rFonts w:asciiTheme="majorHAnsi" w:hAnsiTheme="majorHAnsi"/>"#,
            "Major",
        ),
        para(
            &format!(r#"<w:rFonts w:ascii="{sans}" w:hAnsi="{sans}" w:asciiTheme="majorHAnsi"/>"#),
            "Both",
        ),
        para(
            &format!(r#"<w:rFonts w:ascii="{sans}" w:hAnsi="{sans}"/>"#),
            "Sans",
        ),
        para(
            &format!(r#"<w:rFonts w:ascii="{serif}" w:hAnsi="{serif}"/>"#),
            "Serif",
        ),
    ]
    .concat();
    let path = DocxBuilder::new()
        .styles(r#"<w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:asciiTheme="minorHAnsi" w:hAnsiTheme="minorHAnsi" w:eastAsiaTheme="minorEastAsia"/></w:rPr></w:rPrDefault><w:pPrDefault/></w:docDefaults><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>"#)
        .settings(&format!(
            r#"{}<w:themeFontLang w:val="en-US" w:eastAsia="zh-CN"/>"#,
            common::docx::DEFAULT_SETTINGS
        ))
        .theme(&common::docx::font_theme((&serif, "宋体"), (&sans, "宋体")))
        .body(&body)
        .build("theme_fonts.docx");
    let frags = frags_of(&convert(&path).value.pdf);
    let font = |t| font_of(&frags, t);
    assert_ne!(font("Sans"), font("Serif"));
    assert_eq!(font("Minor"), font("Sans"), "正文主题字体");
    assert_eq!(font("Major"), font("Serif"), "标题主题字体");
    assert_eq!(font("Both"), font("Serif"), "同一个元素里主题字体优先");
}

/// 每个字用哪套字体：拉丁补充里的 × 是西文；引号跟随前一个字（跨 run 也一样），
/// 段首的算西文；`w:hint="eastAsia"` 的 run 里引号用中文字体。
#[test]
fn ambiguous_characters_follow_context_and_the_east_asia_hint() {
    if !require_cjk_font() {
        return;
    }
    let run = |hint: &str, text: &str| {
        format!(
            r#"<w:r><w:rPr><w:rFonts w:ascii="Liberation Serif" w:hAnsi="Liberation Serif" w:eastAsia="宋体"{hint}/><w:sz w:val="24"/></w:rPr><w:t>{text}</w:t></w:r>"#
        )
    };
    let body = format!(
        "<w:p>{}{}</w:p><w:p>{}</w:p><w:p>{}</w:p>",
        run("", "甲×乙"),
        run("", "“丙”"),
        run("", "AB“CD”"),
        run(r#" w:hint="eastAsia""#, "EF‘GH’"),
    );
    let frags = frags_of(&convert(&make_docx("ambiguous_chars.docx", &body)).value.pdf);
    let font = |t| font_of(&frags, t);
    let (cjk, latin) = (font("甲"), font("AB"));
    assert_ne!(cjk, latin);
    assert_eq!(font("×"), latin, "× 是西文字符");
    assert_eq!(font("“丙"), cjk, "引号跟随上一个 run 末尾的汉字");
    assert_eq!(font("AB“CD”"), latin, "跟在西文后面的引号用西文字体");
    assert_eq!(font("‘"), cjk, "w:hint=eastAsia 时引号用中文字体");
    assert_eq!(font("EF"), latin, "字母不受 hint 影响");
}

/// 中西文间距只加在汉字与西文字母、数字之间，中文标点、西文符号两侧不加。
#[test]
fn autospace_only_between_ideographs_and_alphanumerics() {
    if !require_cjk_font() {
        return;
    }
    let cases = ["中a", "，a", "中×", "中1"];
    let body: String = cases.iter().map(|c| para(c)).collect();
    let pages = common::pdftext::extract(&convert(&make_docx("autospace.docx", &body)).value.pdf);
    let advances: Vec<f32> = pages[0]
        .lines
        .iter()
        .map(|l| {
            let g: Vec<f32> = l
                .frags
                .iter()
                .flat_map(|f| &f.glyphs)
                .map(|(_, x)| *x)
                .collect();
            g[1] - g[0]
        })
        .collect();
    // 12 磅的全角字宽 12pt，间距 0.2em = 2.4pt。
    for (case, (got, want)) in cases
        .iter()
        .zip(advances.iter().zip([14.4, 12.0, 12.0, 14.4]))
    {
        assert!(
            (got - want).abs() < 0.01,
            "「{case}」第二个字离第一个字 {got}，应为 {want}"
        );
    }
}

/// 段落边框与底纹：红头线、合成一个框的相邻段落、分隔线、底纹，以及跨页时框在
/// 两页各自收口。几何关系都按相对量断言，不依赖字体度量。
#[test]
fn paragraph_borders_and_shading_are_drawn() {
    use common::pdfpaths::{self, Path};
    if !require_cjk_font() {
        return;
    }
    // 固定行距 20pt：一页 34 行，与字体无关。
    let p = |ppr: &str, text: &str| {
        format!(
            r#"<w:p><w:pPr><w:spacing w:line="400" w:lineRule="exact"/>{ppr}</w:pPr><w:r><w:rPr><w:rFonts w:ascii="Times New Roman" w:eastAsia="宋体"/><w:sz w:val="24"/></w:rPr><w:t>{text}</w:t></w:r></w:p>"#
        )
    };
    let side = |s: &str, color: &str, sz: u32, space: u32| {
        format!(r#"<w:{s} w:val="single" w:sz="{sz}" w:space="{space}" w:color="{color}"/>"#)
    };
    let red = format!("<w:pBdr>{}</w:pBdr>", side("bottom", "FF0000", 12, 1));
    let boxed = |between: bool, space: u32| {
        let mut s: String = ["top", "left", "bottom", "right"]
            .iter()
            .map(|s| side(s, "000000", 4, space))
            .collect();
        if between {
            s += &side("between", "000000", 4, space);
        }
        format!("<w:pBdr>{s}</w:pBdr>")
    };
    let convert_body = |name: &str, body: &str| {
        let pdf = convert(&make_docx(name, body)).value.pdf;
        (common::pdftext::extract(&pdf), pdfpaths::extract(&pdf), pdf)
    };
    let color_is = |p: &Path, c: [f32; 3]| p.color.iter().zip(c).all(|(a, b)| (a - b).abs() < 0.01);
    let horizontal = |paths: &[Path], c: [f32; 3]| -> Vec<Path> {
        paths
            .iter()
            .filter(|p| color_is(p, c) && p.w() > p.h())
            .cloned()
            .collect()
    };
    const RED: [f32; 3] = [1.0, 0.0, 0.0];
    const BLACK: [f32; 3] = [0.0, 0.0, 0.0];

    // 红头线：下边框 1.5pt、距文字 1pt，本段因此高出 2.5pt；线横跨整个版心。
    let gap = |pages: &[common::pdftext::PageText]| pages[0].lines[0].y - pages[0].lines[1].y;
    let (plain, _, _) = convert_body("red_plain.docx", &(p("", "标题") + &p("", "正文")));
    let (text, paths, _) = convert_body("red_line.docx", &(p(&red, "标题") + &p("", "正文")));
    assert!((gap(&text) - gap(&plain) - 2.5).abs() < 0.01);
    let line = &horizontal(&paths[0], RED)[..];
    assert_eq!(line.len(), 1, "{paths:?}");
    assert!((line[0].h() - 1.5).abs() < 0.01 && (line[0].w() - 436.5).abs() < 0.05);
    assert!(line[0].bbox[1] > text[0].lines[1].y && line[0].bbox[3] < text[0].lines[0].y);

    // 边框相同的相邻段落合成一个框：中间没有横线，左边框从头连到尾；
    // 写了 between 才在中间画一条。
    for (between, name) in [(false, "box_group.docx"), (true, "box_between.docx")] {
        let body = p(&boxed(between, 1), "框一") + &p(&boxed(between, 1), "框二");
        let (text, paths, _) = convert_body(name, &body);
        assert_eq!(horizontal(&paths[0], BLACK).len(), 2 + between as usize);
        let left = paths[0]
            .iter()
            .filter(|p| color_is(p, BLACK) && p.h() > p.w())
            .map(|p| p.bbox[0])
            .fold(f32::MAX, f32::min);
        let spans_both = paths[0].iter().any(|p| {
            p.bbox[0] == left && p.bbox[3] > text[0].lines[0].y && p.bbox[1] < text[0].lines[1].y
        });
        assert!(spans_both, "左边框应当连通两段：{paths:?}");
    }

    // 底纹盖住文字所在的行，而且先画、在文字下面。
    let shd = r#"<w:shd w:val="clear" w:color="auto" w:fill="D9D9D9"/>"#;
    let (text, paths, pdf) = convert_body("shading.docx", &p(shd, "底纹"));
    let fill = paths[0]
        .iter()
        .find(|p| !p.stroke && color_is(p, [0.85; 3]))
        .expect("应有底纹");
    assert!(fill.bbox[1] < text[0].lines[0].y && text[0].lines[0].y < fill.bbox[3]);
    let ops: Vec<String> = all_content_ops(&pdf)
        .into_iter()
        .map(|o| o.operator)
        .collect();
    let first = |names: &[&str]| ops.iter().position(|o| names.contains(&o.as_str()));
    assert!(first(&["f"]) < first(&["Tj", "TJ"]), "底纹要画在文字下面");

    // 跨页：前 30 行填充后第一页还剩 97.9pt。上下边框各占 9.5pt，8 行的带框段落
    // 只放得下 3 行 —— 不给下边框留地方的话会放 4 行。两页各自是一个完整的框。
    let filler: String = (0..30).map(|i| p("", &format!("填充{i}"))).collect();
    let body = filler + &p(&boxed(false, 9), &"边框段落".repeat(70));
    let (text, paths, _) = convert_body("box_split.docx", &body);
    assert_eq!(text.len(), 2);
    assert_eq!(text[0].lines.len(), 33, "第一页：30 行填充 + 3 行带框");
    for (page, paths) in paths.iter().enumerate() {
        assert_eq!(
            horizontal(paths, BLACK).len(),
            2,
            "第 {} 页的框要收口",
            page + 1
        );
    }
}

/// 多节：每节用自己的纸张与边距；偶数页起时页码奇偶不对就空出一页；连续分节
/// 不换页，左边距从分节处起生效。
#[test]
fn sections_have_their_own_pages() {
    if !require_cjk_font() {
        return;
    }
    let sect = |kind: &str, (w, h): (u32, u32), left: u32| {
        format!(
            r#"<w:sectPr><w:type w:val="{kind}"/><w:pgSz w:w="{w}" w:h="{h}"/><w:pgMar w:top="1440" w:right="1588" w:bottom="1440" w:left="{left}" w:header="851" w:footer="992"/></w:sectPr>"#
        )
    };
    let para = |text: &str, sect: &str| {
        format!(
            r#"<w:p><w:pPr>{sect}</w:pPr><w:r><w:rPr><w:rFonts w:ascii="Times New Roman" w:eastAsia="宋体"/><w:sz w:val="24"/></w:rPr><w:t>{text}</w:t></w:r></w:p>"#
        )
    };
    let (portrait, landscape) = ((11906, 16838), (16838, 11906));
    let body = [
        para("第一节", &sect("nextPage", portrait, 1588)),
        para("第二节横向", &sect("nextPage", landscape, 2000)),
        para("第三节", &sect("evenPage", portrait, 1588)),
        para("第四节", &sect("continuous", portrait, 3000)),
        para("最后一节", ""),
    ]
    .concat();
    let path = DocxBuilder::new()
        .body(&body)
        .sect_extra(r#"<w:type w:val="continuous"/>"#)
        .build("sections.docx");
    let pdf = convert(&path).value.pdf;

    let doc = lopdf::Document::load_mem(&pdf).unwrap();
    let sizes: Vec<(f32, f32)> = doc
        .get_pages()
        .values()
        .map(|&id| {
            let b = doc.get_dictionary(id).unwrap().get(b"MediaBox").unwrap();
            let v: Vec<f32> = b
                .as_array()
                .unwrap()
                .iter()
                .map(|o| o.as_float().unwrap())
                .collect();
            (v[2], v[3])
        })
        .collect();
    let a4 = (595.3, 841.9);
    assert_eq!(
        sizes.len(),
        4,
        "第二节占第 2 页；第三节要从偶数页起，空出第 3 页"
    );
    for (got, want) in sizes.iter().zip([a4, (841.9, 595.3), a4, a4]) {
        assert!(
            (got.0 - want.0).abs() < 0.1 && (got.1 - want.1).abs() < 0.1,
            "{sizes:?}"
        );
    }

    let pages = common::pdftext::extract(&pdf);
    let left = |page: usize, text: &str| {
        pages[page]
            .lines
            .iter()
            .find(|l| l.text.contains(text))
            .map(|l| l.x0)
            .unwrap_or_else(|| panic!("第 {} 页找不到「{text}」", page + 1))
    };
    assert!((left(0, "第一节") - 79.4).abs() < 0.01);
    assert!((left(1, "第二节") - 100.0).abs() < 0.01);
    assert!(pages[2].lines.is_empty(), "空出来的一页");
    assert!((left(3, "第三节") - 79.4).abs() < 0.01);
    assert!(
        (left(3, "第四节") - 150.0).abs() < 0.01,
        "连续分节：同一页、新的左边距"
    );
    assert!((left(3, "最后一节") - 79.4).abs() < 0.01);
}
