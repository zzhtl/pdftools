//! 一页的绘图面。
//!
//! 绘制方只管「在哪画什么」；这一页用到了哪些字体、图片、资源名叫什么，都由这里登记。
//! 手工记账的版本很容易漏写资源 —— 那种页面在 Chrome 里正常，在 Acrobat 里是空白。
//!
//! 每段文字、每个图形都包在 `q … Q` 里，状态不会泄漏到后面的绘制：
//! 早先字距（`Tc`）只在非零时设置、也不复位，结果下一行不该有字距的地方也带上了。

use std::collections::HashMap;

use pdf_writer::types::TextRenderingMode;
use pdf_writer::{Finish, Name, Ref, Str};

use super::font::EmbeddedFont;
use super::PageSpec;
use crate::fonts::{FontFace, ShapedGlyph};

/// 合成斜体的斜切量（tan 12°），与常见排版软件的「伪斜体」一致。
const SYNTHETIC_ITALIC_SKEW: f32 = 0.2126;
/// 合成粗体的描边宽度与字号之比：描边加在字形轮廓两侧，笔画各加粗半个线宽。
const SYNTHETIC_BOLD_STROKE: f32 = 1.0 / 30.0;

/// 一段待绘制的文字：同字体、同字号、同颜色，从某个基线点开始。
pub struct GlyphRun<'a> {
    pub font: &'a EmbeddedFont,
    pub face: &'a FontFace,
    pub glyphs: &'a [ShapedGlyph],
    /// 每个字形之后额外的推进（点）：字距、两端对齐分到这个字形的份额。
    /// 比字形少时，缺的按 0 算。
    pub extra_after: &'a [f32],
    pub size_pt: f32,
    /// 基线起点，PDF 用户空间（原点在左下角）。
    pub x_pt: f32,
    pub y_pt: f32,
    /// 基线上浮（上标为正、下标为负），点。
    pub rise_pt: f32,
    pub color: [u8; 3],
    pub synthetic_bold: bool,
    pub synthetic_italic: bool,
}

pub struct Canvas {
    spec: PageSpec,
    fonts: HashMap<Ref, String>,
    images: HashMap<Ref, String>,
}

fn rgb(c: [u8; 3]) -> (f32, f32, f32) {
    (
        c[0] as f32 / 255.0,
        c[1] as f32 / 255.0,
        c[2] as f32 / 255.0,
    )
}

impl Canvas {
    pub fn new(width_pt: f32, height_pt: f32) -> Self {
        Self {
            spec: PageSpec::new(width_pt, height_pt),
            fonts: HashMap::new(),
            images: HashMap::new(),
        }
    }

    fn font_name(&mut self, font: Ref) -> String {
        let n = self.fonts.len();
        let spec = &mut self.spec;
        self.fonts
            .entry(font)
            .or_insert_with(|| {
                let name = format!("F{n}");
                spec.fonts.push((name.clone(), font));
                name
            })
            .clone()
    }

    fn image_name(&mut self, image: Ref) -> String {
        let n = self.images.len();
        let spec = &mut self.spec;
        self.images
            .entry(image)
            .or_insert_with(|| {
                let name = format!("Im{n}");
                spec.images.push((name.clone(), image));
                name
            })
            .clone()
    }

    /// 画一段文字。
    ///
    /// `/W` 数组里写的是 hmtx 的步进，所以整形（GPOS）带来的差额、字距、两端对齐的
    /// 额外推进，全部用 `TJ` 数组里的数字表达 —— 不用 `Tc`/`Tw`：`Tw` 只作用于单字节
    /// 编码里的 0x20，对我们用的双字节 Identity-H 编码根本不起作用。
    pub fn glyphs(&mut self, run: &GlyphRun) {
        if run.glyphs.is_empty() {
            return;
        }
        let name = self.font_name(run.font.font_ref);
        let metrics = run.face.metrics();
        let (r, g, b) = rgb(run.color);
        let c = &mut self.spec.content;

        c.save_state();
        c.set_fill_rgb(r, g, b);
        if run.synthetic_bold {
            // 用与填充同色的描边把笔画加粗，字宽不变 —— Word 对没有粗体的字体也是这么做的。
            c.set_stroke_rgb(r, g, b);
            c.set_line_width(run.size_pt * SYNTHETIC_BOLD_STROKE);
        }
        c.begin_text();
        c.set_font(Name(name.as_bytes()), run.size_pt);
        if run.synthetic_bold {
            c.set_text_rendering_mode(TextRenderingMode::FillStroke);
        }
        if run.rise_pt != 0.0 {
            c.set_rise(run.rise_pt);
        }
        let skew = if run.synthetic_italic {
            SYNTHETIC_ITALIC_SKEW
        } else {
            0.0
        };
        // 直接设文本矩阵而不是用 Td 链式累加：每段自带绝对位置，
        // 混排时不必跨 Tf 切换去追踪累计步进。
        c.set_text_matrix([1.0, 0.0, skew, 1.0, run.x_pt, run.y_pt]);

        let mut positioned = c.show_positioned();
        let mut items = positioned.items();
        let mut pending: Vec<u8> = Vec::with_capacity(run.glyphs.len() * 2);
        for (i, glyph) in run.glyphs.iter().enumerate() {
            pending.extend_from_slice(&run.font.map.new_gid(glyph.gid).to_be_bytes());

            // hmtx 步进与整形步进的差额，就是 GPOS 的贡献（字体单位）。不到半个单位的
            // 舍入噪声不值得拆开 TJ。
            let gpos = glyph.x_advance as f32 - run.face.advance(glyph.gid) as f32;
            let gpos = if gpos.abs() > 0.5 {
                metrics.to_pdf_units(gpos)
            } else {
                0.0
            };
            let extra = run.extra_after.get(i).copied().unwrap_or(0.0) * 1000.0 / run.size_pt;
            let adjust = gpos + extra;
            if adjust.abs() > 1e-3 {
                items.show(Str(&pending));
                pending.clear();
                // TJ 的数字是「从当前位置减去」，单位是 1/1000 文本空间，
                // 所以要往右多移就得给负数。
                items.adjust(-adjust);
            }
        }
        if !pending.is_empty() {
            items.show(Str(&pending));
        }
        items.finish();
        positioned.finish();
        c.end_text();
        c.restore_state();
    }

