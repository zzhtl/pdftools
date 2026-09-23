//! 校准测量：每项只考一条排版规则，从 PDF 里量出一个数，与 LibreOffice 对照。
//!
//! 探针（`probes.rs`）看整体 —— 页数、分页点、行位置；这里看单条规则的具体数值，
//! 比如「一个空段落多高」「段落标记的字号算不算进末行」。规则改没改对，一眼可见。
//!
//! 量高度一律用**差分**：同一份文档里放两组结构相同、只有被测对象不同的行，
//! 用两组基线间距之差消掉上下文字行自身的行框，剩下的就是被测对象。

use super::docx::DocxBuilder;
use super::pdftext::PageText;

/// 从一份 PDF 的文字坐标里量出被测的数。
pub type Extract = Box<dyn Fn(&[PageText]) -> Option<f32>>;

pub struct Measure {
    pub name: String,
    pub doc: DocxBuilder,
    pub unit: &'static str,
    pub value: Extract,
}

const GRID: &str = r#"<w:docGrid w:type="lines" w:linePitch="312"/>"#;
const FONTS: &str = r#"<w:rFonts w:ascii="Liberation Serif" w:hAnsi="Liberation Serif" w:eastAsia="Noto Serif CJK SC"/>"#;

/// 一行用来定位的文字。`tag` 要在整份文档里唯一。
fn marker(tag: &str) -> String {
    format!(
        r#"<w:p><w:r><w:rPr>{FONTS}<w:sz w:val="24"/></w:rPr><w:t>标记{tag}行</w:t></w:r></w:p>"#
    )
}

/// 含 `标记{tag}行` 的那一行：(页序号, 基线 y)。
pub fn baseline(pages: &[PageText], tag: &str) -> Option<(usize, f32)> {
    let needle = format!("标记{tag}行");
    pages.iter().enumerate().find_map(|(i, p)| {
        p.lines
            .iter()
            .find(|l| super::pdftext::norm(&l.text).contains(&needle))
            .map(|l| (i, l.y))
    })
}

/// 两个标记行的基线距离。不在同一页时没有意义，返回 None。
pub fn gap(pages: &[PageText], a: &str, b: &str) -> Option<f32> {
    let (pa, ya) = baseline(pages, a)?;
    let (pb, yb) = baseline(pages, b)?;
    (pa == pb).then_some(ya - yb)
}

/// 测量用的文档一律带 settings.xml：真实文档都有，而 LibreOffice 在缺了它时
/// 按另一套默认规则排段落间距（同一组正文 → 小标题差出 3pt）。
fn doc(body: String, grid: bool) -> DocxBuilder {
    let d = DocxBuilder::new().body(&body).settings(COMPAT_15);
    if grid {
        d.sect_extra(GRID)
    } else {
        d
    }
}

/// 在「甲 [被测 × k] 乙」与「丙 丁」两组之间差分：被测对象平均每个多高。
fn per_item_height(name: String, grid: bool, item: &str, k: usize) -> Measure {
    let body = marker("甲") + &item.repeat(k) + &marker("乙") + &marker("丙") + &marker("丁");
    Measure {
        name,
        doc: doc(body, grid),
        unit: "pt",
        value: Box::new(move |p| Some((gap(p, "甲", "乙")? - gap(p, "丙", "丁")?) / k as f32)),
    }
}

