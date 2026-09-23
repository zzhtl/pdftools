//! 与 LibreOffice 比对用的构造探针。
//!
//! 每个探针只考一类排版行为，内容造得足够长、能跨页，这样分页差异才显现得出来。
//! 字体一律写本机真实存在的族名（见 [`super::docx::probe_para`]），两边才是同一个字体。
//!
//! 首行缩进要同时写 `w:firstLine` 与 `w:firstLineChars`：LibreOffice 只认前者，
//! 真实文书（Word、WPS 产出）也总是两个都写。
//!
//! 探针在这里只负责「造文档」；期望值由各里程碑在 `tests/oracle.rs` 里按名字挂上去。

use super::docx::{probe_para, DocxBuilder};

pub struct Probe {
    pub name: &'static str,
    pub doc: DocxBuilder,
}

const GRID_312: &str = r#"<w:docGrid w:type="lines" w:linePitch="312"/>"#;

/// 确定性的中文填充文字，带标点、数字和西文，长度 `n` 个字符左右。
pub fn filler(seed: usize, n: usize) -> String {
    const BASE: &str = "本合同自双方签字之日起生效，有效期为一年，期满前一个月双方可协商续签。\
甲方应于2024年3月15日前支付首期款项共计12,580元，并提供Invoice与银行回单复印件。\
乙方收到款项后应在十个工作日内完成交付，逾期交付的，每日按合同总价的万分之五支付违约金。";
    let chars: Vec<char> = BASE.chars().collect();
    (0..n)
        .map(|i| chars[(seed * 7 + i) % chars.len()])
        .collect()
}

fn paras(count: usize, ppr: &str, len: usize) -> String {
    (0..count)
        .map(|i| probe_para(ppr, &filler(i, len)))
        .collect()
}

/// 空段落探针。`bare` 是没有任何属性的空段落怎么写：Word 写成 `<w:p/>`。
pub fn empty_paragraphs(bare: &str) -> DocxBuilder {
    let body = (0..8)
        .map(|i| {
            probe_para("", &filler(i, 40))
                + &r#"<w:p><w:pPr><w:rPr><w:sz w:val="24"/></w:rPr></w:pPr></w:p>"#.repeat(6)
                + bare
        })
        .collect::<String>();
    DocxBuilder::new().body(&body).sect_extra(GRID_312)
}

