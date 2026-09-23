//! PDF 页面操作：取页、重排、旋转、合并；继承来的属性不丢，没选的页不被带进来。

use lopdf::{dictionary, Document, Object, ObjectId, Stream};
use pdfcore::ops::pdf_pages::{assemble, PagePick, Source};
use pdfcore::pdf::render;
use pdfcore::{CoreError, WarningKind};

/// 画一个 50×50 黑框的表单 XObject，挂在 `/Resources` 里叫 `/Box`。
fn box_resources(doc: &mut Document) -> ObjectId {
    let form = doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 50.into(), 50.into()],
        },
        b"0 g 0 0 50 50 re f".to_vec(),
    ));
    doc.add_object(dictionary! { "XObject" => dictionary! { "Box" => form } })
}

/// 内容流：在 (20, 20) 画黑框，前面一行注释当标记，用来认出是哪一页的内容。
fn content(doc: &mut Document, marker: &str) -> ObjectId {
    doc.add_object(Stream::new(
        dictionary! {},
        format!("% {marker}\nq 1 0 0 1 20 20 cm /Box Do Q").into_bytes(),
    ))
}

fn save(mut doc: Document, catalog: ObjectId) -> Vec<u8> {
    doc.trailer.set("Root", catalog);
    let mut buf = Vec::new();
    doc.save_to(&mut buf).unwrap();
    buf
}

/// 三页。资源和页面尺寸写在根 `/Pages` 上，第二页的 `/Rotate 90` 写在中间一层
/// `/Pages` 上，第三页有自己的尺寸：换了页树，这些都得跟着页面走。
fn inherited() -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let resources = box_resources(&mut doc);
    let (root, mid) = (doc.new_object_id(), doc.new_object_id());
    let page = |doc: &mut Document, parent, marker: &str, media: Option<i64>| {
        let c = content(doc, marker);
        let mut d = dictionary! { "Type" => "Page", "Parent" => parent, "Contents" => c };
        if let Some(w) = media {
            d.set("MediaBox", vec![0.into(), 0.into(), w.into(), 300.into()]);
        }
        doc.add_object(d)
    };
    let a1 = page(&mut doc, root, "PAGE-A1", None);
    let a2 = page(&mut doc, mid, "PAGE-A2", None);
    let a3 = page(&mut doc, root, "PAGE-A3", Some(120));
    doc.objects.insert(
        mid,
        Object::Dictionary(dictionary! {
            "Type" => "Pages", "Parent" => root, "Kids" => vec![a2.into()], "Count" => 1, "Rotate" => 90,
        }),
    );
    doc.objects.insert(
        root,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![a1.into(), mid.into(), a3.into()],
            "Count" => 3,
            "MediaBox" => vec![0.into(), 0.into(), 200.into(), 300.into()],
            "Resources" => resources,
        }),
    );
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => root });
    save(doc, catalog)
}

/// 两页横的，各自带资源；第一页有一个跳到第二页的链接。目录里有书签。
fn plain() -> Vec<u8> {
    let mut doc = Document::with_version("1.4");
    let resources = box_resources(&mut doc);
    let root = doc.new_object_id();
    let c1 = content(&mut doc, "PAGE-B1");
    let c2 = content(&mut doc, "PAGE-B2");
    let b2 = doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => root, "Contents" => c2, "Resources" => resources,
        "MediaBox" => vec![0.into(), 0.into(), 310.into(), 200.into()],
    });
    let link = doc.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "Link",
        "Rect" => vec![0.into(), 0.into(), 10.into(), 10.into()],
        "Dest" => vec![b2.into(), "Fit".into()],
    });
    let b1 = doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => root, "Contents" => c1, "Resources" => resources,
        "MediaBox" => vec![0.into(), 0.into(), 300.into(), 200.into()],
        "Annots" => vec![link.into()],
    });
    doc.objects.insert(
        root,
        Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => vec![b1.into(), b2.into()], "Count" => 2,
        }),
    );
    let item = doc.new_object_id();
    let outlines = doc.add_object(dictionary! {
        "Type" => "Outlines", "First" => item, "Last" => item, "Count" => 1,
    });
    doc.objects.insert(
        item,
        Object::Dictionary(dictionary! {
            "Title" => Object::string_literal("B"), "Parent" => outlines, "Dest" => vec![b1.into(), "Fit".into()],
        }),
    );
    let catalog = doc.add_object(dictionary! {
        "Type" => "Catalog", "Pages" => root, "Outlines" => outlines,
    });
    save(doc, catalog)
}

