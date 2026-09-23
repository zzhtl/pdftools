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
    v
}