/// 空段落的 XML。`mark` 是段落标记的 rPr 内容，`ppr` 是其余段落属性。
fn empty_para(ppr: &str, mark: &str) -> String {
    format!(r#"<w:p><w:pPr>{ppr}<w:rPr>{mark}</w:rPr></w:pPr></w:p>"#)
}

/// P1：一个空段落占多高。
pub fn empty_paragraphs() -> Vec<Measure> {
    let mut v = Vec::new();
    for grid in [true, false] {
        let g = if grid { "网格" } else { "无网格" };
        let mut add = |label: &str, item: String| {
            v.push(per_item_height(
                format!("P1 空段落 {label} {g}"),
                grid,
                &item,
                5,
            ));
        };
        add("<w:p/>", "<w:p/>".into());
        add("无字号", empty_para("", FONTS));
        add(
            "12pt",
            empty_para("", &format!("{FONTS}<w:sz w:val=\"24\"/>")),
        );
        add(
            "16pt",
            empty_para("", &format!("{FONTS}<w:sz w:val=\"32\"/>")),
        );
        add(
            "12pt 只写中文字体",
            empty_para(
                "",
                r#"<w:rFonts w:eastAsia="Noto Serif CJK SC"/><w:sz w:val="24"/>"#,
            ),
        );
        add(
            "12pt hint=eastAsia",
            empty_para(
                "",
                r#"<w:rFonts w:ascii="Liberation Serif" w:hAnsi="Liberation Serif" w:eastAsia="Noto Serif CJK SC" w:hint="eastAsia"/><w:sz w:val="24"/>"#,
            ),
        );
        add(
            "12pt 1.5 倍",
            empty_para(
                r#"<w:spacing w:line="360" w:lineRule="auto"/>"#,
                &format!("{FONTS}<w:sz w:val=\"24\"/>"),
            ),
        );
        add(
            "12pt 固定 20pt",
            empty_para(
                r#"<w:spacing w:line="400" w:lineRule="exact"/>"#,
                &format!("{FONTS}<w:sz w:val=\"24\"/>"),
            ),
        );
        add(
            "12pt 至少 30pt",
            empty_para(
                r#"<w:spacing w:line="600" w:lineRule="atLeast"/>"#,
                &format!("{FONTS}<w:sz w:val=\"24\"/>"),
            ),
        );
    }
    v
}

/// 含 `标记{tag}行` 的片段的字号。
fn size_of(pages: &[PageText], tag: &str) -> Option<f32> {
    let needle = format!("标记{tag}行");
    pages
        .iter()
        .flat_map(|p| &p.lines)
        .find(|l| super::pdftext::norm(&l.text).contains(&needle))
        .and_then(|l| l.frags.first())
        .map(|f| f.size)
}

/// 哪儿都没写 `w:sz` 时的字号。
pub fn default_size() -> Vec<Measure> {
    let text = format!(r#"<w:p><w:r><w:rPr>{FONTS}</w:rPr><w:t>标记甲行</w:t></w:r></w:p>"#);
    let styles = format!(
        r#"<w:docDefaults><w:rPrDefault><w:rPr>{FONTS}</w:rPr></w:rPrDefault></w:docDefaults>
<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>"#
    );
    vec![
        Measure {
            name: "默认字号 没有 styles.xml".into(),
            doc: DocxBuilder::new().body(&text).settings(COMPAT_15),
            unit: "pt",
            value: Box::new(|p| size_of(p, "甲")),
        },
        Measure {
            name: "默认字号 docDefaults 不写 sz".into(),
            doc: DocxBuilder::new()
                .body(&text)
                .styles(&styles)
                .settings(COMPAT_15),
            unit: "pt",
            value: Box::new(|p| size_of(p, "甲")),
        },
    ]
}

/// 一段单行文字：run 的字号 `run_sz`，段落标记的字号 `mark_sz`（半磅）。
fn one_line(tag: &str, ppr: &str, run_sz: u32, mark_sz: u32) -> String {
    format!(
        r#"<w:p><w:pPr>{ppr}<w:rPr>{FONTS}<w:sz w:val="{mark_sz}"/></w:rPr></w:pPr><w:r><w:rPr>{FONTS}<w:sz w:val="{run_sz}"/></w:rPr><w:t>标记{tag}行</w:t></w:r></w:p>"#
    )
}

/// 「甲 X 乙」与「丙 Y 丁」：X 是被测行，Y 是只有被测属性不同的对照行。
/// 返回 (X 比 Y 在基线之上多出的高度, 在基线之下多出的高度)。
fn line_delta(pages: &[PageText]) -> Option<(f32, f32)> {
    let above = gap(pages, "甲", "被测")? - gap(pages, "丙", "对照")?;
    let below = gap(pages, "被测", "乙")? - gap(pages, "对照", "丁")?;
    Some((above, below))
}

fn delta_measures(label: &str, grid: bool, x: String, y: String) -> Vec<Measure> {
    let body = marker("甲") + &x + &marker("乙") + &marker("丙") + &y + &marker("丁");
    let d = doc(body, grid);
    vec![
        Measure {
            name: format!("{label} 基线上方"),
            doc: d.clone(),
            unit: "pt",
            value: Box::new(|p| line_delta(p).map(|(a, _)| a)),
        },
        Measure {
            name: format!("{label} 基线下方"),
            doc: d,
            unit: "pt",
            value: Box::new(|p| line_delta(p).map(|(_, b)| b)),
        },
    ]
}

/// P2：段落标记的字号比正文大时，末行有没有被撑高。
pub fn paragraph_mark_in_last_line() -> Vec<Measure> {
    let mut v = Vec::new();
    for grid in [false, true] {
        let g = if grid { "网格" } else { "无网格" };
        v.extend(delta_measures(
            &format!("P2 标记 24pt 正文 10.5pt {g}"),
            grid,
            one_line("被测", "", 21, 48),
            one_line("对照", "", 21, 21),
        ));
    }
    v
}

/// P3：固定行距、最小行距时，基线在行框里的位置（与单倍行距的同一行相比）。
pub fn exact_and_at_least() -> Vec<Measure> {
    let mut v = Vec::new();
    for grid in [false, true] {
        let g = if grid { "网格" } else { "无网格" };
        for (label, ppr) in [
            (
                "固定 30pt",
                r#"<w:spacing w:line="600" w:lineRule="exact"/>"#,
            ),
            (
                "固定 10pt",
                r#"<w:spacing w:line="200" w:lineRule="exact"/>"#,
            ),
            (
                "至少 30pt",
                r#"<w:spacing w:line="600" w:lineRule="atLeast"/>"#,
            ),
            (
                "至少 10pt",
                r#"<w:spacing w:line="200" w:lineRule="atLeast"/>"#,
            ),
        ] {
            v.extend(delta_measures(
                &format!("P3 {label} {g}"),
                grid,
                one_line("被测", ppr, 24, 24),
                one_line("对照", "", 24, 24),
            ));
        }
    }
    v
}

/// P6：行尾空格算不算行宽。右对齐时，算的话文字右缘离右边距一个空格宽，
/// 不算（悬挂在边距外）的话正好贴着右边距。量第一行右缘到右边距的距离。
pub fn trailing_space() -> Vec<Measure> {
    let words = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau upsilon phi chi psi omega ".repeat(3);
    let body = format!(
        r#"<w:p><w:pPr><w:jc w:val="right"/></w:pPr><w:r><w:rPr>{FONTS}<w:sz w:val="24"/></w:rPr><w:t xml:space="preserve">{words}</w:t></w:r></w:p>"#
    );
    vec![Measure {
        name: "P6 右对齐 首行右缘距右边距".into(),
        doc: DocxBuilder::new().body(&body).settings(COMPAT_15),
        unit: "pt",
        value: Box::new(|p| {
            let page = p.first()?;
            let first = page.lines.first()?;
            let right = first
                .frags
                .iter()
                .map(|f| f.x + f.width)
                .fold(f32::MIN, f32::max);
            // 页面右边距：DocxBuilder 默认 1588 twips。
            Some(page.width - 1588.0 / 20.0 - right)
        }),
    }]
}

/// 带段落间距的标记行。
fn spaced_marker(tag: &str, after: u32, before: u32) -> String {
    format!(
        r#"<w:p><w:pPr><w:spacing w:before="{before}" w:after="{after}"/></w:pPr><w:r><w:rPr>{FONTS}<w:sz w:val="24"/></w:rPr><w:t>标记{tag}行</w:t></w:r></w:p>"#
    )
}

/// P4：上一段的段后距与下一段的段前距，实际隔开多少（twips 写入，量出来是点）。
/// 行距倍数下的情形见 [`spacing_with_multiple`]。
pub fn paragraph_spacing() -> Vec<Measure> {
    let mut v = Vec::new();
    for grid in [false, true] {
        let g = if grid { "网格" } else { "无网格" };
        for (after, before) in [
            (60, 0),
            (0, 160),
            (80, 160),
            (360, 0),
            (360, 240),
            (240, 240),
            (100, 100),
        ] {
            let body = spaced_marker("甲", after, 0)
                + &spaced_marker("乙", 0, before)
                + &marker("丙")
                + &marker("丁");
            v.push(Measure {
                name: format!(
                    "P4 段后 {} 段前 {} {g}",
                    after as f32 / 20.0,
                    before as f32 / 20.0
                ),
                doc: doc(body, grid),
                unit: "pt",
                value: Box::new(|p| Some(gap(p, "甲", "乙")? - gap(p, "丙", "丁")?)),
            });
        }
    }
    v
}

/// P4b：1.3 倍行距下，段后距与段前距怎么合并。量的是两行基线距离比没有段落间距时多出的部分。
pub fn spacing_with_multiple() -> Vec<Measure> {
    let mut v = Vec::new();
    for grid in [false, true] {
        let g = if grid { "网格" } else { "无网格" };
        for (after, before) in [
            (60, 240),
            (240, 60),
            (80, 160),
            (120, 0),
            (0, 240),
            (240, 240),
        ] {
            let body = line130("甲", 24, false, 0, after)
                + &line130("乙", 24, false, before, 0)
                + &line130("丙", 24, false, 0, 0)
                + &line130("丁", 24, false, 0, 0);
            v.push(Measure {
                name: format!(
                    "P4b 1.3 倍 段后 {} 段前 {} {g}",
                    after as f32 / 20.0,
                    before as f32 / 20.0
                ),
                doc: doc(body, grid),
                unit: "pt",
                value: Box::new(|p| Some(gap(p, "甲", "乙")? - gap(p, "丙", "丁")?)),
            });
        }
    }
    v
}

/// 1.3 倍行距下的一段单行文字（中文 12pt，或按参数加粗、改字号、加段落间距）。
fn line130(tag: &str, sz: u32, bold: bool, before: u32, after: u32) -> String {
    let b = if bold { "<w:b/>" } else { "" };
    format!(
        r#"<w:p><w:pPr><w:spacing w:before="{before}" w:after="{after}" w:line="312" w:lineRule="auto"/><w:rPr>{FONTS}{b}<w:sz w:val="{sz}"/></w:rPr></w:pPr><w:r><w:rPr>{FONTS}{b}<w:sz w:val="{sz}"/></w:rPr><w:t>标记{tag}行</w:t></w:r></w:p>"#
    )
}

/// P5：1.3 倍行距、行网格下，两行基线之间的实际距离（参照语料的常见写法）。
pub fn multiple_spacing_transitions() -> Vec<Measure> {
    let cases: [(&str, String); 6] = [
        (
            "常规→常规",
            line130("甲", 24, false, 0, 0) + &line130("乙", 24, false, 0, 0),
        ),
        (
            "常规→加粗",
            line130("甲", 24, false, 0, 0) + &line130("乙", 24, true, 0, 0),
        ),
        (
            "加粗→常规",
            line130("甲", 24, true, 0, 0) + &line130("乙", 24, false, 0, 0),
        ),
        (
            "常规 段后4→加粗 段前8",
            line130("甲", 24, false, 0, 80) + &line130("乙", 24, true, 160, 80),
        ),
        (
            "16pt→12pt",
            line130("甲", 32, false, 0, 0) + &line130("乙", 24, false, 0, 0),
        ),
        (
            "16pt 加粗→12pt",
            line130("甲", 32, true, 0, 0) + &line130("乙", 24, false, 0, 0),
        ),
    ];
    cases
        .into_iter()
        .map(|(label, body)| Measure {
            name: format!("P5 1.3 倍 网格 {label}"),
            doc: doc(body, true),
            unit: "pt",
            value: Box::new(|p| gap(p, "甲", "乙")),
        })
        .collect()
}

/// 指定中文字体的 1.3 倍行距单行段落。
fn line130_font(tag: &str, font: &str, sz: u32, bold: bool, before: u32, after: u32) -> String {
    let b = if bold { "<w:b/>" } else { "" };
    let f = format!(
        r#"<w:rFonts w:ascii="Liberation Serif" w:hAnsi="Liberation Serif" w:eastAsia="{font}"/>"#
    );
    format!(
        r#"<w:p><w:pPr><w:spacing w:before="{before}" w:after="{after}" w:line="312" w:lineRule="auto"/><w:rPr>{f}{b}<w:sz w:val="{sz}"/></w:rPr></w:pPr><w:r><w:rPr>{f}{b}<w:sz w:val="{sz}"/></w:rPr><w:t>标记{tag}行</w:t></w:r></w:p>"#
    )
}

/// P5b：正文（段后 3pt）接 14pt 小标题（段前 12pt）：两行基线的距离。
pub fn heading_transitions() -> Vec<Measure> {
    let mut v = Vec::new();
    for (label, body_font, head_font, bold) in [
        (
            "衬线正文→衬线加粗",
            "Noto Serif CJK SC",
            "Noto Serif CJK SC",
            true,
        ),
        (
            "衬线正文→黑体加粗",
            "Noto Serif CJK SC",
            "Noto Sans CJK SC",
            true,
        ),
        (
            "黑体正文→黑体加粗",
            "Noto Sans CJK SC",
            "Noto Sans CJK SC",
            true,
        ),
        (
            "黑体正文→黑体常规",
            "Noto Sans CJK SC",
            "Noto Sans CJK SC",
            false,
        ),
        (
            "衬线正文→衬线常规",
            "Noto Serif CJK SC",
            "Noto Serif CJK SC",
            false,
        ),
    ] {
        let body = line130_font("甲", body_font, 24, false, 0, 60)
            + &line130_font("乙", head_font, 28, bold, 240, 120);
        v.push(Measure {
            name: format!("P5b 网格 {label}"),
            doc: doc(body, true),
            unit: "pt",
            value: Box::new(|p| gap(p, "甲", "乙")),
        });
    }
    v
}

const COMPAT_14: &str = r#"<w:compat><w:compatSetting w:name="compatibilityMode" w:uri="http://schemas.microsoft.com/office/word" w:val="14"/></w:compat>"#;
const COMPAT_15: &str = r#"<w:compat><w:compatSetting w:name="compatibilityMode" w:uri="http://schemas.microsoft.com/office/word" w:val="15"/></w:compat>"#;
const COMPAT_WPS: &str = r#"<w:compat><w:spaceForUL/><w:balanceSingleByteDoubleByteWidth/><w:doNotLeaveBackslashAlone/><w:ulTrailSpace/><w:doNotExpandShiftReturn/><w:adjustLineHeightInTable/><w:doNotWrapTextWithPunct/><w:doNotUseEastAsianBreakRules/><w:useFELayout/><w:compatSetting w:name="compatibilityMode" w:uri="http://schemas.microsoft.com/office/word" w:val="14"/></w:compat>"#;

const COMPAT_NO_HTML: &str = r#"<w:compat><w:doNotUseHTMLParagraphAutoSpacing/><w:compatSetting w:name="compatibilityMode" w:uri="http://schemas.microsoft.com/office/word" w:val="15"/></w:compat>"#;

/// P5c：同一组「正文 → 小标题」在不同兼容设置下。
pub fn heading_by_compat() -> Vec<Measure> {
    let body = line130_font("甲", "Noto Sans CJK SC", 24, false, 0, 60)
        + &line130_font("乙", "Noto Sans CJK SC", 28, true, 240, 120);
    [
        ("compatibilityMode 14", COMPAT_14),
        ("compatibilityMode 15", COMPAT_15),
        ("参照语料的兼容选项", COMPAT_WPS),
        ("doNotUseHTMLParagraphAutoSpacing", COMPAT_NO_HTML),
        ("空的 settings", ""),
    ]
    .into_iter()
    .map(|(label, settings)| Measure {
        name: format!("P5c 网格 正文→小标题 {label}"),
        doc: doc(body.clone(), true).settings(settings),
        unit: "pt",
        value: Box::new(|p| gap(p, "甲", "乙")),
    })
    .collect()
}

/// P7：大字号的行（标题）比 12pt 的行在基线上方、下方各多占多少。
pub fn large_text_lines() -> Vec<Measure> {
    let mut v = Vec::new();
    for grid in [true, false] {
        let g = if grid { "网格" } else { "无网格" };
        for (spacing, ppr) in [
            ("单倍", ""),
            ("1.3 倍", r#"<w:spacing w:line="312" w:lineRule="auto"/>"#),
        ] {
            for sz in [32u32, 40, 48] {
                v.extend(delta_measures(
                    &format!("P7 {}pt {spacing} {g}", sz / 2),
                    grid,
                    one_line("被测", ppr, sz, sz),
                    one_line("对照", ppr, 24, 24),
                ));
            }
        }
    }
    v
}

/// P8：正文第一行的基线离正文顶多远（上边距 1440 twips = 72pt）。
pub fn first_baseline() -> Vec<Measure> {
    let mut v = Vec::new();
    for grid in [true, false] {
        let g = if grid { "网格" } else { "无网格" };
        for sz in [21u32, 24, 28, 32, 40, 48, 64] {
            let body = one_line("甲", "", sz, sz) + &marker("乙");
            v.push(Measure {
                name: format!("P8 {}pt 首行基线 {g}", sz as f32 / 2.0),
                doc: doc(body, grid),
                unit: "pt",
                value: Box::new(|p| {
                    let (page, y) = baseline(p, "甲")?;
                    (page == 0).then(|| p[0].height - 72.0 - y)
                }),
            });
        }
    }
    v
}

/// P9：行网格在版心里的位置 —— 第二页首行、不吸附网格的段落首行，基线离正文顶多远。
pub fn grid_origin() -> Vec<Measure> {
    let filler = |i: usize| one_line(&format!("填{i}"), "", 24, 24);
    // 22 行 12pt 正好占满一页的网格（每行 2 格）；第 23 行落到第二页的第一行。
    let two_pages: String = (0..22).map(filler).collect::<String>() + &marker("甲");
    let no_snap = one_line("甲", r#"<w:snapToGrid w:val="0"/>"#, 24, 24) + &marker("乙");
    let first_on_page = |p: &[PageText], page: usize| {
        let (pg, y) = baseline(p, "甲")?;
        (pg == page).then(|| p[pg].height - 72.0 - y)
    };
    // 23pt 固定行距、不吸附网格：网格区（686.4pt）放 29 行，版心去掉顶部偏移（692.1pt）放 30 行。
    let exact23: String = (0..40)
        .map(|i| {
            one_line(
                &format!("固{i}"),
                r#"<w:snapToGrid w:val="0"/><w:spacing w:line="460" w:lineRule="exact"/>"#,
                24,
                24,
            )
        })
        .collect();
    vec![
        Measure {
            name: "P9 固定 23pt 不吸附 首页行数 网格".into(),
            doc: doc(exact23, true),
            unit: "行",
            value: Box::new(|p| Some(p.first()?.lines.len() as f32)),
        },
        Measure {
            name: "P9 第二页首行基线 网格".into(),
            doc: doc(two_pages, true),
            unit: "pt",
            value: Box::new(move |p| first_on_page(p, 1)),
        },
        Measure {
            name: "P9 不吸附网格的首行基线 网格".into(),
            doc: doc(no_snap, true),
            unit: "pt",
            value: Box::new(move |p| first_on_page(p, 0)),
        },
    ]
}

/// P10：页底最后一行 —— 行距倍数多出来的空白算不算进「放得下」。
/// 量第一页放了几行：一段很长的中文，每行都是整行。
pub fn page_bottom() -> Vec<Measure> {
    let text: String = "行距倍数的空白能否越过页底".repeat(120);
    [
        ("1.5 倍 无网格", false, 360),
        ("1.3 倍 网格", true, 312),
        ("单倍 网格", true, 240),
    ]
    .into_iter()
    .map(|(label, grid, line)| {
        let body = format!(
            r#"<w:p><w:pPr><w:spacing w:line="{line}" w:lineRule="auto"/></w:pPr><w:r><w:rPr>{FONTS}<w:sz w:val="24"/></w:rPr><w:t>{text}</w:t></w:r></w:p>"#
        );
        Measure {
            name: format!("P10 {label} 首页行数"),
            doc: doc(body, grid),
            unit: "行",
            value: Box::new(|p| Some(p.first()?.lines.len() as f32)),
        }
    })
    .collect()
}

/// P11：行尾标点能不能悬挂在右边距外。36 个 12pt 汉字正好排满一行（436.5pt 宽），
/// 后面接一个标点：能悬挂的话首行 37 个字（标点在边距外），不能的话标点不许出现在行首，
/// 要带着前一个字换行，首行只剩 35 个。量首行的字数。
pub fn hanging_punctuation() -> Vec<Measure> {
    let mut v = Vec::new();
    for (jc, jc_label) in [("left", "左对齐"), ("both", "两端对齐")] {
        for p in [
            '，', '。', '、', '；', '：', '！', '？', '」', '）', '”', ',', '.',
        ] {
            let text = format!("{}{p}{}", "测".repeat(36), "测".repeat(10));
            let body = format!(
                r#"<w:p><w:pPr><w:jc w:val="{jc}"/></w:pPr><w:r><w:rPr>{FONTS}<w:sz w:val="24"/></w:rPr><w:t>{text}</w:t></w:r></w:p>"#
            );
            v.push(Measure {
                name: format!("P11 {jc_label} 行尾「{p}」 首行字数"),
                doc: doc(body, false),
                unit: "字",
                value: Box::new(|p| Some(p.first()?.lines.first()?.text.chars().count() as f32)),
            });
        }
    }
    v
}

/// P11b：两端对齐、行尾标点悬挂时，首行最后一个字与那个标点各自的右缘离右边距多远
/// （正数在边距以内）。以及半角标点里还有哪些能悬挂。
pub fn hanging_positions() -> Vec<Measure> {
    let mut v = Vec::new();
    let text = format!("{}，{}", "测".repeat(36), "测".repeat(10));
    let body = format!(
        r#"<w:p><w:pPr><w:jc w:val="both"/></w:pPr><w:r><w:rPr>{FONTS}<w:sz w:val="24"/></w:rPr><w:t>{text}</w:t></w:r></w:p>"#
    );
    // 右边距 1588 twips；首行的字形按 x 排好，逐个看右缘。
    let edges = |p: &[PageText]| -> Option<(f32, f32)> {
        let page = p.first()?;
        let line = page.lines.first()?;
        let margin = page.width - 1588.0 / 20.0;
        let last = line.frags.last()?;
        // 片段可能把多个字合在一起；用片段右缘近似最后一个字形的右缘。
        let punct_right = last.x + last.width;
        let n = last.text.chars().count() as f32;
        let char_w = last.width / n.max(1.0);
        Some((margin - (punct_right - char_w), margin - punct_right))
    };
    v.push(Measure {
        name: "P11b 两端对齐 悬挂时首行最后一个汉字右缘".into(),
        doc: doc(body.clone(), false),
        unit: "pt",
        value: Box::new(move |p| edges(p).map(|(a, _)| a)),
    });
    v.push(Measure {
        name: "P11b 两端对齐 悬挂标点右缘".into(),
        doc: doc(body, false),
        unit: "pt",
        value: Box::new(move |p| edges(p).map(|(_, b)| b)),
    });
    for p in [';', ':', '!', '?', '．', '·'] {
        let text = format!("{}{p}{}", "测".repeat(36), "测".repeat(10));
        let body = format!(
            r#"<w:p><w:r><w:rPr>{FONTS}<w:sz w:val="24"/></w:rPr><w:t>{text}</w:t></w:r></w:p>"#
        );
        v.push(Measure {
            name: format!("P11c 行尾「{p}」 首行字数"),
            doc: doc(body, false),
            unit: "字",
            value: Box::new(|p| Some(p.first()?.lines.first()?.text.chars().count() as f32)),
        });
    }
    v
}

/// P12：标点挤压（`w:characterSpacingControl`）。一行里标点很多时，一行放得下几个字。
pub fn punctuation_compression() -> Vec<Measure> {
    let unit = "测试，内容。标点、";
    let text = unit.repeat(20);
    let body = format!(
        r#"<w:p><w:r><w:rPr>{FONTS}<w:sz w:val="24"/></w:rPr><w:t>{text}</w:t></w:r></w:p>"#
    );
    [
        (
            "compressPunctuation",
            "<w:characterSpacingControl w:val=\"compressPunctuation\"/>",
        ),
        (
            "doNotCompress",
            "<w:characterSpacingControl w:val=\"doNotCompress\"/>",
        ),
        ("不写", ""),
    ]
    .into_iter()
    .map(|(label, setting)| Measure {
        name: format!("P12 标点挤压 {label} 首行字数"),
        doc: DocxBuilder::new()
            .body(&body)
            .settings(&format!("{setting}{COMPAT_15}")),
        unit: "字",
        value: Box::new(|p| Some(p.first()?.lines.first()?.text.chars().count() as f32)),
    })
    .collect()
}

/// 以 `first` 开头的那一行里每个字形的 (原文, x)。
fn line_glyphs(pages: &[PageText], first: &str) -> Option<Vec<(String, f32)>> {
    pages
        .iter()
        .flat_map(|p| &p.lines)
        .find(|l| l.text.starts_with(first))
        .map(|l| {
            l.frags
                .iter()
                .flat_map(|f| f.glyphs.iter().cloned())
                .collect()
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GapKind {
    Cjk,
    LatinInner,
    AfterSpace,
    BeforeSpace,
    CjkLatin,
}

fn gap_kind(a: &str, b: &str) -> Option<GapKind> {
    let cjk = |s: &str| {
        s.chars().next().is_some_and(|c| {
            ('\u{3000}'..='\u{9fff}').contains(&c) || ('\u{ff00}'..='\u{ffef}').contains(&c)
        })
    };
    let latin = |s: &str| s.chars().next().is_some_and(|c| c.is_ascii_alphanumeric());
    Some(match (a, b) {
        (" ", _) => GapKind::AfterSpace,
        (_, " ") => GapKind::BeforeSpace,
        _ if cjk(a) && cjk(b) => GapKind::Cjk,
        _ if latin(a) && latin(b) => GapKind::LatinInner,
        _ if (cjk(a) && latin(b)) || (latin(a) && cjk(b)) => GapKind::CjkLatin,
        _ => return None,
    })
}

/// 两端对齐比左对齐在每个字形之后多加了多少（按间隙种类取平均）。
/// 文档里同一段文字排两遍：以「甲」开头的左对齐、以「乙」开头的两端对齐。
fn justify_extra(pages: &[PageText], kind: GapKind) -> Option<f32> {
    let left = line_glyphs(pages, "甲")?;
    let just = line_glyphs(pages, "乙")?;
    let n = left.len().min(just.len());
    let extra: Vec<f32> = (0..n).map(|i| just[i].1 - left[i].1).collect();
    let deltas: Vec<f32> = (0..n.saturating_sub(1))
        .filter(|&i| gap_kind(&left[i].0, &left[i + 1].0) == Some(kind))
        .map(|i| extra[i + 1] - extra[i])
        .collect();
    (!deltas.is_empty()).then(|| deltas.iter().sum::<f32>() / deltas.len() as f32)
}

/// P13：两端对齐把一行剩下的空间分到哪里。
pub fn justification() -> Vec<Measure> {
    let variants: [(&str, String, &[GapKind]); 4] = [
        (
            "纯中文",
            "测试两端对齐的分配规则".repeat(12),
            &[GapKind::Cjk],
        ),
        (
            "中文夹西文词",
            "测试两端对齐Word分配规则ABC".repeat(10),
            &[GapKind::Cjk, GapKind::LatinInner, GapKind::CjkLatin],
        ),
        (
            "中西混排带空格",
            "测试两端对齐 word 与 space 的分配 ".repeat(10),
            &[
                GapKind::Cjk,
                GapKind::LatinInner,
                GapKind::AfterSpace,
                GapKind::BeforeSpace,
            ],
        ),
        (
            "纯西文",
            "justification spreads extra space between words ".repeat(8),
            &[
                GapKind::LatinInner,
                GapKind::AfterSpace,
                GapKind::BeforeSpace,
            ],
        ),
    ];
    let mut v = Vec::new();
    for (label, text, kinds) in variants {
        let para = |first: &str, jc: &str| {
            format!(
                r#"<w:p><w:pPr><w:jc w:val="{jc}"/></w:pPr><w:r><w:rPr>{FONTS}<w:sz w:val="24"/></w:rPr><w:t xml:space="preserve">{first}{text}</w:t></w:r></w:p>"#
            )
        };
        let body = para("甲", "left") + &para("乙", "both");
        for &kind in kinds {
            v.push(Measure {
                name: format!("P13 {label} {kind:?}"),
                doc: doc(body.clone(), false),
                unit: "pt",
                value: Box::new(move |p| justify_extra(p, kind)),
            });
        }
    }
    v
}

/// P14：`w:br` 的分页符、分栏符之后，「乙」在第几页、离正文顶多远。
pub fn page_breaks() -> Vec<Measure> {
    let run =
        |inner: &str| format!(r#"<w:r><w:rPr>{FONTS}<w:sz w:val="24"/></w:rPr>{inner}</w:r>"#);
    let cases = [
        (
            "段中分页符",
            format!(
                "<w:p>{}</w:p>",
                run(r#"<w:t>标记甲行</w:t><w:br w:type="page"/><w:t>标记乙行</w:t>"#)
            ),
        ),
        (
            "段末分页符",
            format!(
                "<w:p>{}</w:p>{}",
                run(r#"<w:t>标记甲行</w:t><w:br w:type="page"/>"#),
                marker("乙")
            ),
        ),
        (
            "只有分页符的段落",
            format!(
                "{}<w:p>{}</w:p>{}",
                marker("甲"),
                run(r#"<w:br w:type="page"/>"#),
                marker("乙")
            ),
        ),
        (
            "分页符后接段前分页",
            format!(
                r#"<w:p>{}</w:p><w:p><w:pPr><w:pageBreakBefore/></w:pPr>{}</w:p>"#,
                run(r#"<w:t>标记甲行</w:t><w:br w:type="page"/>"#),
                run("<w:t>标记乙行</w:t>")
            ),
        ),
        (
            "分页符所在段有段后距",
            format!(
                r#"<w:p><w:pPr><w:spacing w:after="480"/></w:pPr>{}</w:p>{}"#,
                run(r#"<w:t>标记甲行</w:t><w:br w:type="page"/>"#),
                marker("乙")
            ),
        ),
        (
            "单栏里的分栏符",
            format!(
                "<w:p>{}</w:p>",
                run(r#"<w:t>标记甲行</w:t><w:br w:type="column"/><w:t>标记乙行</w:t>"#)
            ),
        ),
    ];
    let mut v = Vec::new();
    for (label, body) in cases {
        v.push(Measure {
            name: format!("P14 {label} 乙所在页"),
            doc: doc(body.clone(), false),
            unit: "页",
            value: Box::new(|p| baseline(p, "乙").map(|(pg, _)| pg as f32 + 1.0)),
        });
        v.push(Measure {
            name: format!("P14 {label} 乙离正文顶"),
            doc: doc(body, false),
            unit: "pt",
            value: Box::new(|p| baseline(p, "乙").map(|(pg, y)| p[pg].height - 72.0 - y)),
        });
    }
    v
}

/// 含「甲」的那一行里，字符 `c` 第一次出现的字形起点离左边距多远（左边距 1588 twips）。
fn glyph_x(pages: &[PageText], c: &str) -> Option<f32> {
    let line = pages
        .iter()
        .flat_map(|p| &p.lines)
        .find(|l| l.text.contains('甲'))?;
    line.frags
        .iter()
        .flat_map(|f| &f.glyphs)
        .find(|(t, _)| t == c)
        .map(|(_, x)| x - 1588.0 / 20.0)
}

/// 含「甲」的那一行里，最后一个字形的右缘离左边距多远。
fn line_right(pages: &[PageText]) -> Option<f32> {
    let line = pages
        .iter()
        .flat_map(|p| &p.lines)
        .find(|l| l.text.contains('甲'))?;
    Some(line.x1 - 1588.0 / 20.0)
}

/// P15：制表位。
pub fn tabs() -> Vec<Measure> {
    let para = |ppr: &str, runs: &str| {
        format!(
            r#"<w:p><w:pPr>{ppr}</w:pPr><w:r><w:rPr>{FONTS}<w:sz w:val="24"/></w:rPr>{runs}</w:r></w:p>"#
        )
    };
    let tab_doc = |body: String, default_stop: Option<u32>| {
        let settings = match default_stop {
            Some(v) => format!(r#"<w:defaultTabStop w:val="{v}"/>{COMPAT_15}"#),
            None => COMPAT_15.to_string(),
        };
        DocxBuilder::new().body(&body).settings(&settings)
    };
    let x_of = |c: &'static str| -> Extract { Box::new(move |p| glyph_x(p, c)) };
    let mut v = vec![
        Measure {
            name: "P15 默认制表位 420".into(),
            doc: tab_doc(para("", "<w:t>甲</w:t><w:tab/><w:t>乙</w:t>"), Some(420)),
            unit: "pt",
            value: x_of("乙"),
        },
        Measure {
            name: "P15 默认制表位 不写".into(),
            doc: tab_doc(para("", "<w:t>甲</w:t><w:tab/><w:t>乙</w:t>"), None),
            unit: "pt",
            value: x_of("乙"),
        },
        Measure {
            name: "P15 两个制表符 420".into(),
            doc: tab_doc(
                para(
                    "",
                    "<w:t>甲</w:t><w:tab/><w:t>乙</w:t><w:tab/><w:t>丙</w:t>",
                ),
                Some(420),
            ),
            unit: "pt",
            value: x_of("丙"),
        },
        Measure {
            name: "P15 首行缩进 24pt 后的制表符 420".into(),
            doc: tab_doc(
                para(
                    r#"<w:ind w:firstLine="480"/>"#,
                    "<w:t>甲</w:t><w:tab/><w:t>乙</w:t>",
                ),
                Some(420),
            ),
            unit: "pt",
            value: x_of("乙"),
        },
        Measure {
            name: "P15 左缩进 50pt 后的制表符 420".into(),
            doc: tab_doc(
                para(
                    r#"<w:ind w:left="1000"/>"#,
                    "<w:t>甲</w:t><w:tab/><w:t>乙</w:t>",
                ),
                Some(420),
            ),
            unit: "pt",
            value: x_of("乙"),
        },
        Measure {
            name: "P15 悬挂缩进 左 42 悬挂 21".into(),
            doc: tab_doc(
                para(
                    r#"<w:ind w:left="840" w:hanging="420"/>"#,
                    "<w:t>甲</w:t><w:tab/><w:t>乙</w:t>",
                ),
                Some(420),
            ),
            unit: "pt",
            value: x_of("乙"),
        },
        Measure {
            name: "P15 自定义左对齐 150pt".into(),
            doc: tab_doc(
                para(
                    r#"<w:tabs><w:tab w:val="left" w:pos="3000"/></w:tabs>"#,
                    "<w:t>甲</w:t><w:tab/><w:t>乙</w:t>",
                ),
                Some(420),
            ),
            unit: "pt",
            value: x_of("乙"),
        },
        Measure {
            name: "P15 自定义右对齐 300pt 文字右缘".into(),
            doc: tab_doc(
                para(
                    r#"<w:tabs><w:tab w:val="right" w:pos="6000"/></w:tabs>"#,
                    "<w:t>甲</w:t><w:tab/><w:t>乙乙乙</w:t>",
                ),
                Some(420),
            ),
            unit: "pt",
            value: Box::new(line_right),
        },
        Measure {
            name: "P15 自定义居中 200pt 首字".into(),
            doc: tab_doc(
                para(
                    r#"<w:tabs><w:tab w:val="center" w:pos="4000"/></w:tabs>"#,
                    "<w:t>甲</w:t><w:tab/><w:t>乙乙乙乙</w:t>",
                ),
                Some(420),
            ),
            unit: "pt",
            value: x_of("乙"),
        },
        Measure {
            name: "P15 小数点对齐 250pt 小数点".into(),
            doc: tab_doc(
                para(
                    r#"<w:tabs><w:tab w:val="decimal" w:pos="5000"/></w:tabs>"#,
                    "<w:t>甲</w:t><w:tab/><w:t>1234.50</w:t>",
                ),
                Some(420),
            ),
            unit: "pt",
            value: x_of("."),
        },
        Measure {
            name: "P15 越过最后一个自定义位".into(),
            doc: tab_doc(
                para(
                    r#"<w:tabs><w:tab w:val="left" w:pos="1000"/></w:tabs>"#,
                    "<w:t>甲</w:t><w:tab/><w:t>乙</w:t><w:tab/><w:t>丙</w:t>",
                ),
                Some(420),
            ),
            unit: "pt",
            value: x_of("丙"),
        },
        Measure {
            name: "P15 前导点 右对齐 300pt 点数".into(),
            doc: tab_doc(
                para(
                    r#"<w:tabs><w:tab w:val="right" w:leader="dot" w:pos="6000"/></w:tabs>"#,
                    "<w:t>甲</w:t><w:tab/><w:t>乙</w:t>",
                ),
                Some(420),
            ),
            unit: "个",
            value: Box::new(|p| {
                let line = p
                    .iter()
                    .flat_map(|p| &p.lines)
                    .find(|l| l.text.contains('甲'))?;
                Some(line.text.chars().filter(|c| *c == '.' || *c == '·').count() as f32)
            }),
        },
    ];
    // 默认制表位前正好还差一点点：「甲」之后的第一个默认位离得太近时会不会跳到下一个。
    v.push(Measure {
        name: "P15 文字刚好越过默认位 420".into(),
        doc: tab_doc(para("", "<w:t>甲乙</w:t><w:tab/><w:t>丙</w:t>"), Some(420)),
        unit: "pt",
        value: x_of("丙"),
    });
    v
}

pub fn all() -> Vec<Measure> {
    let mut v = empty_paragraphs();
    v.extend(default_size());
    v.extend(paragraph_mark_in_last_line());
    v.extend(exact_and_at_least());
    v.extend(trailing_space());
    v.extend(paragraph_spacing());
    v.extend(spacing_with_multiple());
    v.extend(multiple_spacing_transitions());
    v.extend(heading_transitions());
    v.extend(heading_by_compat());
    v.extend(large_text_lines());
    v.extend(first_baseline());
    v.extend(grid_origin());
    v.extend(page_bottom());
    v.extend(hanging_punctuation());
    v.extend(hanging_positions());
    v.extend(punctuation_compression());
    v.extend(justification());
    v.extend(page_breaks());
    v.extend(tabs());
    v
}