fn pick(source: usize, page: usize, rotate: i32) -> PagePick {
    PagePick {
        source,
        page,
        rotate,
    }
}

/// 输出里每页的宽度与 `/Rotate`（都是页面自己身上的，不再靠继承）。
fn layout(pdf: &[u8]) -> Vec<(i64, i64)> {
    let doc = Document::load_mem(pdf).unwrap();
    doc.get_pages()
        .into_values()
        .map(|id| {
            let page = doc.get_dictionary(id).unwrap();
            let media = page.get(b"MediaBox").unwrap().as_array().unwrap();
            let width = media[2].as_float().unwrap() as i64;
            let rotate = page.get(b"Rotate").map_or(0, |r| r.as_i64().unwrap());
            (width, rotate)
        })
        .collect()
}

#[test]
fn pages_are_reordered_rotated_and_merged() {
    let sources = [
        Source::open(&inherited()).unwrap(),
        Source::open(&plain()).unwrap(),
    ];
    assert_eq!(sources[0].page_count(), 3);
    let picks = [
        pick(1, 0, 0),
        pick(0, 2, 0),
        pick(0, 0, 90),
        // 原来就转了 90°（继承来的），再转 90° 就是 180°。
        pick(0, 1, 90),
        pick(1, 1, -90),
        // 同一页可以出现两次。
        pick(0, 0, 0),
    ];
    let report = assemble(&sources, &picks).unwrap();
    let pdf = report.value;
    assert_eq!(
        layout(&pdf),
        [
            (300, 0),
            (120, 0),
            (200, 90),
            (200, 180),
            (310, 270),
            (200, 0)
        ]
    );

    // 资源原本写在上级节点里，换了页树以后每页照样画得出黑框。
    let doc = render::Document::open(pdf).unwrap();
    for i in 0..doc.page_count() {
        let img = doc.render(i, 1.0, &doc.cache());
        let ink = img.pixels().filter(|p| p.0[0] < 60).count();
        assert!(ink >= 40 * 40, "第 {} 页是空白的（{ink} 个黑点）", i + 1);
    }

    // 书签属于整份文档，带不过来，要说一声。
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.kind == WarningKind::UnsupportedElement && w.detail.contains("书签")),
        "{:?}",
        report.warnings
    );
}

/// 只取一页：别的页不能顺着链接、页树被整个带进新文件。
#[test]
fn pages_not_picked_stay_behind() {
    let sources = [Source::open(&plain()).unwrap()];
    let report = assemble(&sources, &[pick(0, 0, 0)]).unwrap();
    let pdf = Document::load_mem(&report.value).unwrap();
    let mut contents = Vec::new();
    for id in pdf.get_pages().into_values() {
        contents.extend(pdf.get_page_content(id));
    }
    let text = String::from_utf8_lossy(&contents);
    assert!(text.contains("PAGE-B1"));
    let everything: Vec<u8> = pdf
        .objects
        .values()
        .filter_map(|o| o.as_stream().ok())
        .flat_map(|s| {
            s.decompressed_content()
                .unwrap_or_else(|_| s.content.clone())
        })
        .collect();
    assert!(
        !String::from_utf8_lossy(&everything).contains("PAGE-B2"),
        "没选的第二页被链接带进了新文件"
    );
    // 只剩这一页，没有别的 /Page 对象。
    let pages = pdf
        .objects
        .values()
        .filter(|o| {
            o.as_dict()
                .ok()
                .and_then(|d| d.get(b"Type").ok())
                .and_then(|t| t.as_name().ok())
                == Some(b"Page".as_ref())
        })
        .count();
    assert_eq!(pages, 1);
}

#[test]
fn bad_requests_and_signed_files_are_refused() {
    let sources = [Source::open(&inherited()).unwrap()];
    let err = |picks: &[PagePick]| match assemble(&sources, picks) {
        Err(CoreError::Unsupported(m)) => m,
        other => panic!("{:?}", other.map(|r| r.value.len())),
    };
    assert!(err(&[]).contains("没有选中"));
    assert!(err(&[pick(0, 3, 0)]).contains("没有第 4 页"));
    assert!(err(&[pick(0, 0, 45)]).contains("90°"));

    // 带数字签名的：一改签名就失效，不接。
    let mut doc = Document::load_mem(&inherited()).unwrap();
    doc.add_object(dictionary! { "Type" => "Sig", "Filter" => "Adobe.PPKLite" });
    let mut signed = Vec::new();
    doc.save_to(&mut signed).unwrap();
    assert!(matches!(
        Source::open(&signed),
        Err(CoreError::Unsupported(m)) if m.contains("数字签名")
    ));
}