    pub fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: [u8; 3]) {
        let (r, g, b) = rgb(color);
        let c = &mut self.spec.content;
        c.save_state();
        c.set_fill_rgb(r, g, b);
        c.rect(x, y, w, h);
        c.fill_nonzero();
        c.restore_state();
    }

    /// 画一条线。`dash` 为虚线的实/空长度序列（点）。
    pub fn stroke_line(
        &mut self,
        from: (f32, f32),
        to: (f32, f32),
        width: f32,
        color: [u8; 3],
        dash: Option<&[f32]>,
    ) {
        let (r, g, b) = rgb(color);
        let c = &mut self.spec.content;
        c.save_state();
        c.set_stroke_rgb(r, g, b);
        c.set_line_width(width);
        if let Some(d) = dash {
            c.set_dash_pattern(d.iter().copied(), 0.0);
        }
        c.move_to(from.0, from.1);
        c.line_to(to.0, to.1);
        c.stroke();
        c.restore_state();
    }

    /// 保存图形状态。与 [`restore`](Self::restore) 成对使用，中间可以 [`clip_rect`](Self::clip_rect)。
    pub fn save(&mut self) {
        self.spec.content.save_state();
    }

    pub fn restore(&mut self) {
        self.spec.content.restore_state();
    }

    /// 之后的绘制只在这个矩形里可见，直到对应的 [`restore`](Self::restore)。
    pub fn clip_rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        let c = &mut self.spec.content;
        c.rect(x, y, w, h);
        c.clip_nonzero();
        c.end_path();
    }

    /// 放一张图。image XObject 画在单位方块里，`matrix` 把它拉伸、摆到页面上。
    pub fn image(&mut self, image: Ref, matrix: [f32; 6]) {
        let name = self.image_name(image);
        let c = &mut self.spec.content;
        c.save_state();
        c.transform(matrix);
        c.x_object(Name(name.as_bytes()));
        c.restore_state();
    }

    /// 页面上一块可点击的区域，指向外部链接。`rect` 为 [x0, y0, x1, y1]。
    pub fn link(&mut self, rect: [f32; 4], uri: &str) {
        self.spec.links.push((rect, uri.to_string()));
    }

    pub fn finish(self) -> PageSpec {
        self.spec
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts::system::{SystemFonts, LATIN_SERIF_PREFERENCE};
    use crate::pdf::writer::{DocBuilder, ImageData, ImageEncoding};

    /// 找一个本机有的西文字体；没有就跳过（CI 上一定有 DejaVu 或 Liberation）。
    fn latin_face() -> Option<std::sync::Arc<FontFace>> {
        SystemFonts::shared()
            .find_embeddable(LATIN_SERIF_PREFERENCE, false, false)
            .or_else(|| SystemFonts::shared().find_embeddable(&["DejaVu Sans"], false, false))
            .map(|f| f.face)
    }

    fn page_ops(pdf: &[u8]) -> Vec<lopdf::content::Operation> {
        let doc = lopdf::Document::load_mem(pdf).unwrap();
        let page = *doc.get_pages().values().next().unwrap();
        doc.get_and_decode_page_content(page).unwrap().operations
    }

    fn render(bold: bool, italic: bool, extras: &[f32]) -> Option<Vec<u8>> {
        let face = latin_face()?;
        let run = crate::fonts::shape_run(&face, "Hi you", rustybuzz::script::LATIN);
        let used: std::collections::BTreeMap<u16, String> =
            run.glyphs.iter().map(|g| (g.gid, String::new())).collect();
        let mut doc = DocBuilder::new();
        let embedded = {
            let (pdf, alloc) = doc.parts();
            crate::pdf::writer::font::embed_font(pdf, alloc, &face, &used).unwrap()
        };
        let mut canvas = Canvas::new(200.0, 100.0);
        canvas.glyphs(&GlyphRun {
            font: &embedded,
            face: &face,
            glyphs: &run.glyphs,
            extra_after: extras,
            size_pt: 12.0,
            x_pt: 10.0,
            y_pt: 50.0,
            rise_pt: 0.0,
            color: [0, 0, 0],
            synthetic_bold: bold,
            synthetic_italic: italic,
        });
        doc.add_page(canvas.finish());
        Some(doc.finish().unwrap())
    }

    #[test]
    fn spacing_goes_into_tj_never_tc_or_tw() {
        let Some(pdf) = render(false, false, &[0.0, 0.0, 3.0]) else {
            return;
        };
        let ops = page_ops(&pdf);
        assert!(
            !ops.iter().any(|o| o.operator == "Tc" || o.operator == "Tw"),
            "不该再出现 Tc/Tw"
        );
        // 第三个字形之后 3pt 的额外推进 = -250（1/1000 字号）写在 TJ 里。
        let tj = ops.iter().find(|o| o.operator == "TJ").unwrap();
        let nums: Vec<f32> = tj.operands[0]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|o| o.as_float().ok())
            .collect();
        assert!(
            nums.iter().any(|n| (n + 250.0).abs() < 0.01),
            "TJ 里没有 -250：{nums:?}"
        );
        // 文字包在 q/Q 里，状态不会泄漏。
        assert_eq!(ops.first().unwrap().operator, "q");
        assert_eq!(ops.last().unwrap().operator, "Q");
    }

    #[test]
    fn synthetic_bold_strokes_and_synthetic_italic_skews() {
        let Some(pdf) = render(true, true, &[]) else {
            return;
        };
        let ops = page_ops(&pdf);
        let tr = ops
            .iter()
            .find(|o| o.operator == "Tr")
            .expect("合成粗体要设 Tr");
        assert_eq!(tr.operands[0].as_i64().unwrap(), 2, "Tr 2 = 填充加描边");
        assert!(ops.iter().any(|o| o.operator == "w"), "描边要设线宽");
        let tm = ops.iter().find(|o| o.operator == "Tm").unwrap();
        let skew = tm.operands[2].as_float().unwrap();
        assert!((skew - 0.2126).abs() < 1e-3, "合成斜体的斜切量 {skew}");

        let plain = page_ops(&render(false, false, &[]).unwrap());
        assert!(!plain.iter().any(|o| o.operator == "Tr"));
    }

    #[test]
    fn lines_images_and_links_are_written_with_their_resources() {
        let mut doc = DocBuilder::new();
        let pixels = [0u8, 128, 255, 64];
        let img = doc.add_image(&ImageData {
            width: 2,
            height: 2,
            gray: true,
            encoding: ImageEncoding::Raw(&pixels),
            alpha: None,
        });
        let mut canvas = Canvas::new(200.0, 100.0);
        canvas.stroke_line(
            (10.0, 10.0),
            (190.0, 10.0),
            0.5,
            [255, 0, 0],
            Some(&[2.0, 1.0]),
        );
        canvas.image(img, [50.0, 0.0, 0.0, 50.0, 20.0, 20.0]);
        canvas.link([10.0, 10.0, 60.0, 30.0], "https://example.com/a b");
        doc.add_page(canvas.finish());
        let pdf = doc.finish().unwrap();

        let ops = page_ops(&pdf);
        for op in ["RG", "w", "d", "m", "l", "S", "cm", "Do"] {
            assert!(ops.iter().any(|o| o.operator == op), "缺少 {op}");
        }
        let lo = lopdf::Document::load_mem(&pdf).unwrap();
        let page = *lo.get_pages().values().next().unwrap();
        let (res, _) = lo.get_page_resources(page).unwrap();
        let xobjects = res.unwrap().get(b"XObject").unwrap().as_dict().unwrap();
        assert_eq!(xobjects.len(), 1, "图片资源要登记在页面上");

        let annots = lo
            .get_dictionary(page)
            .unwrap()
            .get(b"Annots")
            .unwrap()
            .as_array()
            .unwrap();
        let annot = lo
            .get_dictionary(annots[0].as_reference().unwrap())
            .unwrap();
        let uri = annot
            .get(b"A")
            .unwrap()
            .as_dict()
            .unwrap()
            .get(b"URI")
            .unwrap()
            .as_str()
            .unwrap();
        // /URI 只能是 7 位 ASCII，空格要编码。
        assert_eq!(uri, b"https://example.com/a%20b");
    }
}
