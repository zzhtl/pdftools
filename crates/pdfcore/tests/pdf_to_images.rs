//! PDF 转图片：尺寸、方向、选页、命名，以及没嵌字体的中文能不能画出来。

mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use pdf_writer::types::{CidFontType, FontFlags};
use pdf_writer::{Content, Finish, Name, Pdf, Rect, Ref, Str};
use pdfcore::fsio::OutputNamer;
use pdfcore::ops::pdf_to_images::{self, Format, Options};
use pdfcore::{CoreError, NoProgress, Progress, ProgressSink, WarningKind};

fn dir(name: &str) -> PathBuf {
    let d = common::tmp("pdf_to_images").join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 一页：宽高（点）、`/Rotate`、内容流。
struct PageSpec {
    size: (f32, f32),
    rotate: i32,
    content: Vec<u8>,
}

/// 往 PDF 里写一个字体对象（给定对象号）。
type WriteFont<'a> = &'a dyn Fn(&mut Pdf, Ref);

/// 把几页拼成 PDF。`font` 给了的话，每页都挂上这个字体资源（名字 F1）。
fn build(pages: &[PageSpec], font: Option<WriteFont>) -> Vec<u8> {
    let mut pdf = Pdf::new();
    let (catalog, tree, font_id) = (Ref::new(1), Ref::new(2), Ref::new(3));
    pdf.catalog(catalog).pages(tree);
    let ids: Vec<(Ref, Ref)> = (0..pages.len() as i32)
        .map(|i| (Ref::new(10 + 2 * i), Ref::new(11 + 2 * i)))
        .collect();
    pdf.pages(tree)
        .kids(ids.iter().map(|(p, _)| *p))
        .count(pages.len() as i32);
    for (spec, (page_id, content_id)) in pages.iter().zip(&ids) {
        let mut page = pdf.page(*page_id);
        page.media_box(Rect::new(0.0, 0.0, spec.size.0, spec.size.1))
            .parent(tree)
            .contents(*content_id);
        if spec.rotate != 0 {
            page.rotate(spec.rotate);
        }
        if font.is_some() {
            page.resources().fonts().pair(Name(b"F1"), font_id);
        }
        page.finish();
        pdf.stream(*content_id, &spec.content);
    }
    if let Some(write_font) = font {
        write_font(&mut pdf, font_id);
    }
    pdf.finish()
}

/// 黑色方块，`(x, y)` 是左下角（PDF 坐标）。
fn square(x: f32, y: f32, side: f32) -> Vec<u8> {
    let mut c = Content::new();
    c.set_fill_gray(0.0);
    c.rect(x, y, side, side);
    c.fill_nonzero();
    c.finish().to_vec()
}

/// 同 [`square`]，红色。
fn red_square(x: f32, y: f32, side: f32) -> Vec<u8> {
    let mut c = Content::new();
    c.set_fill_rgb(1.0, 0.0, 0.0);
    c.rect(x, y, side, side);
    c.fill_nonzero();
    c.finish().to_vec()
}

fn shapes(dir: &Path) -> PathBuf {
    let pdf = build(
        &[
            // 竖页，方块在左上角。
            PageSpec {
                size: (200.0, 300.0),
                rotate: 0,
                content: square(20.0, 230.0, 50.0),
            },
            // 横页，方块在右下角；左上角还有一块红的。
            PageSpec {
                size: (300.0, 200.0),
                rotate: 0,
                content: [square(230.0, 20.0, 50.0), red_square(20.0, 150.0, 30.0)].join(&b'\n'),
            },
            // 与第一页一样，但顺时针转 90° 显示：左上角的方块转到右上角。
            PageSpec {
                size: (200.0, 300.0),
                rotate: 90,
                content: square(20.0, 230.0, 50.0),
            },
        ],
        None,
    );
    let path = dir.join("shapes.pdf");
    std::fs::write(&path, pdf).unwrap();
    path
}

fn dark(img: &image::RgbImage, x: u32, y: u32) -> bool {
    img.get_pixel(x, y).0.iter().all(|&v| v < 60)
}

#[test]
fn size_and_orientation_follow_the_page() {
    let d = dir("orientation");
    let pdf = shapes(&d);
    let out = d.join("out");
    let opts = Options {
        dpi: 144.0,
        format: Format::Png,
        pages: String::new(),
    };
    let mut namer = OutputNamer::new([&pdf]);
    let report = pdf_to_images::run(&pdf, &out, &opts, &mut namer, &NoProgress).unwrap();
    let names: Vec<String> = report
        .value
        .files
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        ["shapes_p001.png", "shapes_p002.png", "shapes_p003.png"]
    );
    assert_eq!(report.value.page_count, 3);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);

    let img: Vec<image::RgbImage> = report
        .value
        .files
        .iter()
        .map(|p| image::open(p).unwrap().to_rgb8())
        .collect();
    // 144 DPI：1 点 2 像素。
    assert_eq!(img[0].dimensions(), (400, 600));
    assert_eq!(img[1].dimensions(), (600, 400));
    assert_eq!(img[2].dimensions(), (600, 400));
    // 方块中心离所在两条边各 45 点。
    assert!(dark(&img[0], 90, 90) && !dark(&img[0], 200, 300));
    assert!(dark(&img[1], 510, 310) && !dark(&img[1], 90, 90));
    assert!(dark(&img[2], 510, 90) && !dark(&img[2], 90, 90));
    assert_eq!(img[1].get_pixel(70, 70).0, [255, 0, 0]);
    // 只有黑白灰的页存成单通道（无损，体积小一半）；有颜色的照旧 RGB。
    let color = |i: usize| image::open(&report.value.files[i]).unwrap().color();
    assert_eq!(color(0), image::ColorType::L8);
    assert_eq!(color(1), image::ColorType::Rgb8);
}

