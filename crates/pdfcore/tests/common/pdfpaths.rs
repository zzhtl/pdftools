//! 从 PDF 内容流里取出画出来的线与面：量段落边框、底纹、表格框线用。
//!
//! 只关心「画在哪、什么颜色、多粗」：每个被填充或描边的路径给出外框（曲线按控制点
//! 算），坐标已乘上 CTM，原点在页面左下角。不展开 Form XObject —— 我们与
//! LibreOffice 都直接画在页面内容流里。
//!
//! 本文件只依赖 lopdf，也被 `examples/pdfdump.rs` 通过 `#[path]` 引用。

use lopdf::Document;

#[derive(Debug, Clone, PartialEq)]
pub struct Path {
    /// 描边（线）还是填充（面）。
    pub stroke: bool,
    /// 外框 `[x0, y0, x1, y1]`，x0 ≤ x1、y0 ≤ y1。
    pub bbox: [f32; 4],
    /// 颜色，RGB 各 0–1。灰度与 CMYK 已换算。
    pub color: [f32; 3],
    /// 描边的线宽（按 CTM 的横向缩放折算）；填充时为 0。
    pub width: f32,
    /// 描边用了虚线样式。
    pub dashed: bool,
}

impl Path {
    pub fn w(&self) -> f32 {
        self.bbox[2] - self.bbox[0]
    }
    pub fn h(&self) -> f32 {
        self.bbox[3] - self.bbox[1]
    }
}

const IDENTITY: [f32; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

fn mul(m: [f32; 6], n: [f32; 6]) -> [f32; 6] {
    [
        m[0] * n[0] + m[1] * n[2],
        m[0] * n[1] + m[1] * n[3],
        m[2] * n[0] + m[3] * n[2],
        m[2] * n[1] + m[3] * n[3],
        m[4] * n[0] + m[5] * n[2] + n[4],
        m[4] * n[1] + m[5] * n[3] + n[5],
    ]
}

#[derive(Clone)]
struct Gs {
    ctm: [f32; 6],
    fill: [f32; 3],
    stroke: [f32; 3],
    width: f32,
    dashed: bool,
}

fn cmyk([c, m, y, k]: [f32; 4]) -> [f32; 3] {
    [
        (1.0 - c) * (1.0 - k),
        (1.0 - m) * (1.0 - k),
        (1.0 - y) * (1.0 - k),
    ]
}

/// 按操作数个数猜颜色空间：1 灰度、3 RGB、4 CMYK。`sc`/`scn` 这样处理对我们与
/// LibreOffice 的输出都够用（没有专色、图案）。
fn color(v: &[f32]) -> Option<[f32; 3]> {
    match v {
        [g] => Some([*g, *g, *g]),
        [r, g, b] => Some([*r, *g, *b]),
        [c, m, y, k] => Some(cmyk([*c, *m, *y, *k])),
        _ => None,
    }
}

/// 每页画出来的路径，按内容流里的顺序。
pub fn extract(pdf: &[u8]) -> Vec<Vec<Path>> {
    let doc = Document::load_mem(pdf).expect("PDF 无法解析");
    doc.get_pages()
        .values()
        .map(|&id| {
            let content = doc.get_and_decode_page_content(id).expect("内容流无法解码");
            page_paths(&content.operations)
        })
        .collect()
}

fn page_paths(ops: &[lopdf::content::Operation]) -> Vec<Path> {
    let mut gs = Gs {
        ctm: IDENTITY,
        fill: [0.0; 3],
        stroke: [0.0; 3],
        width: 1.0,
        dashed: false,
    };
    let mut stack: Vec<Gs> = Vec::new();
    // 当前路径上的点（已变换到页面坐标）。
    let mut points: Vec<(f32, f32)> = Vec::new();
    let mut out = Vec::new();

    for op in ops {
        let nums: Vec<f32> = op
            .operands
            .iter()
            .filter_map(|o| o.as_float().ok())
            .collect();
        let n = |i: usize| nums.get(i).copied().unwrap_or(0.0);
        let mut add = |x: f32, y: f32, ctm: [f32; 6]| {
            points.push((
                ctm[0] * x + ctm[2] * y + ctm[4],
                ctm[1] * x + ctm[3] * y + ctm[5],
            ));
        };
        match op.operator.as_str() {
            "q" => stack.push(gs.clone()),
            "Q" => {
                if let Some(g) = stack.pop() {
                    gs = g;
                }
            }
            "cm" => gs.ctm = mul([n(0), n(1), n(2), n(3), n(4), n(5)], gs.ctm),
            "w" => gs.width = n(0),
            "d" => {
                gs.dashed = op
                    .operands
                    .first()
                    .and_then(|o| o.as_array().ok())
                    .is_some_and(|a| !a.is_empty());
            }
            "g" | "rg" | "k" | "sc" | "scn" => {
                if let Some(c) = color(&nums) {
                    gs.fill = c;
                }
            }
            "G" | "RG" | "K" | "SC" | "SCN" => {
                if let Some(c) = color(&nums) {
                    gs.stroke = c;
                }
            }
            "m" | "l" => add(n(0), n(1), gs.ctm),
            "c" => {
                for i in 0..3 {
                    add(n(2 * i), n(2 * i + 1), gs.ctm);
                }
            }
            "v" | "y" => {
                for i in 0..2 {
                    add(n(2 * i), n(2 * i + 1), gs.ctm);
                }
            }
            "re" => {
                let (x, y, w, h) = (n(0), n(1), n(2), n(3));
                for (px, py) in [(x, y), (x + w, y), (x + w, y + h), (x, y + h)] {
                    add(px, py, gs.ctm);
                }
            }
            "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" | "n" => {
                let paint = op.operator.as_str();
                if !points.is_empty() && paint != "n" {
                    let bbox = points
                        .iter()
                        .fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, &(x, y)| {
                            [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)]
                        });
                    let fills = !matches!(paint, "S" | "s");
                    let strokes = matches!(paint, "S" | "s" | "B" | "B*" | "b" | "b*");
                    if fills {
                        out.push(Path {
                            stroke: false,
                            bbox,
                            color: gs.fill,
                            width: 0.0,
                            dashed: false,
                        });
                    }
                    if strokes {
                        let scale = (gs.ctm[0].powi(2) + gs.ctm[1].powi(2)).sqrt();
                        out.push(Path {
                            stroke: true,
                            bbox,
                            color: gs.stroke,
                            width: gs.width * scale,
                            dashed: gs.dashed,
                        });
                    }
                }
                points.clear();
            }
            _ => {}
        }
    }
    out
}

/// 调试 dump：一条路径一行。
pub fn dump(pages: &[Vec<Path>]) -> String {
    let mut s = String::new();
    for (i, paths) in pages.iter().enumerate() {
        for p in paths {
            let [x0, y0, x1, y1] = p.bbox;
            let [r, g, b] = p.color;
            s.push_str(&format!(
                "p{} {} [{x0:.2} {y0:.2} {x1:.2} {y1:.2}] rgb({r:.2},{g:.2},{b:.2}){}{}\n",
                i + 1,
                if p.stroke { "stroke" } else { "fill" },
                if p.stroke {
                    format!(" w={:.2}", p.width)
                } else {
                    String::new()
                },
                if p.dashed { " dashed" } else { "" },
            ));
        }
    }
    s
}
