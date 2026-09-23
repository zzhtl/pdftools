//! 解析层：只看「读到了什么」，不涉及层叠与排版。

use super::*;
use crate::docx::model::{Align, Block, Para, RunItem};

fn body(inner: &str) -> Document {
    let xml = format!(
        r#"<w:document xmlns:w="w" xmlns:mc="mc" xmlns:wp="wp" xmlns:v="v"><w:body>{inner}</w:body></w:document>"#
    );
    parse_document(&xml, Styles::default(), Settings::default()).expect("解析失败")
}

fn paras(doc: &Document) -> Vec<&Para> {
    doc.body
        .iter()
        .filter_map(|b| match b {
            Block::Para(p) => Some(p),
            Block::Table(_) => None,
        })
        .collect()
}

fn text_of(p: &Para) -> String {
    p.runs
        .iter()
        .flat_map(|r| &r.items)
        .filter_map(|i| match i {
            RunItem::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn self_closing_paragraph_is_a_paragraph() {
    let doc = body("<w:p/><w:p><w:r><w:t>甲</w:t></w:r></w:p><w:p/>");
    assert_eq!(paras(&doc).len(), 3);
}

/// 修订记录里的旧格式不能当成当前格式，也不能打断其后属性的读取。
#[test]
fn tracked_formatting_changes_are_ignored() {
    let doc = body(
        r#"<w:p><w:pPr><w:jc w:val="center"/><w:pPrChange w:id="1"><w:pPr><w:jc w:val="right"/><w:ind w:left="999"/></w:pPr></w:pPrChange><w:ind w:left="100"/></w:pPr>
<w:r><w:rPr><w:b/><w:rPrChange w:id="2"><w:rPr><w:sz w:val="40"/><w:i/></w:rPr></w:rPrChange><w:sz w:val="24"/></w:rPr><w:t>甲</w:t></w:r></w:p>"#,
    );
    let p = paras(&doc)[0];
    assert_eq!(p.ppr.align, Some(Align::Center));
    assert_eq!(p.ppr.indent.left_twips, Some(100));
    let rpr = &p.runs[0].rpr;
    assert_eq!(
        (rpr.bold, rpr.italic, rpr.size_half_pt),
        (Some(true), None, Some(24))
    );
}

/// 域代码是给 Word 的指令；显示的是域结果。
#[test]
fn field_codes_are_not_text() {
    let doc = body(
        r#"<w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> PAGE </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>7</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#,
    );
    assert_eq!(text_of(paras(&doc)[0]), "7");
}

/// `mc:AlternateContent` 只取一个分支，不然同一个对象会出现两次。
#[test]
fn alternate_content_yields_one_branch() {
    let doc = body(
        r#"<w:p><w:r><mc:AlternateContent><mc:Choice Requires="wps"><w:drawing><wp:docPr id="1" name="a" descr="新版"/></w:drawing></mc:Choice><mc:Fallback><w:pict><v:shape/></w:pict></mc:Fallback></mc:AlternateContent></w:r></w:p>
<mc:AlternateContent><mc:Choice Requires="wps"><w:p><w:r><w:t>新版</w:t></w:r></w:p></mc:Choice><mc:Fallback><w:p><w:r><w:t>兼容</w:t></w:r></w:p></mc:Fallback></mc:AlternateContent>"#,
    );
    let ps = paras(&doc);
    let items: Vec<&RunItem> = ps[0].runs.iter().flat_map(|r| &r.items).collect();
    assert_eq!(
        items,
        vec![&RunItem::Drawing { alt: None }],
        "只该有 Fallback 里那一个"
    );
    assert_eq!(ps.len(), 2);
    assert_eq!(text_of(ps[1]), "兼容");
}

#[test]
fn deleted_and_moved_away_text_is_dropped() {
    let doc = body(
        r#"<w:p><w:r><w:t>留</w:t></w:r><w:del w:id="1"><w:r><w:delText>删</w:delText></w:r></w:del><w:moveFrom w:id="2"><w:r><w:t>移走</w:t></w:r></w:moveFrom><w:ins w:id="3"><w:r><w:t>增</w:t></w:r></w:ins><w:moveTo w:id="4"><w:r><w:t>移来</w:t></w:r></w:moveTo></w:p>"#,
    );
    assert_eq!(text_of(paras(&doc)[0]), "留增移来");
}

/// 没有 `xml:space="preserve"` 时只去整个元素的首尾空白；实体两边的空格要留着。
#[test]
fn entities_keep_the_spaces_around_them() {
    let doc = body(r#"<w:p><w:r><w:t> A &amp; B </w:t></w:r></w:p>"#);
    assert_eq!(text_of(paras(&doc)[0]), "A & B");
}

#[test]
fn attribute_values_are_unescaped() {
    let doc = body(
        r#"<w:p><w:r><w:drawing><wp:docPr id="1" name="a" descr="&quot;公章&quot; &amp; 签名"/></w:drawing></w:r></w:p>"#,
    );
    let items: Vec<&RunItem> = paras(&doc)[0].runs.iter().flat_map(|r| &r.items).collect();
    assert_eq!(
        items,
        vec![&RunItem::Drawing {
            alt: Some("\"公章\" & 签名".into())
        }]
    );
}

/// 表格样式、编号样式也标 `w:default="1"`，而且常常排在 Normal 前面。
#[test]
fn only_paragraph_styles_can_be_the_default_paragraph_style() {
    let styles = parse_styles(
        r#"<w:styles>
<w:style w:type="table" w:default="1" w:styleId="TableNormal"><w:rPr><w:sz w:val="40"/></w:rPr></w:style>
<w:style w:type="numbering" w:default="1" w:styleId="NoList"/>
<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:rPr><w:sz w:val="21"/></w:rPr></w:style>
</w:styles>"#,
    );
    assert_eq!(styles.default_paragraph_style.as_deref(), Some("Normal"));
    assert!(!styles.paragraph.contains_key("TableNormal"));
}

#[test]
fn nested_tables_keep_every_cell() {
    let doc = body(
        r#"<w:tbl><w:tr><w:tc><w:p><w:r><w:t>外甲</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t>内</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:t>外乙</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>右</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
    );
    let Block::Table(t) = &doc.body[0] else {
        panic!("应当是表格");
    };
    assert_eq!(t.rows.len(), 1);
    assert_eq!(t.rows[0].cells.len(), 2);
    let first = &t.rows[0].cells[0].content;
    assert_eq!(first.len(), 3, "外层单元格：段落、嵌套表格、段落");
}

#[test]
fn breaks_keep_their_kind() {
    let doc = body(
        r#"<w:p><w:r><w:t>一</w:t><w:br/><w:t>二</w:t><w:br w:type="page"/><w:t>三</w:t><w:br w:type="column"/><w:cr/><w:tab/><w:noBreakHyphen/></w:r></w:p>"#,
    );
    use crate::docx::model::BreakKind::*;
    let items: Vec<RunItem> = paras(&doc)[0].runs[0].items.clone();
    assert_eq!(
        items,
        vec![
            RunItem::Text("一".into()),
            RunItem::Break(Line),
            RunItem::Text("二".into()),
            RunItem::Break(Page),
            RunItem::Text("三".into()),
            RunItem::Break(Column),
            RunItem::Break(Line),
            RunItem::Tab,
            RunItem::NoBreakHyphen,
        ]
    );
}

#[test]
fn html_paragraph_spacing_switch() {
    let on = parse_settings(
        r#"<w:settings><w:compat><w:doNotUseHTMLParagraphAutoSpacing/><w:useFELayout/></w:compat></w:settings>"#,
    );
    let off = parse_settings(r#"<w:settings><w:compat><w:useFELayout/></w:compat></w:settings>"#);
    assert!(on.no_html_paragraph_spacing);
    assert!(!off.no_html_paragraph_spacing);
}

/// 同一个 `w:rFonts` 里主题字体优先于字体名；`w:hAnsi*` 只在 `w:ascii*` 都没写时顶上。
#[test]
fn theme_fonts_take_precedence_within_one_rfonts() {
    use crate::docx::model::{FontRef, ThemeFont, ThemeScript};
    let doc = body(
        r#"<w:p><w:r><w:rPr><w:rFonts w:ascii="Serif A" w:asciiTheme="majorHAnsi" w:eastAsia="Song B" w:hint="eastAsia"/></w:rPr><w:t>甲</w:t></w:r>
<w:r><w:rPr><w:rFonts w:hAnsiTheme="minorHAnsi" w:eastAsiaTheme="minorEastAsia"/></w:rPr><w:t>乙</w:t></w:r>
<w:r><w:rPr><w:rFonts w:hAnsi="Sans C" w:hint="default"/></w:rPr><w:t>丙</w:t></w:r></w:p>"#,
    );
    let runs = &paras(&doc)[0].runs;
    let theme = |major, script| Some(FontRef::Theme(ThemeFont { major, script }));
    assert_eq!(runs[0].rpr.font_ascii, theme(true, ThemeScript::Latin));
    assert_eq!(
        runs[0].rpr.font_east_asia,
        Some(FontRef::Name("Song B".into()))
    );
    assert_eq!(runs[0].rpr.legacy_font_ascii.as_deref(), Some("Serif A"));
    assert_eq!(runs[0].rpr.hint_east_asia, Some(true));
    assert_eq!(runs[1].rpr.font_ascii, theme(false, ThemeScript::Latin));
    assert_eq!(
        runs[1].rpr.font_east_asia,
        theme(false, ThemeScript::EastAsia)
    );
    assert_eq!(runs[1].rpr.legacy_font_ascii, None);
    assert_eq!(runs[2].rpr.font_ascii, Some(FontRef::Name("Sans C".into())));
    assert_eq!(runs[2].rpr.hint_east_asia, Some(false));
}

#[test]
fn theme_font_scheme_and_language() {
    let theme = parse_theme(
        r#"<a:theme xmlns:a="a"><a:themeElements><a:fontScheme name="x">
<a:majorFont><a:latin typeface="Major Latin"></a:latin><a:ea typeface=""/><a:font script="Hans" typeface="Major Hans"/></a:majorFont>
<a:minorFont><a:latin typeface="Minor Latin"/><a:ea typeface="Minor EA"/><a:font script="Jpan" typeface="Minor Jpan"/></a:minorFont>
</a:fontScheme></a:themeElements></a:theme>"#,
    );
    assert_eq!(theme.major.latin.as_deref(), Some("Major Latin"));
    assert_eq!(theme.major.east_asia, None, "空的 typeface 等于没写");
    assert_eq!(theme.major.by_script["Hans"], "Major Hans");
    assert_eq!(theme.minor.latin.as_deref(), Some("Minor Latin"));
    assert_eq!(theme.minor.east_asia.as_deref(), Some("Minor EA"));
    assert_eq!(theme.minor.by_script["Jpan"], "Minor Jpan");

    // 自闭合的 majorFont 不能让后面的字体被记到它名下。
    let theme = parse_theme(
        r#"<a:theme xmlns:a="a"><a:majorFont/><a:latin typeface="Stray"/><a:minorFont><a:latin typeface="Minor Latin"/></a:minorFont></a:theme>"#,
    );
    assert_eq!(theme.major.latin, None);
    assert_eq!(theme.minor.latin.as_deref(), Some("Minor Latin"));

    let settings = parse_settings(
        r#"<w:settings><w:themeFontLang w:val="en-US" w:eastAsia="zh-CN"/></w:settings>"#,
    );
    assert_eq!(settings.theme_font_lang_east_asia.as_deref(), Some("zh-CN"));
}

#[test]
fn paragraph_borders_and_shading() {
    use crate::docx::model::BorderStyle;
    let doc = body(
        r#"<w:p><w:pPr><w:pBdr><w:top w:val="single" w:sz="12" w:space="1" w:color="FF0000"/><w:left w:val="nil"/><w:bottom w:val="thinThickSmallGap" w:sz="6" w:space="4" w:color="auto"/><w:between w:val="dashSmallGap"/></w:pBdr><w:shd w:val="pct20" w:color="auto" w:fill="FFFFFF"/></w:pPr></w:p>
<w:p><w:pPr><w:shd w:val="solid" w:color="00FF00" w:fill="FF0000"/></w:pPr><w:r><w:rPr><w:shd w:val="clear" w:color="auto" w:fill="auto"/></w:rPr><w:t>甲</w:t></w:r></w:p>"#,
    );
    let ps = paras(&doc);
    let b = &ps[0].ppr.borders;
    let top = b.top.unwrap();
    assert_eq!(
        (top.style, top.size_eighths, top.space_pt, top.color),
        (BorderStyle::Single, 12, 1, Some([0xFF, 0, 0]))
    );
    assert_eq!(
        b.left.unwrap().style,
        BorderStyle::None,
        "nil 要能盖掉样式里的边"
    );
    let bottom = b.bottom.unwrap();
    assert_eq!((bottom.style, bottom.color), (BorderStyle::Double, None));
    assert_eq!(b.between.unwrap().style, BorderStyle::Dashed);
    assert_eq!(b.right, None);
    // 20% 的黑色图案盖在白底上。
    assert_eq!(ps[0].ppr.shading, Some(Some([204, 204, 204])));
    // solid 全是前景色，不是底色。
    assert_eq!(ps[1].ppr.shading, Some(Some([0, 0xFF, 0])));
    assert_eq!(
        ps[1].runs[0].rpr.shading,
        Some(None),
        "auto 底色等于没有底纹"
    );
}

#[test]
fn numbering_definitions() {
    use crate::docx::model::NumSuffix;
    let n = parse_numbering(
        r#"<w:numbering xmlns:w="w" xmlns:mc="mc"><w:abstractNum w:abstractNumId="3"><w:multiLevelType w:val="multilevel"/>
<w:lvl w:ilvl="0"><w:start w:val="2"/><w:numFmt w:val="chineseCounting"/><w:suff w:val="space"/><w:lvlText w:val="第%1条"/><w:lvlJc w:val="right"/><w:pPr><w:ind w:left="420" w:hanging="420"/></w:pPr><w:rPr><w:b/></w:rPr></w:lvl>
<w:lvl w:ilvl="1"><mc:AlternateContent><mc:Choice Requires="w14"><w:numFmt w:val="custom" w:format="01, 02, 03, ..."/></mc:Choice><mc:Fallback><w:numFmt w:val="decimalZero"/></mc:Fallback></mc:AlternateContent><w:lvlRestart w:val="0"/><w:isLgl/><w:lvlText w:val="%1.%2"/></w:lvl>
<w:lvl w:ilvl="9"><w:numFmt w:val="decimal"/></w:lvl>
</w:abstractNum>
<w:abstractNum w:abstractNumId="4"><w:numStyleLink w:val="ListStyle"/></w:abstractNum>
<w:num w:numId="7"><w:abstractNumId w:val="3"/><w:lvlOverride w:ilvl="0"><w:startOverride w:val="5"/></w:lvlOverride><w:lvlOverride w:ilvl="1"><w:lvl w:ilvl="1"><w:numFmt w:val="upperLetter"/><w:lvlText w:val="%2)"/></w:lvl></w:lvlOverride></w:num>
</w:numbering>"#,
    );
    let a = &n.abstracts[&3];
    assert_eq!(a.levels.len(), 2, "第 9 级超出范围，不收");
    let l0 = &a.levels[&0];
    assert_eq!(
        (l0.start, l0.format.as_deref(), l0.text.as_deref()),
        (Some(2), Some("chineseCounting"), Some("第%1条"))
    );
    assert_eq!(
        (l0.suffix, l0.align),
        (Some(NumSuffix::Space), Some(Align::Right))
    );
    assert_eq!(
        (
            l0.ppr.indent.left_twips,
            l0.ppr.indent.hanging_twips,
            l0.rpr.bold
        ),
        (Some(420), Some(420), Some(true))
    );
    let l1 = &a.levels[&1];
    assert_eq!(
        l1.format.as_deref(),
        Some("decimalZero"),
        "取 mc:Fallback 的通用格式"
    );
    assert_eq!((l1.restart, l1.legal), (Some(0), true));
    assert_eq!(n.abstracts[&4].num_style_link.as_deref(), Some("ListStyle"));
    let num = &n.nums[&7];
    assert_eq!(num.abstract_id, 3);
    assert_eq!(num.overrides[&0].start, Some(5));
    let over = num.overrides[&1].level.as_ref().unwrap();
    assert_eq!(over.format.as_deref(), Some("upperLetter"));
}
