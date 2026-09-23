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

/// 自动编号被丢弃时必须汇总报告，而且只报一条。
///
/// 静默丢掉编号，用户拿到的就是一份没有序号的诉讼请求 —— 这正是「诚实失败」
/// 要防的情形。但也不能逐段报：50 项的列表报 50 条警告等于没报。
#[test]
fn dropped_numbering_is_reported_once() {
    if !require_cjk_font() {
        return;
    }
    let num = r#"<w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr>"#;
    let body: String = (0..5)
        .map(|i| {
            format!(
                r#"<w:p><w:pPr>{num}</w:pPr><w:r><w:rPr><w:rFonts w:eastAsia="宋体"/>
<w:sz w:val="24"/></w:rPr><w:t>第{i}项条款</w:t></w:r></w:p>"#
            )
        })
        .collect();
    let report = convert(&make_docx_with_sect("numbering.docx", &body, ""));

    let hits: Vec<_> = report
        .warnings
        .iter()
        .filter(|w| w.detail.contains("自动编号"))
        .collect();
    assert_eq!(hits.len(), 1, "编号警告应当只汇总成一条，实际 {hits:?}");
    assert!(
        hits[0].detail.contains('5'),
        "警告里要说清有几段受影响：{}",
        hits[0].detail
    );

    // 正文本身不能丢。
    let text = text_of(&report.value.pdf);
    assert!(text.contains("第0项条款") && text.contains("第4项条款"));
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