pub fn all() -> Vec<Probe> {
    let mut v = Vec::new();
    let mut add = |name: &'static str, doc: DocxBuilder| v.push(Probe { name, doc });

    add(
        "grid312_line312",
        DocxBuilder::new()
            .body(&paras(
                60,
                r#"<w:spacing w:line="312" w:lineRule="auto"/><w:ind w:firstLine="480" w:firstLineChars="200"/>"#,
                90,
            ))
            .sect_extra(GRID_312),
    );

    add(
        "nogrid_single",
        DocxBuilder::new().body(&paras(
            60,
            r#"<w:spacing w:line="240" w:lineRule="auto"/>"#,
            90,
        )),
    );

    add(
        "exact_and_atleast",
        DocxBuilder::new().body(
            &(paras(20, r#"<w:spacing w:line="400" w:lineRule="exact"/>"#, 90)
                + &paras(20, r#"<w:spacing w:line="400" w:lineRule="atLeast"/>"#, 90)),
        ),
    );

    add(
        "space_before_after",
        DocxBuilder::new()
            .body(&paras(
                50,
                r#"<w:spacing w:before="156" w:after="156" w:line="312" w:lineRule="auto"/>"#,
                70,
            ))
            .sect_extra(GRID_312),
    );

    add(
        "indent_hanging",
        DocxBuilder::new().body(
            &(paras(15, r#"<w:ind w:left="720" w:hanging="360"/>"#, 120)
                + &paras(15, r#"<w:ind w:left="420" w:right="420"/>"#, 120)),
        ),
    );

    add(
        "justify_both",
        DocxBuilder::new().body(
            &(paras(20, r#"<w:jc w:val="both"/>"#, 150)
                + &(0..10)
                    .map(|_| {
                        probe_para(
                            r#"<w:jc w:val="both"/>"#,
                            "The court finds that the plaintiff has provided sufficient evidence \
                         of the repair costs, including receipts and invoices dated March 2024, \
                         and that the defendant failed to rebut these facts at trial.",
                        )
                    })
                    .collect::<String>()),
        ),
    );

    add(
        "center_right",
        DocxBuilder::new().body(
            &(paras(10, r#"<w:jc w:val="center"/>"#, 30)
                + &paras(10, r#"<w:jc w:val="right"/>"#, 30)),
        ),
    );

    add("empty_paragraphs", empty_paragraphs("<w:p/>"));

    add("empty_document", DocxBuilder::new());

    let mixed = (0..20)
        .map(|i| {
            let run = |sz: u32, t: String| {
                format!(
                    r#"<w:r><w:rPr><w:rFonts w:ascii="Liberation Serif" w:hAnsi="Liberation Serif" w:eastAsia="Noto Serif CJK SC"/><w:sz w:val="{sz}"/></w:rPr><w:t xml:space="preserve">{t}</w:t></w:r>"#
                )
            };
            format!(
                "<w:p>{}{}{}{}</w:p>",
                run(21, filler(i, 20)),
                run(32, filler(i + 1, 8)),
                run(24, filler(i + 2, 30)),
                run(44, filler(i + 3, 4))
            )
        })
        .collect::<String>();
    add("mixed_sizes", DocxBuilder::new().body(&mixed));

    add(
        "long_unbreakable",
        DocxBuilder::new().body(
            &(0..6)
                .map(|i| {
                    probe_para(
                        "",
                        &format!("链接：https://example.com/{}{}", "a1b2c3d4e5".repeat(12), i),
                    ) + &probe_para("", &"1234567890".repeat(15))
                })
                .collect::<String>(),
        ),
    );

    let breaks = (0..6)
        .map(|i| {
            format!(
                r#"<w:p><w:r><w:rPr><w:rFonts w:ascii="Liberation Serif" w:hAnsi="Liberation Serif" w:eastAsia="Noto Serif CJK SC"/><w:sz w:val="24"/></w:rPr><w:t>{}</w:t><w:br/><w:t>{}</w:t><w:br w:type="page"/><w:t>{}</w:t></w:r></w:p>"#,
                filler(i, 30),
                filler(i + 1, 30),
                filler(i + 2, 30)
            )
        })
        .collect::<String>();
    add("br_line_and_page", DocxBuilder::new().body(&breaks));

    add(
        "page_break_before",
        DocxBuilder::new().body(
            &(0..6)
                .map(|i| probe_para(r#"<w:pageBreakBefore/>"#, &filler(i, 20)) + &paras(3, "", 80))
                .collect::<String>(),
        ),
    );

    add(
        "tabs_default",
        DocxBuilder::new().body(
            &(0..20)
                .map(|i| {
                    format!(
                        r#"<w:p><w:r><w:rPr><w:rFonts w:ascii="Liberation Serif" w:hAnsi="Liberation Serif" w:eastAsia="Noto Serif CJK SC"/><w:sz w:val="24"/></w:rPr><w:t>姓名</w:t><w:tab/><w:t>A{i}</w:t><w:tab/><w:t>金额</w:t><w:tab/><w:t>{}.00</w:t></w:r></w:p>"#,
                        1000 + i * 37
                    )
                })
                .collect::<String>(),
        ),
    );

    let cell = |w: u32, t: &str| {
        format!(
            r#"<w:tc><w:tcPr><w:tcW w:w="{w}" w:type="dxa"/></w:tcPr>{}</w:tc>"#,
            probe_para("", t)
        )
    };
    let rows = (0..25)
        .map(|i| {
            format!(
                "<w:tr>{}{}{}</w:tr>",
                cell(1500, &format!("附件{}", i + 1)),
                cell(3500, &filler(i, 18)),
                cell(3600, &filler(i + 3, 40))
            )
        })
        .collect::<String>();
    let border =
        |side: &str| format!(r#"<w:{side} w:val="single" w:sz="4" w:space="0" w:color="000000"/>"#);
    add(
        "table_basic",
        DocxBuilder::new().body(&format!(
            r#"{}<w:tbl><w:tblPr><w:tblW w:w="8600" w:type="dxa"/><w:tblBorders>{}{}{}{}{}{}</w:tblBorders></w:tblPr><w:tblGrid><w:gridCol w:w="1500"/><w:gridCol w:w="3500"/><w:gridCol w:w="3600"/></w:tblGrid>{rows}</w:tbl>{}"#,
            probe_para("", "附件清单"),
            border("top"),
            border("left"),
            border("bottom"),
            border("right"),
            border("insideH"),
            border("insideV"),
            probe_para("", "以上为全部附件。")
        )),
    );

    // 跨页：标题行重复、长行在页底拆开、cantSplit 的行整行挪走、左列纵向合并跨页。
    let xcell = |w: u32, pr: &str, xml: &str| {
        format!(r#"<w:tc><w:tcPr><w:tcW w:w="{w}" w:type="dxa"/>{pr}</w:tcPr>{xml}</w:tc>"#)
    };
    let row = |trpr: &str, a: &str, b: &str| {
        format!(
            "<w:tr><w:trPr>{trpr}</w:trPr>{}{}</w:tr>",
            xcell(1500, "", &probe_para("", a)),
            xcell(7100, "", b)
        )
    };
    let split_rows = row("<w:tblHeader/>", "序号", &probe_para("", "内容"))
        + &row("", "甲", &probe_para("", &filler(1, 30)))
        + &row("", "乙", &paras(40, "", 60))
        + &row("<w:cantSplit/>", "丙", &paras(12, "", 60))
        + &(0..30)
            .map(|i| {
                let (merge, text) = if i == 0 {
                    (r#"<w:vMerge w:val="restart"/>"#, "丁")
                } else {
                    ("<w:vMerge/>", "")
                };
                format!(
                    "<w:tr>{}{}</w:tr>",
                    xcell(1500, merge, &probe_para("", text)),
                    xcell(7100, "", &probe_para("", &filler(i, 20)))
                )
            })
            .collect::<String>();
    add(
        "table_split",
        DocxBuilder::new().body(&format!(
            r#"{}<w:tbl><w:tblPr><w:tblW w:w="8600" w:type="dxa"/><w:tblBorders>{}{}{}{}{}{}</w:tblBorders></w:tblPr><w:tblGrid><w:gridCol w:w="1500"/><w:gridCol w:w="7100"/></w:tblGrid>{split_rows}</w:tbl>{}"#,
            paras(10, "", 60),
            border("top"),
            border("left"),
            border("bottom"),
            border("right"),
            border("insideH"),
            border("insideV"),
            probe_para("", "表格之后的正文。")
        )),
    );

    // 嵌套表格、行首行尾空列（gridBefore、gridAfter）、没写列宽的表格。
    let tbl = |pr: &str, grid: &[u32], rows: &str| {
        let cols: String = grid
            .iter()
            .map(|w| format!(r#"<w:gridCol w:w="{w}"/>"#))
            .collect();
        format!(
            r#"<w:tbl><w:tblPr>{pr}<w:tblBorders>{}{}{}{}{}{}</w:tblBorders></w:tblPr><w:tblGrid>{cols}</w:tblGrid>{rows}</w:tbl>"#,
            border("top"),
            border("left"),
            border("bottom"),
            border("right"),
            border("insideH"),
            border("insideV")
        )
    };
    let tr = |cells: &[(u32, &str)]| {
        let tcs: String = cells.iter().map(|&(w, t)| xcell(w, "", t)).collect();
        format!("<w:tr>{tcs}</w:tr>")
    };
    // 内层表格要写 tblW：没写时 LibreOffice 把嵌套表格撑满整格（Word 按网格）。
    let inner = tbl(
        r#"<w:tblW w:w="3600" w:type="dxa"/>"#,
        &[1800, 1800],
        &(tr(&[
            (1800, &probe_para("", "内一")),
            (1800, &probe_para("", &filler(3, 12))),
        ]) + &tr(&[
            (1800, &probe_para("", "内二")),
            (1800, &probe_para("", &filler(5, 30))),
        ])),
    );
    let nested = tbl(
        "",
        &[2000, 6600],
        &(0..6)
            .map(|i| {
                let content = if i % 2 == 0 {
                    probe_para("", &filler(i, 40)) + &inner + &probe_para("", "嵌套之后")
                } else {
                    probe_para("", &filler(i, 80))
                };
                tr(&[
                    (2000, &probe_para("", &format!("第{i}行"))),
                    (6600, &content),
                ])
            })
            .collect::<String>(),
    );
    let ragged = tbl(
        "",
        &[2150, 2150, 2150, 2150],
        &(tr(&[
            (2150, &probe_para("", "甲")),
            (2150, &probe_para("", "乙")),
            (2150, &probe_para("", "丙")),
            (2150, &probe_para("", "丁")),
        ]) + &format!(
            r#"<w:tr><w:trPr><w:gridBefore w:val="1"/></w:trPr>{}{}{}</w:tr>"#,
            xcell(2150, "", &probe_para("", "乙二")),
            xcell(2150, "", &probe_para("", "丙二")),
            xcell(2150, "", &probe_para("", "丁二"))
        ) + &format!(
            r#"<w:tr><w:trPr><w:gridAfter w:val="2"/></w:trPr>{}{}</w:tr>"#,
            xcell(2150, "", &probe_para("", "甲三")),
            xcell(2150, "", &probe_para("", "乙三"))
        )),
    );
    let bare = format!(
        r#"<w:tbl><w:tblPr><w:tblBorders>{}{}{}{}{}{}</w:tblBorders></w:tblPr><w:tr><w:tc>{}</w:tc><w:tc>{}</w:tc><w:tc>{}</w:tc></w:tr></w:tbl>"#,
        border("top"),
        border("left"),
        border("bottom"),
        border("right"),
        border("insideH"),
        border("insideV"),
        probe_para("", &filler(7, 20)),
        probe_para("", &filler(8, 20)),
        probe_para("", &filler(9, 20))
    );
    add(
        "table_nested",
        DocxBuilder::new().body(
            &(probe_para("", "嵌套表格")
                + &nested
                + &probe_para("", "参差的行")
                + &ragged
                + &probe_para("", "没写列宽")
                + &bare
                + &probe_para("", "表格之后的正文。")),
        ),
    );

    // 表格样式：网格型（框线、段距、字号都来自样式）与带首行、隔行、末行格式的样式。
    // 各样式都自己写全段落格式（Word 内置样式就是这样）：LibreOffice 不沿 basedOn 取
    // 表格样式的段落格式。
    let all_borders: String = ["top", "left", "bottom", "right", "insideH", "insideV"]
        .iter()
        .map(|s| border(s))
        .collect();
    let cell_ppr = r#"<w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/></w:pPr><w:rPr><w:sz w:val="21"/></w:rPr>"#;
    let styles = format!(
        r#"<w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Liberation Serif" w:hAnsi="Liberation Serif" w:eastAsia="Noto Serif CJK SC"/><w:sz w:val="24"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after="160" w:line="240" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults>
<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>
<w:style w:type="table" w:default="1" w:styleId="TableNormal"><w:name w:val="Normal Table"/><w:tblPr><w:tblInd w:w="0" w:type="dxa"/><w:tblCellMar><w:top w:w="0" w:type="dxa"/><w:left w:w="108" w:type="dxa"/><w:bottom w:w="0" w:type="dxa"/><w:right w:w="108" w:type="dxa"/></w:tblCellMar></w:tblPr></w:style>
<w:style w:type="table" w:styleId="TableGrid"><w:name w:val="Table Grid"/><w:basedOn w:val="TableNormal"/>{cell_ppr}<w:tblPr><w:tblBorders>{all_borders}</w:tblBorders></w:tblPr></w:style>
<w:style w:type="table" w:styleId="Banded"><w:name w:val="Banded"/><w:basedOn w:val="TableGrid"/>{cell_ppr}<w:tblPr><w:tblStyleRowBandSize w:val="1"/></w:tblPr>
<w:tblStylePr w:type="firstRow"><w:rPr><w:b/><w:color w:val="FFFFFF"/></w:rPr><w:tcPr><w:shd w:val="clear" w:color="auto" w:fill="4472C4"/></w:tcPr></w:tblStylePr>
<w:tblStylePr w:type="band1Horz"><w:tcPr><w:shd w:val="clear" w:color="auto" w:fill="D9E2F3"/></w:tcPr></w:tblStylePr>
<w:tblStylePr w:type="lastRow"><w:rPr><w:b/></w:rPr><w:tcPr><w:tcBorders><w:top w:val="double" w:sz="4" w:space="0" w:color="000000"/></w:tcBorders></w:tcPr></w:tblStylePr></w:style>"#
    );
    let styled = |style: &str, look: &str, rows: &str| {
        format!(
            r#"<w:tbl><w:tblPr><w:tblStyle w:val="{style}"/><w:tblW w:w="8600" w:type="dxa"/>{look}</w:tblPr><w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="6600"/></w:tblGrid>{rows}</w:tbl>"#
        )
    };
    let plain = |t: &str| format!("<w:p><w:r><w:t>{t}</w:t></w:r></w:p>");
    let two = |i: usize, len: usize| {
        format!(
            "<w:tr>{}{}</w:tr>",
            xcell(2000, "", &plain(&format!("第{i}项"))),
            xcell(6600, "", &plain(&filler(i, len)))
        )
    };
    let body = plain("网格型表格")
        + &styled(
            "TableGrid",
            "",
            &(0..8).map(|i| two(i, 20 + i * 9)).collect::<String>(),
        )
        + &plain("带条件格式的表格")
        + &styled(
            "Banded",
            r#"<w:tblLook w:val="04E0" w:firstRow="1" w:lastRow="1" w:firstColumn="1" w:lastColumn="0" w:noHBand="0" w:noVBand="1"/>"#,
            &(0..10).map(|i| two(i, 16 + i * 5)).collect::<String>(),
        )
        + &plain("表格之后的正文。");
    add(
        "table_styles",
        DocxBuilder::new().styles(&styles).body(&body),
    );

    // 行内图片：单独一段的、夹在字里的、一点五倍行距的，还有一张高得要换页的。
    let mut builder = DocxBuilder::new();
    let png = |w: u32, h: u32| {
        let img = image::DynamicImage::ImageRgb8(super::images::photo(w, h));
        let mut bytes = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
        bytes
    };
    let wide = builder.media("wide.png", png(80, 40));
    let square = builder.media(
        "square.jpeg",
        super::images::jpeg_q(
            &image::DynamicImage::ImageRgb8(super::images::photo(40, 40)),
            90,
        ),
    );
    let pic = |rid: &str, w: u32, h: u32| {
        format!(
            r#"<w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="{}" cy="{}"/><wp:docPr id="1" name="p"/><a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:nvPicPr><pic:cNvPr id="1" name="p"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="{rid}"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="{}" cy="{}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>"#,
            w * 12700,
            h * 12700,
            w * 12700,
            h * 12700
        )
    };
    let run = |t: &str| {
        format!(
            r#"<w:r><w:rPr><w:rFonts w:ascii="Liberation Serif" w:hAnsi="Liberation Serif" w:eastAsia="Noto Serif CJK SC"/><w:sz w:val="24"/></w:rPr><w:t xml:space="preserve">{t}</w:t></w:r>"#
        )
    };
    let mut body = String::new();
    for i in 0..6 {
        body += &paras(3, "", 50 + i * 7);
        body += &format!("<w:p>{}</w:p>", pic(&wide, 160, 80));
        body += &format!(
            "<w:p>{}{}{}</w:p>",
            run(&filler(i, 12)),
            pic(&square, 24, 24),
            run(&filler(i + 2, 30))
        );
        body += &format!(
            r#"<w:p><w:pPr><w:spacing w:line="360" w:lineRule="auto"/><w:jc w:val="center"/></w:pPr>{}</w:p>"#,
            pic(&square, 60, 60)
        );
    }
    body += &format!("<w:p>{}</w:p>", pic(&wide, 400, 200));
    body += &paras(4, "", 60);
    add("images_inline", builder.body(&body));

    // 浮动图片：浮于文字上方、衬于文字下方（公章），按页面、版心、段落、行定位，
    // 上下型环绕把字挤到图下面，跨页时跟着锚点段落走。
    let mut builder = DocxBuilder::new();
    let seal = builder.media("seal.png", png(60, 60));
    let photo = builder.media(
        "photo.jpeg",
        super::images::jpeg_q(
            &image::DynamicImage::ImageRgb8(super::images::photo(80, 50)),
            90,
        ),
    );
    let floating = |rid: &str,
                    w: u32,
                    h: u32,
                    h_pos: &str,
                    v_pos: &str,
                    wrap: &str,
                    behind: bool| {
        format!(
            r#"<w:r><w:drawing><wp:anchor distT="0" distB="0" distL="0" distR="0" simplePos="0" relativeHeight="251659264" behindDoc="{}" locked="0" layoutInCell="1" allowOverlap="1"><wp:simplePos x="0" y="0"/>{h_pos}{v_pos}<wp:extent cx="{}" cy="{}"/><wp:effectExtent l="0" t="0" r="0" b="0"/>{wrap}<wp:docPr id="1" name="p"/><a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:nvPicPr><pic:cNvPr id="1" name="p"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="{rid}"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="{}" cy="{}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:anchor></w:drawing></w:r>"#,
            behind as u8,
            w * 12700,
            h * 12700,
            w * 12700,
            h * 12700
        )
    };
    let pos = |axis: &str, from: &str, inner: String| {
        format!(r#"<wp:position{axis} relativeFrom="{from}">{inner}</wp:position{axis}>"#)
    };
    let off = |pt: u32| format!("<wp:posOffset>{}</wp:posOffset>", pt * 12700);
    let align = |a: &str| format!("<wp:align>{a}</wp:align>");
    let mut body = String::new();
    for i in 0..4 {
        body += &paras(4, "", 55 + i * 9);
        body += &format!(
            "<w:p>{}{}</w:p>",
            run("盖章处"),
            floating(
                &seal,
                60,
                60,
                &pos("H", "margin", align("right")),
                &pos("V", "paragraph", off(0)),
                "<wp:wrapNone/>",
                i % 2 == 1
            )
        );
        body += &paras(2, "", 70);
        body += &format!(
            "<w:p>{}{}</w:p>",
            run("插图"),
            floating(
                &photo,
                160,
                100,
                &pos("H", "margin", align("center")),
                &pos("V", "paragraph", off(0)),
                "<wp:wrapTopAndBottom/>",
                false
            )
        );
        body += &format!(
            "<w:p>{}{}</w:p>",
            run(&filler(i, 20)),
            floating(
                &seal,
                30,
                30,
                &pos("H", "page", off(40 + i as u32 * 10)),
                &pos("V", "line", off(5)),
                "<wp:wrapNone/>",
                false
            )
        );
    }
    body += &format!(
        "<w:p>{}{}</w:p>",
        run("页面定位"),
        floating(
            &photo,
            80,
            50,
            &pos("H", "page", off(420)),
            &pos("V", "page", off(60)),
            "<wp:wrapNone/>",
            false
        )
    );
    add("images_floating", builder.body(&body));

    // 文本框与直线：框里的字（有行网格也不吸附、行尾标点悬挂）、竖直对齐、行内的框、
    // 公文红线那样的横线、VML 直线，跨页时跟着锚点段落走。
    let shape = |w: u32, h: u32, sp_pr: &str, inner: &str, anchor_v: &str| {
        let txbx = if inner.is_empty() {
            String::new()
        } else {
            format!("<wps:txbx><w:txbxContent>{inner}</w:txbxContent></wps:txbx>")
        };
        format!(
            r#"<a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:cNvSpPr/><wps:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="{}" cy="{}"/></a:xfrm>{sp_pr}</wps:spPr>{txbx}<wps:bodyPr rot="0" vert="horz" wrap="square" lIns="91440" tIns="45720" rIns="91440" bIns="45720" anchor="{anchor_v}"><a:noAutofit/></wps:bodyPr></wps:wsp></a:graphicData></a:graphic>"#,
            w * 12700,
            h * 12700
        )
    };
    let boxed = r#"<a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:solidFill><a:srgbClr val="FFF2CC"/></a:solidFill><a:ln w="12700"><a:solidFill><a:srgbClr val="C00000"/></a:solidFill></a:ln>"#;
    let red_line = r#"<a:prstGeom prst="line"><a:avLst/></a:prstGeom><a:ln w="19050"><a:solidFill><a:srgbClr val="FF0000"/></a:solidFill></a:ln>"#;
    let anchored = |place: &str, w: u32, h: u32, graphic: &str| {
        format!(
            r#"<w:r><mc:AlternateContent><mc:Choice Requires="wps"><w:drawing><wp:anchor distT="0" distB="0" distL="0" distR="0" simplePos="0" relativeHeight="251659264" behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1"><wp:simplePos x="0" y="0"/>{place}<wp:extent cx="{}" cy="{}"/><wp:effectExtent l="0" t="0" r="0" b="0"/><wp:wrapNone/><wp:docPr id="1" name="s"/>{graphic}</wp:anchor></w:drawing></mc:Choice><mc:Fallback/></mc:AlternateContent></w:r>"#,
            w * 12700,
            h * 12700
        )
    };
    let inline = |w: u32, h: u32, graphic: &str| {
        format!(
            r#"<w:r><mc:AlternateContent><mc:Choice Requires="wps"><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="{}" cy="{}"/><wp:effectExtent l="0" t="0" r="0" b="0"/><wp:docPr id="1" name="s"/>{graphic}</wp:inline></w:drawing></mc:Choice><mc:Fallback/></mc:AlternateContent></w:r>"#,
            w * 12700,
            h * 12700
        )
    };
    let in_box = |seed: usize, n: usize| format!("<w:p>{}</w:p>", run(&filler(seed, n)));
    let mut body = String::new();
    for i in 0..4 {
        body += &paras(3, "", 50 + i * 11);
        let align_v = ["t", "ctr", "b", "t"][i];
        body += &format!(
            "<w:p>{}{}</w:p>",
            run("说明"),
            anchored(
                &(pos("H", "margin", align("right")) + &pos("V", "paragraph", off(0))),
                150,
                110,
                &shape(
                    150,
                    110,
                    boxed,
                    &(in_box(i, 30) + &in_box(i + 1, 12)),
                    align_v
                )
            )
        );
        body += &format!(
            "<w:p>{}{}</w:p>",
            run(&filler(i + 3, 16)),
            anchored(
                &(pos("H", "column", off(0)) + &pos("V", "paragraph", off(26))),
                430,
                0,
                &shape(430, 0, red_line, "", "t")
            )
        );
        body += &paras(2, "", 40 + i * 5);
        body += &format!(
            "<w:p>{}{}{}</w:p>",
            run(&filler(i, 10)),
            inline(110, 44, &shape(110, 44, boxed, &in_box(i + 5, 9), "t")),
            run(&filler(i + 2, 24))
        );
    }
    // 放不下的框：只露出两行。
    body += &format!(
        "<w:p>{}{}</w:p>",
        run("溢出"),
        anchored(
            &(pos("H", "page", off(360)) + &pos("V", "page", off(90))),
            120,
            40,
            &shape(
                120,
                40,
                boxed,
                &(0..5).map(|k| in_box(k, 8)).collect::<String>(),
                "t"
            )
        )
    );
    body += r##"<w:p><w:r><w:pict><v:line style="position:absolute;z-index:251670000;mso-position-horizontal-relative:page;mso-position-vertical-relative:page" from="90pt,760pt" to="300pt,750pt" strokecolor="#0000ff" strokeweight="2pt"/></w:pict></w:r></w:p>"##;
    body += &paras(3, "", 60);
    add(
        "text_boxes",
        DocxBuilder::new().body(&body).sect_extra(GRID_312),
    );

    let numbering = r#"<w:abstractNum w:abstractNumId="0"><w:multiLevelType w:val="hybridMultilevel"/>
<w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="chineseCounting"/><w:lvlText w:val="%1、"/><w:lvlJc w:val="left"/><w:pPr><w:ind w:left="420" w:hanging="420"/></w:pPr></w:lvl>
<w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%2."/><w:lvlJc w:val="left"/><w:pPr><w:ind w:left="840" w:hanging="420"/></w:pPr></w:lvl>
</w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#;
    let numbered = (0..30)
        .map(|i| {
            let lvl = if i % 4 == 0 { 0 } else { 1 };
            probe_para(
                &format!(r#"<w:numPr><w:ilvl w:val="{lvl}"/><w:numId w:val="1"/></w:numPr>"#),
                &filler(i, 50),
            )
        })
        .collect::<String>();
    add(
        "numbering_basic",
        DocxBuilder::new().numbering(numbering).body(&numbered),
    );

    let page_field = r#"<w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:rPr><w:rFonts w:ascii="Liberation Serif" w:hAnsi="Liberation Serif"/><w:sz w:val="18"/></w:rPr><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:rPr><w:rFonts w:ascii="Liberation Serif" w:hAnsi="Liberation Serif"/><w:sz w:val="18"/></w:rPr><w:instrText xml:space="preserve"> PAGE </w:instrText></w:r><w:r><w:rPr><w:rFonts w:ascii="Liberation Serif" w:hAnsi="Liberation Serif"/><w:sz w:val="18"/></w:rPr><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:rPr><w:rFonts w:ascii="Liberation Serif" w:hAnsi="Liberation Serif"/><w:sz w:val="18"/></w:rPr><w:t>1</w:t></w:r><w:r><w:rPr><w:rFonts w:ascii="Liberation Serif" w:hAnsi="Liberation Serif"/><w:sz w:val="18"/></w:rPr><w:fldChar w:fldCharType="end"/></w:r></w:p>"#;
    add(
        "header_footer_page",
        DocxBuilder::new()
            .header(
                "default",
                &probe_para(r#"<w:jc w:val="right"/>"#, "示例文档页眉"),
            )
            .footer("default", page_field)
            .body(&paras(40, "", 90)),
    );

    let styles = r#"<w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Liberation Serif" w:hAnsi="Liberation Serif" w:eastAsia="Noto Serif CJK SC"/><w:sz w:val="24"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after="0" w:line="360" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults>
<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:pPr><w:ind w:firstLine="480" w:firstLineChars="200"/></w:pPr></w:style>
<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:pPr><w:keepNext/><w:spacing w:before="240" w:after="120"/><w:ind w:firstLine="0" w:firstLineChars="0"/><w:jc w:val="center"/></w:pPr><w:rPr><w:b/><w:sz w:val="32"/></w:rPr></w:style>"#;
    let styled = (0..12)
        .map(|i| {
            format!(
                r#"<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>第{}部分</w:t></w:r></w:p>"#,
                i + 1
            ) + &(0..3)
                .map(|k| format!(r#"<w:p><w:r><w:t>{}</w:t></w:r></w:p>"#, filler(i + k, 110)))
                .collect::<String>()
        })
        .collect::<String>();
    add(
        "styles_heading_keepnext",
        DocxBuilder::new().styles(styles).body(&styled),
    );

    // 段落边框与底纹：红头线、上下框、带缩进的四边框、合成一个框的相邻段落
    // （有无分隔线）、双线、粗线、底纹，再加一段跨页的带框段落。
    let side = |s: &str, val: &str, sz: u32, space: u32, color: &str| {
        format!(r#"<w:{s} w:val="{val}" w:sz="{sz}" w:space="{space}" w:color="{color}"/>"#)
    };
    let boxed = |sz: u32, space: u32, between: bool| {
        let mut s: String = ["top", "left", "bottom", "right"]
            .iter()
            .map(|s| side(s, "single", sz, space, "000000"))
            .collect();
        if between {
            s += &side("between", "single", sz, space, "000000");
        }
        format!("<w:pBdr>{s}</w:pBdr>")
    };
    let shd = r#"<w:shd w:val="clear" w:color="auto" w:fill="D9D9D9"/>"#;
    let indent = r#"<w:ind w:left="720" w:right="720"/>"#;
    let groups: Vec<Vec<String>> = vec![
        vec![probe_para(
            &format!(
                r#"<w:pBdr>{}</w:pBdr><w:jc w:val="center"/>"#,
                side("bottom", "single", 12, 1, "FF0000")
            ),
            &filler(1, 10),
        )],
        vec![probe_para(
            &format!(
                "<w:pBdr>{}{}</w:pBdr>",
                side("top", "single", 8, 4, "000000"),
                side("bottom", "single", 8, 4, "000000")
            ),
            &filler(2, 60),
        )],
        vec![probe_para(&(boxed(4, 4, false) + indent), &filler(3, 60))],
        (0..2)
            .map(|i| probe_para(&boxed(4, 1, false), &filler(4 + i, 30)))
            .collect(),
        (0..2)
            .map(|i| probe_para(&boxed(4, 1, true), &filler(6 + i, 30)))
            .collect(),
        vec![probe_para(
            &format!(
                "<w:pBdr>{}</w:pBdr>",
                side("bottom", "double", 6, 1, "000000")
            ),
            &filler(8, 30),
        )],
        vec![probe_para(&(shd.to_string() + indent), &filler(9, 60))],
        vec![probe_para(&(boxed(4, 4, false) + shd), &filler(10, 30))],
        vec![probe_para(
            &format!(
                "<w:pBdr>{}</w:pBdr>",
                side("bottom", "single", 24, 0, "000000")
            ),
            &filler(11, 30),
        )],
    ];
    let mut bordered: String = groups
        .iter()
        .enumerate()
        .map(|(i, g)| probe_para("", &filler(20 + i, 30)) + &g.concat())
        .collect();
    bordered += &(0..12)
        .map(|i| probe_para("", &filler(40 + i, 30)))
        .collect::<String>();
    bordered += &probe_para(&(boxed(4, 4, false) + shd), &filler(60, 240));
    add("paragraph_borders", DocxBuilder::new().body(&bordered));

    // 编号的语义：多级模板、isLgl、几个 num 共用计数器与 startOverride、三种后缀、
    // 右对齐与居中的编号、加粗的编号、编号来自样式时的缩进层叠、项目符号。
    // pPr 里的元素按 schema 顺序写：LibreOffice 按出现顺序处理，缩进写在编号前面
    // 会被编号盖掉，真实文档不会这样。
    let rpr = r#"<w:rPr><w:rFonts w:ascii="Liberation Serif" w:hAnsi="Liberation Serif" w:eastAsia="Noto Serif CJK SC"/><w:sz w:val="24"/></w:rPr>"#;
    let lvl = |i: u8, fmt: &str, text: &str, ind: (u32, u32), extra: &str, rpr: &str| {
        format!(
            r#"<w:lvl w:ilvl="{i}"><w:start w:val="1"/><w:numFmt w:val="{fmt}"/>{extra}<w:lvlText w:val="{text}"/><w:pPr><w:ind w:left="{}" w:hanging="{}"/></w:pPr>{rpr}</w:lvl>"#,
            ind.0, ind.1
        )
    };
    let bold = rpr.replace("<w:sz ", "<w:b/><w:sz ");
    let symbol = rpr.replace(
        r#"w:ascii="Liberation Serif" w:hAnsi="Liberation Serif""#,
        r#"w:ascii="Symbol" w:hAnsi="Symbol""#,
    );
    let abstracts = [
        lvl(0, "chineseCounting", "%1、", (420, 420), "", rpr)
            + &lvl(1, "decimal", "%1.%2", (840, 420), "", rpr)
            + &lvl(2, "lowerLetter", "(%3)", (1260, 420), "", rpr),
        lvl(0, "chineseCounting", "第%1章", (0, 0), "", rpr)
            + &lvl(1, "decimal", "%1.%2", (420, 420), "<w:isLgl/>", rpr),
        lvl(0, "decimal", "%1.", (420, 420), "", rpr),
        lvl(
            0,
            "upperRoman",
            "%1.",
            (0, 0),
            r#"<w:suff w:val="space"/>"#,
            rpr,
        ),
        lvl(
            0,
            "decimal",
            "%1.",
            (0, 0),
            r#"<w:suff w:val="nothing"/>"#,
            rpr,
        ),
        lvl(
            0,
            "decimal",
            "%1.",
            (720, 720),
            r#"<w:lvlJc w:val="right"/>"#,
            rpr,
        ),
        lvl(
            0,
            "decimal",
            "%1.",
            (720, 720),
            r#"<w:lvlJc w:val="center"/>"#,
            rpr,
        ),
        lvl(0, "decimal", "%1.", (420, 420), "", &bold),
        lvl(0, "decimal", "%1", (840, 840), "", rpr),
        lvl(0, "bullet", "\u{F0B7}", (420, 420), "", &symbol),
    ];
    let mut numbering: String = abstracts
        .iter()
        .enumerate()
        .map(|(i, a)| {
            format!(
                r#"<w:abstractNum w:abstractNumId="{i}"><w:multiLevelType w:val="multilevel"/>{a}</w:abstractNum>"#
            )
        })
        .collect();
    numbering += &(0..abstracts.len())
        .map(|i| {
            format!(
                r#"<w:num w:numId="{}"><w:abstractNumId w:val="{i}"/></w:num>"#,
                i + 1
            )
        })
        .collect::<String>();
    numbering += r#"<w:num w:numId="20"><w:abstractNumId w:val="2"/></w:num><w:num w:numId="21"><w:abstractNumId w:val="2"/><w:lvlOverride w:ilvl="0"><w:startOverride w:val="5"/></w:lvlOverride></w:num>"#;
    let styles = r#"<w:docDefaults><w:rPrDefault><w:rPr/></w:rPrDefault><w:pPrDefault/></w:docDefaults>
<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>
<w:style w:type="paragraph" w:styleId="H"><w:name w:val="H"/><w:pPr><w:numPr><w:numId w:val="9"/></w:numPr><w:ind w:left="0" w:firstLine="0"/></w:pPr></w:style>
<w:style w:type="paragraph" w:styleId="Base"><w:name w:val="Base"/><w:pPr><w:ind w:firstLine="480" w:firstLineChars="200"/></w:pPr></w:style>
<w:style w:type="paragraph" w:styleId="Hx"><w:name w:val="Hx"/><w:basedOn w:val="Base"/><w:pPr><w:numPr><w:numId w:val="9"/></w:numPr></w:pPr></w:style>"#;
    let item = |style: &str, num: Option<(u32, u8)>, ind: &str, seed: usize| {
        let style = if style.is_empty() {
            String::new()
        } else {
            format!(r#"<w:pStyle w:val="{style}"/>"#)
        };
        let num = num.map_or(String::new(), |(id, l)| {
            format!(r#"<w:numPr><w:ilvl w:val="{l}"/><w:numId w:val="{id}"/></w:numPr>"#)
        });
        probe_para(&format!("{style}{num}{ind}"), &filler(seed, 40))
    };
    let seq = [
        ("", Some((1, 0)), ""),
        ("", Some((1, 1)), ""),
        ("", Some((1, 1)), ""),
        ("", Some((1, 2)), ""),
        ("", Some((1, 0)), ""),
        ("", Some((1, 1)), ""),
        ("", Some((2, 0)), ""),
        ("", Some((2, 1)), ""),
        ("", Some((2, 1)), ""),
        ("", Some((3, 0)), ""),
        ("", Some((20, 0)), ""),
        ("", Some((21, 0)), ""),
        ("", Some((21, 0)), ""),
        ("", Some((3, 0)), ""),
        ("", Some((4, 0)), ""),
        ("", Some((5, 0)), ""),
        ("", Some((6, 0)), ""),
        ("", Some((7, 0)), ""),
        ("", Some((8, 0)), ""),
        ("H", None, ""),
        ("H", None, r#"<w:ind w:left="1260"/>"#),
        ("H", Some((0, 0)), ""),
        ("Hx", None, ""),
        ("Base", Some((9, 0)), ""),
        ("", Some((9, 0)), r#"<w:ind w:firstLine="200"/>"#),
        ("", Some((10, 0)), ""),
        ("", Some((10, 0)), ""),
    ];
    let numbered: String = seq
        .iter()
        .enumerate()
        .map(|(i, (st, num, ind))| item(st, *num, ind, i))
        .collect();
    add(
        "numbering_semantics",
        DocxBuilder::new()
            .numbering(&numbering)
            .styles(styles)
            .body(&numbered),
    );

    // 多节：纵向 → 横向（边距不同、带网格）→ 纵向，再接一个设置相同的连续分节。
    // 奇偶页起、连续分节改边距这两种 LibreOffice 与规范不同，不放进来：LibreOffice
    // 在连续分节之后连新的一页都不用新节的边距。
    let sect = |kind: &str, (w, h): (u32, u32), left: u32, grid: &str| {
        format!(
            r#"<w:sectPr><w:type w:val="{kind}"/><w:pgSz w:w="{w}" w:h="{h}"/><w:pgMar w:top="1440" w:right="1588" w:bottom="1440" w:left="{left}" w:header="851" w:footer="992" w:gutter="0"/>{grid}</w:sectPr>"#
        )
    };
    let section = |seed: usize, n: usize, sect: String| {
        (0..n)
            .map(|i| probe_para(if i + 1 == n { &sect } else { "" }, &filler(seed + i, 60)))
            .collect::<String>()
    };
    let sectioned = section(0, 30, sect("nextPage", (11906, 16838), 1588, ""))
        + &section(40, 30, sect("nextPage", (16838, 11906), 2268, GRID_312))
        + &section(80, 20, sect("nextPage", (11906, 16838), 1588, ""))
        + &section(120, 20, String::new());
    add(
        "sections",
        DocxBuilder::new()
            .body(&sectioned)
            .sect_extra(r#"<w:type w:val="continuous"/>"#),
    );

    // 页眉页脚的种类：首页、偶数页各自的页眉，偶数页的页眉很高（把正文往下推），
    // 页脚是「第 X 页 共 Y 页」。
    let run = |t: &str| {
        format!(
            r#"<w:r><w:rPr><w:rFonts w:ascii="Liberation Serif" w:hAnsi="Liberation Serif" w:eastAsia="Noto Serif CJK SC"/><w:sz w:val="21"/></w:rPr><w:t xml:space="preserve">{t}</w:t></w:r>"#
        )
    };
    let field = |code: &str| {
        let r = |inner: &str| format!("<w:r>{inner}</w:r>");
        [
            r(r#"<w:fldChar w:fldCharType="begin"/>"#),
            r(&format!(
                r#"<w:instrText xml:space="preserve"> {code} </w:instrText>"#
            )),
            r(r#"<w:fldChar w:fldCharType="separate"/>"#),
            run("1"),
            r(r#"<w:fldChar w:fldCharType="end"/>"#),
        ]
        .concat()
    };
    let page_footer = format!(
        r#"<w:p><w:pPr><w:jc w:val="center"/></w:pPr>{}{}{}{}{}</w:p>"#,
        run("第 "),
        field("PAGE"),
        run(" 页 共 "),
        field("NUMPAGES"),
        run(" 页")
    );
    let tall_header: String = (0..6)
        .map(|i| probe_para("", &format!("偶数页页眉第{}行", i + 1)))
        .collect();
    add(
        "header_footer_kinds",
        DocxBuilder::new()
            .body(&paras(90, "", 60))
            .settings(&format!(
                "<w:evenAndOddHeaders/>{}",
                super::docx::DEFAULT_SETTINGS
            ))
            .sect_extra("<w:titlePg/>")
            .header("first", &probe_para("", "首页页眉"))
            .header(
                "default",
                &probe_para(r#"<w:jc w:val="right"/>"#, "奇数页页眉"),
            )
            .header("even", &tall_header)
            .footer("default", &page_footer)
            .footer("even", &page_footer),
    );

    v
}
