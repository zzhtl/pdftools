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

fn doc(body: String, grid: bool) -> DocxBuilder {
    let d = DocxBuilder::new().body(&body);
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
            doc: DocxBuilder::new().body(&text),
            unit: "pt",
            value: Box::new(|p| size_of(p, "甲")),
        },
        Measure {
            name: "默认字号 docDefaults 不写 sz".into(),
            doc: DocxBuilder::new().body(&text).styles(&styles),
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
        doc: DocxBuilder::new().body(&body),
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

pub fn all() -> Vec<Measure> {
    let mut v = empty_paragraphs();
    v.extend(default_size());
    v.extend(paragraph_mark_in_last_line());
    v.extend(exact_and_at_least());
    v.extend(trailing_space());
    v
}