#[test]
fn chosen_pages_as_jpeg_never_overwrite() {
    let d = dir("chosen");
    let pdf = shapes(&d);
    // 目录里已经有一张同名的：不覆盖，改名。
    std::fs::write(d.join("shapes_p001.jpg"), b"old").unwrap();
    let opts = Options {
        dpi: 72.0,
        format: Format::Jpeg,
        pages: "3，1".into(),
    };
    let mut namer = OutputNamer::new([&pdf]);
    let report = pdf_to_images::run(&pdf, &d, &opts, &mut namer, &NoProgress).unwrap();
    assert_eq!(
        report.value.files,
        [d.join("shapes_p003.jpg"), d.join("shapes_p001 (2).jpg")]
    );
    assert_eq!(std::fs::read(d.join("shapes_p001.jpg")).unwrap(), b"old");
    let third = image::open(&report.value.files[0]).unwrap();
    assert_eq!((third.width(), third.height()), (300, 200));

    let opts = Options {
        pages: "4".into(),
        ..opts
    };
    let err = pdf_to_images::run(&pdf, &d, &opts, &mut namer, &NoProgress).err();
    assert!(
        matches!(&err, Some(CoreError::Unsupported(m)) if m.contains("没有第 4 页")),
        "{err:?}"
    );
}

