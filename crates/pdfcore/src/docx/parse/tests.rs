//! 解析层：只看「读到了什么」，不涉及层叠与排版。

use super::*;
use crate::docx::model::{
    Align, Anchor, AnchorPos, Block, Drawing, Geometry, Para, Picture, RunItem, Shape, VAlign,
    WrapKind,
};

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

/// `mc:AlternateContent` 只取一个分支，不然同一个对象会出现两次：认识的特性（`wps`
/// 形状）取 Choice，不认识的取 Fallback。
#[test]
fn alternate_content_yields_one_branch() {
    let doc = body(
        r#"<w:p><w:r><mc:AlternateContent><mc:Choice Requires="wps"><w:drawing><wp:docPr id="1" name="a" descr="新版"/></w:drawing></mc:Choice><mc:Fallback><w:pict><v:shape/></w:pict></mc:Fallback></mc:AlternateContent></w:r></w:p>
<mc:AlternateContent><mc:Choice Requires="w14"><w:p><w:r><w:t>新版</w:t></w:r></w:p></mc:Choice><mc:Fallback><w:p><w:r><w:t>兼容</w:t></w:r></w:p></mc:Fallback></mc:AlternateContent>"#,
    );
    let ps = paras(&doc);
    let items: Vec<&RunItem> = ps[0].runs.iter().flat_map(|r| &r.items).collect();
    assert_eq!(
        items,
        vec![&RunItem::Drawing(Box::new(Drawing {
            alt: Some("新版".into()),
            ..Default::default()
        }))],
        "只该有 Choice 里那一个"
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
        vec![&RunItem::Drawing(Box::new(Drawing {
            alt: Some("\"公章\" & 签名".into()),
            ..Default::default()
        }))]
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

/// 脚注、尾注的引用记下来（本版本不排注释，要给警告）；分栏数取 `@w:num`，各栏分别
/// 写宽度时数 `w:col`。
#[test]
fn note_references_and_columns() {
    let doc = body(
        r#"<w:p><w:r><w:t>正文</w:t></w:r><w:r><w:footnoteReference w:id="1"/></w:r><w:r><w:endnoteReference w:id="2"/></w:r></w:p>
<w:p><w:pPr><w:sectPr><w:cols w:num="2" w:space="425"/></w:sectPr></w:pPr></w:p>
<w:sectPr><w:cols w:equalWidth="0"><w:col w:w="3000"/><w:col w:w="2000"/><w:col w:w="1000"/></w:cols></w:sectPr>"#,
    );
    let items: Vec<&RunItem> = paras(&doc)[0].runs.iter().flat_map(|r| &r.items).collect();
    assert_eq!(
        items,
        vec![
            &RunItem::Text("正文".into()),
            &RunItem::NoteReference,
            &RunItem::NoteReference
        ]
    );
    assert_eq!(paras(&doc)[1].section.as_ref().map(|s| s.columns), Some(2));
    assert_eq!(doc.section.columns, 3);
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

/// 域：`w:fldChar`、`w:instrText` 按原样记下；`w:fldSimple` 展开成同样的序列，
/// 里面的 run 是结果。
#[test]
fn fields_are_recorded_as_items() {
    use crate::docx::model::FieldChar::*;
    let doc = body(
        r#"<w:p><w:r><w:fldChar w:fldCharType="begin"><w:ffData/></w:fldChar></w:r><w:r><w:instrText xml:space="preserve"> PAGE </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>3</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r>
<w:fldSimple w:instr=" NUMPAGES "><w:r><w:t>9</w:t></w:r></w:fldSimple><w:fldSimple w:instr="SECTIONPAGES"/></w:p>"#,
    );
    let items: Vec<RunItem> = paras(&doc)[0]
        .runs
        .iter()
        .flat_map(|r| r.items.clone())
        .collect();
    let code = |s: &str| RunItem::FieldCode(s.into());
    assert_eq!(
        items,
        [
            RunItem::FieldChar(Begin),
            code(" PAGE "),
            RunItem::FieldChar(Separate),
            RunItem::Text("3".into()),
            RunItem::FieldChar(End),
            RunItem::FieldChar(Begin),
            code(" NUMPAGES "),
            RunItem::FieldChar(Separate),
            RunItem::Text("9".into()),
            RunItem::FieldChar(End),
            RunItem::FieldChar(Begin),
            code("SECTIONPAGES"),
            RunItem::FieldChar(Separate),
            RunItem::FieldChar(End),
        ]
    );
}

/// 表格最多解析 16 层；再往里的不解析结构，字收成一段，段与段之间隔一个空格。
#[test]
fn deeply_nested_tables_become_text() {
    let mut xml = "<w:p><w:r><w:t>最里层</w:t></w:r></w:p><w:p><w:r><w:t>第二段</w:t></w:r></w:p>"
        .to_string();
    for _ in 0..20 {
        xml = format!("<w:tbl><w:tr><w:tc>{xml}</w:tc></w:tr></w:tbl><w:p/>");
    }
    let doc = body(&xml);
    let mut depth = 0;
    let mut blocks = &doc.body;
    loop {
        match blocks.first() {
            Some(Block::Table(t)) => {
                depth += 1;
                blocks = &t.rows[0].cells[0].content;
            }
            Some(Block::Para(p)) => {
                assert_eq!(text_of(p), "最里层 第二段");
                break;
            }
            None => panic!("第 {depth} 层的单元格是空的"),
        }
    }
    assert_eq!(depth, 16);
}

/// 行内的图片：显示大小、图片的关系 id、裁剪。组合里的图不算单独的图片。
#[test]
fn inline_pictures_are_read() {
    let pic = |extra: &str| {
        format!(
            r#"<w:r><w:drawing><wp:inline><wp:extent cx="1270000" cy="635000"/><wp:docPr id="1" name="p" descr="签名"/><a:graphic xmlns:a="a"><a:graphicData uri="pic">{extra}</a:graphicData></a:graphic></wp:inline></w:drawing></w:r>"#
        )
    };
    let picture = r#"<pic:pic xmlns:pic="pic"><pic:blipFill><a:blip r:embed="rId7"/><a:srcRect l="25000" r="10000"/></pic:blipFill><pic:spPr><a:xfrm><a:ext cx="1" cy="1"/></a:xfrm></pic:spPr></pic:pic>"#;
    let group = format!("<wpg:wgp>{picture}</wpg:wgp>");
    let doc = body(&format!(
        r#"<w:p>{}</w:p><w:p>{}</w:p>"#,
        pic(picture),
        pic(&group)
    ));
    let drawing = |i: usize| match &paras(&doc)[i].runs[0].items[0] {
        RunItem::Drawing(d) => (**d).clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(
        drawing(0),
        Drawing {
            alt: Some("签名".into()),
            inline: true,
            extent: Some((1_270_000, 635_000)),
            picture: Some(Picture {
                target: Some("rId7".into()),
                crop: [25_000, 0, 10_000, 0],
            }),
            anchor: None,
            shape: None,
        }
    );
    assert_eq!(drawing(1).picture, None);
}

/// 浮动的图：位置（偏移或对齐）、环绕、衬于文字下方、与文字的距离；`simplePos`
/// 写的是相对纸张左上角的位置。
#[test]
fn anchored_pictures_are_read() {
    let anchor = |attrs: &str, inner: &str| {
        format!(
            r#"<w:p><w:r><w:drawing><wp:anchor {attrs}>{inner}<wp:extent cx="12700" cy="25400"/><wp:docPr id="1" name="印章"/></wp:anchor></w:drawing></w:r></w:p>"#
        )
    };
    let doc = body(
        &(anchor(
            r#"behindDoc="1" distT="12700" distB="0" distL="25400" distR="0" simplePos="0""#,
            r#"<wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="margin"><wp:align>center</wp:align></wp:positionH><wp:positionV relativeFrom="paragraph"><wp:posOffset>-63500</wp:posOffset></wp:positionV><wp:wrapTopAndBottom/>"#,
        ) + &anchor(
            r#"behindDoc="0" simplePos="1""#,
            r#"<wp:simplePos x="127000" y="254000"/><wp:positionH relativeFrom="column"><wp:posOffset>1</wp:posOffset></wp:positionH><wp:wrapNone/>"#,
        )),
    );
    let anchors: Vec<Anchor> = paras(&doc)
        .iter()
        .map(|p| match &p.runs[0].items[0] {
            RunItem::Drawing(d) => match &**d {
                Drawing {
                    inline: false,
                    anchor: Some(a),
                    extent: Some((12700, 25400)),
                    ..
                } => a.clone(),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        anchors[0],
        Anchor {
            h: AnchorPos {
                from: Some("margin".into()),
                offset: None,
                align: Some("center".into()),
            },
            v: AnchorPos {
                from: Some("paragraph".into()),
                offset: Some(-63500),
                align: None,
            },
            wrap: WrapKind::TopAndBottom,
            behind: true,
            dist: [12700, 0, 25400, 0],
        }
    );
    let page = |offset| AnchorPos {
        from: Some("page".into()),
        offset: Some(offset),
        align: None,
    };
    assert_eq!(
        (anchors[1].h.clone(), anchors[1].v.clone()),
        (page(127000), page(254000))
    );
    assert_eq!(
        (anchors[1].wrap, anchors[1].behind),
        (WrapKind::None, false)
    );
}

fn drawings(doc: &Document) -> Vec<Drawing> {
    paras(doc)
        .iter()
        .flat_map(|p| p.runs.iter().flat_map(|r| &r.items))
        .filter_map(|i| match i {
            RunItem::Drawing(d) => Some((**d).clone()),
            _ => None,
        })
        .collect()
}

/// 形状（`wps:wsp`）：填充、轮廓、几何形状、翻转与旋转、文本框里的段落、文字边距与
/// 竖直对齐。`a:noFill` 就是没有；主题色只认白与黑；轮廓没写颜色按黑色。
#[test]
fn shapes_and_text_boxes_are_read() {
    let wsp = |sp_pr: &str, rest: &str| {
        format!(
            r#"<w:p><w:r><w:drawing><wp:inline><wp:extent cx="254000" cy="127000"/><wp:docPr id="1" name="s"/><a:graphic xmlns:a="a"><a:graphicData uri="wps"><wps:wsp xmlns:wps="wps"><wps:spPr>{sp_pr}</wps:spPr>{rest}</wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#
        )
    };
    let doc = body(
        &(wsp(
            r#"<a:prstGeom prst="rect"/><a:solidFill><a:srgbClr val="FFF2CC"/></a:solidFill><a:ln w="12700"><a:solidFill><a:schemeClr val="tx1"/></a:solidFill></a:ln>"#,
            r#"<wps:txbx><w:txbxContent><w:p><w:r><w:t>框内</w:t></w:r></w:p><w:p/></w:txbxContent></wps:txbx><wps:bodyPr lIns="0" tIns="12700" anchor="ctr"/>"#,
        ) + &wsp(
            r#"<a:xfrm flipH="1"><a:off x="0" y="0"/></a:xfrm><a:prstGeom prst="line"/><a:noFill/><a:ln w="6350"/>"#,
            r#"<wps:style><a:lnRef idx="1"><a:schemeClr val="accent1"/></a:lnRef></wps:style><wps:bodyPr/>"#,
        ) + &wsp(
            r#"<a:xfrm rot="5400000"/><a:prstGeom prst="ellipse"/><a:solidFill><a:schemeClr val="accent1"/></a:solidFill><a:ln><a:noFill/></a:ln>"#,
            "",
        )),
    );
    let shapes: Vec<Shape> = drawings(&doc)
        .into_iter()
        .map(|d| {
            assert_eq!(
                (d.inline, d.extent, &d.picture),
                (true, Some((254_000, 127_000)), &None)
            );
            d.shape.expect("是形状")
        })
        .collect();
    let text = shapes[0].text.as_ref().expect("有文本框");
    assert_eq!(text.len(), 2);
    match &text[0] {
        Block::Para(p) => assert_eq!(text_of(p), "框内"),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        Shape {
            text: None,
            ..shapes[0].clone()
        },
        Shape {
            geometry: Geometry::Rect,
            fill: Some([0xFF, 0xF2, 0xCC]),
            line: Some((12_700, [0, 0, 0])),
            text: None,
            insets: [0, 12_700, 91_440, 45_720],
            text_align: VAlign::Center,
            flip: [false; 2],
            rotation: 0,
        }
    );
    assert_eq!(
        shapes[1],
        Shape {
            geometry: Geometry::Line,
            fill: None,
            line: Some((6_350, [0, 0, 0])),
            text: None,
            insets: [91_440, 45_720, 91_440, 45_720],
            text_align: VAlign::Top,
            flip: [true, false],
            rotation: 0,
        }
    );
    assert_eq!(
        (
            shapes[2].geometry,
            shapes[2].fill,
            shapes[2].line,
            shapes[2].rotation
        ),
        (Geometry::Other, None, None, 5_400_000)
    );
}

/// VML（`w:pict`）：样式里的位置与大小、`v:imagedata` 的图与裁剪、`v:textbox` 的
/// 文字、填充与描边（缺省填白描黑，子元素 `v:fill` / `v:stroke` 可以改）、`w10:wrap`、
/// z-index 为负是衬于文字下方；直线由两个端点定位置与方向；组合只留位置、大小与
/// 替代文字。
#[test]
fn vml_shapes_are_read() {
    let pict = |inner: &str| format!(r#"<w:p><w:r><w:pict>{inner}</w:pict></w:r></w:p>"#);
    let doc = body(
        &(pict(
            r##"<v:shapetype id="_x0000_t75" coordsize="21600,21600"><v:path o:extrusionok="f"/></v:shapetype><v:shape type="#_x0000_t75" style="position:absolute;margin-left:80pt;margin-top:1in;width:100pt;height:50pt;z-index:-251657216;mso-position-horizontal-relative:page;mso-position-vertical:top;mso-position-vertical-relative:margin"><v:imagedata r:id="rId9" o:title="印" cropleft="6554f" croptop=".25"/><w10:wrap type="topAndBottom"/></v:shape>"##,
        ) + &pict(
            r#"<v:shape style="width:60pt;height:30pt"><v:imagedata r:id="rId9"/></v:shape>"#,
        ) + &pict(
            r##"<v:rect style="width:2in;height:1in" fillcolor="#fc0" strokecolor="red" strokeweight="2pt"><v:textbox inset="0,1pt,0,1pt"><w:txbxContent><w:p><w:r><w:t>框内</w:t></w:r></w:p></w:txbxContent></v:textbox></v:rect>"##,
        ) + &pict(
            r##"<v:line style="position:absolute;z-index:1" from="0,15.6pt" to="400pt,15pt" strokecolor="red" strokeweight="3pt"><v:stroke color="#00f"/></v:line>"##,
        ) + &pict(
            r#"<v:group alt="组合" style="width:10pt;height:10pt"><v:shape style="width:10pt;height:10pt"><v:imagedata r:id="rId9"/></v:shape></v:group>"#,
        ) + &pict(
            r#"<v:roundrect style="width:10pt;height:10pt" filled="f"><v:stroke on="f"/></v:roundrect>"#,
        )),
    );
    let d = drawings(&doc);
    assert_eq!(d.len(), 6);

    assert_eq!(
        (d[0].inline, d[0].extent, d[0].alt.as_deref(), &d[0].shape),
        (false, Some((1_270_000, 635_000)), Some("印"), &None)
    );
    assert_eq!(
        d[0].picture,
        Some(Picture {
            target: Some("rId9".into()),
            crop: [10_001, 25_000, 0, 0],
        })
    );
    assert_eq!(
        d[0].anchor,
        Some(Anchor {
            h: AnchorPos {
                from: Some("page".into()),
                offset: Some(1_016_000),
                align: None,
            },
            v: AnchorPos {
                from: Some("margin".into()),
                offset: Some(914_400),
                align: Some("top".into()),
            },
            wrap: WrapKind::TopAndBottom,
            behind: true,
            dist: [0; 4],
        })
    );

    assert_eq!(
        (d[1].inline, d[1].extent, &d[1].anchor),
        (true, Some((762_000, 381_000)), &None)
    );
    assert_eq!(
        d[1].picture.as_ref().and_then(|p| p.target.as_deref()),
        Some("rId9")
    );

    let text_box = d[2].shape.as_ref().expect("是形状");
    assert_eq!(d[2].extent, Some((1_828_800, 914_400)));
    assert_eq!(
        (
            text_box.geometry,
            text_box.fill,
            text_box.line,
            text_box.insets
        ),
        (
            Geometry::Rect,
            Some([0xFF, 0xCC, 0x00]),
            Some((25_400, [0xFF, 0, 0])),
            [0, 12_700, 0, 12_700]
        )
    );
    assert_eq!(text_box.text.as_ref().map(Vec::len), Some(1));

    let line = d[3].shape.as_ref().expect("是形状");
    assert_eq!(
        (line.geometry, line.fill, line.line, line.flip),
        (
            Geometry::Line,
            None,
            Some((38_100, [0, 0, 0xFF])),
            [true, false]
        )
    );
    assert_eq!(d[3].extent, Some((5_080_000, 7_620)));
    let a = d[3].anchor.as_ref().expect("浮动");
    assert_eq!(
        (&a.h.from, a.h.offset, &a.v.from, a.v.offset),
        (
            &Some("column".to_string()),
            Some(0),
            &Some("paragraph".to_string()),
            Some(190_500)
        )
    );

    assert_eq!(
        (d[4].alt.as_deref(), d[4].extent, &d[4].picture, &d[4].shape),
        (Some("组合"), Some((127_000, 127_000)), &None, &None)
    );

    let plain = d[5].shape.as_ref().expect("是形状");
    assert_eq!(
        (plain.geometry, plain.fill, plain.line),
        (Geometry::RoundRect, None, None)
    );
}
