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

    v
}