/// 公文 PDF 常见：字体只写了个「SimSun」，没嵌进去，用预定义的 UCS-2 编码。
/// 要到系统里找中文字体来画，不然 hayro 只有西文标准字体，汉字全是空白。
#[test]
fn fonts_not_embedded_are_found_on_the_system() {
    if !common::require_cjk_font() {
        return;
    }
    let d = dir("cjk");
    // 「中文字形」的 UCS-2 编码。
    let text: Vec<u8> = "中文字形"
        .encode_utf16()
        .flat_map(|u| u.to_be_bytes())
        .collect();
    let mut c = Content::new();
    c.begin_text();
    c.set_font(Name(b"F1"), 48.0);
    c.next_line(20.0, 30.0);
    c.show(Str(&text));
    c.end_text();
    let simsun = |pdf: &mut Pdf, id: Ref| {
        let (cid, desc) = (Ref::new(4), Ref::new(5));
        pdf.type0_font(id)
            .base_font(Name(b"SimSun"))
            .encoding_predefined(Name(b"UniGB-UCS2-H"))
            .descendant_font(cid);
        pdf.cid_font(cid)
            .subtype(CidFontType::Type0)
            .base_font(Name(b"SimSun"))
            .system_info(pdf_writer::types::SystemInfo {
                registry: Str(b"Adobe"),
                ordering: Str(b"GB1"),
                supplement: 2,
            })
            .font_descriptor(desc)
            .default_width(1000.0);
        pdf.font_descriptor(desc)
            .name(Name(b"SimSun"))
            .flags(FontFlags::SYMBOLIC)
            .bbox(Rect::new(0.0, -140.0, 1000.0, 860.0))
            .italic_angle(0.0)
            .ascent(860.0)
            .descent(-140.0)
            .cap_height(700.0)
            .stem_v(80.0);
    };
    let pdf = build(
        &[PageSpec {
            size: (240.0, 100.0),
            rotate: 0,
            content: c.finish().to_vec(),
        }],
        Some(&simsun),
    );
    let path = d.join("simsun.pdf");
    std::fs::write(&path, pdf).unwrap();

    let opts = Options {
        dpi: 72.0,
        format: Format::Png,
        pages: String::new(),
    };
    let mut namer = OutputNamer::new([&path]);
    let report = pdf_to_images::run(&path, &d, &opts, &mut namer, &NoProgress).unwrap();
    let img = image::open(&report.value.files[0]).unwrap().to_luma8();
    // 四个字各占 48×48 的格子，每格都要有像样的笔画。
    for k in 0..4 {
        let x0 = 20 + 48 * k;
        let ink = (x0..x0 + 48)
            .flat_map(|x| (22..70).map(move |y| (x, y)))
            .filter(|&(x, y)| img.get_pixel(x, y).0[0] < 128)
            .count();
        assert!(ink > 48 * 48 / 20, "第 {} 个字几乎没有笔画：{ink}", k + 1);
    }
    // 本机没有 SimSun 时要说清楚换成了什么。
    let has_simsun = pdfcore::fonts::system::SystemFonts::shared()
        .by_postscript_name("SimSun")
        .is_some();
    if !has_simsun {
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.kind == WarningKind::FontSubstituted && w.detail.contains("SimSun")),
            "{:?}",
            report.warnings
        );
    }
}

struct CancelAfterFirst(AtomicBool);

impl ProgressSink for CancelAfterFirst {
    fn emit(&self, progress: Progress) {
        if matches!(progress, Progress::Item { .. }) {
            self.0.store(true, Ordering::Relaxed);
        }
    }
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

#[test]
fn cancelling_stops_rendering() {
    let d = dir("cancel");
    let many: Vec<PageSpec> = (0..40)
        .map(|_| PageSpec {
            size: (200.0, 300.0),
            rotate: 0,
            content: square(20.0, 20.0, 50.0),
        })
        .collect();
    let path = d.join("many.pdf");
    std::fs::write(&path, build(&many, None)).unwrap();
    let opts = Options {
        dpi: 72.0,
        format: Format::Png,
        pages: String::new(),
    };
    let sink = CancelAfterFirst(AtomicBool::new(false));
    let mut namer = OutputNamer::new([&path]);
    let result = pdf_to_images::run(&path, &d.join("out"), &opts, &mut namer, &sink);
    assert!(matches!(result, Err(CoreError::Cancelled)));
    let written = std::fs::read_dir(d.join("out")).unwrap().count();
    assert!(written < 40, "取消以后还画完了全部 {written} 页");
}
