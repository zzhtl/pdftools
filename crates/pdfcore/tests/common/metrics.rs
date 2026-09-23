//! 我们的 PDF 与参照 PDF（LibreOffice）之间的比对指标。
//!
//! - **PC**：页数。
//! - **TX**：逐页文字（去空白）是否一致。
//! - **BD**：分页边界漂移，单位是「行」。按参照每页开头 12 个字在我们的全文里定位，
//!   再数我们的分页点离那里差几行。页数一致但分页点整体错位，只有它能看出来。
//! - **LY / LX**：两边文字相同、且落在同一页的行，基线（距页顶）与行首 x 的差。
//! - **RI**：72dpi 渲染后墨迹掩码的 IoU。表格线、图片这些文字比对看不到的东西靠它。
//! - 字体一致性：两边实际用的字体不同时，LY/LX 的可比性要打折扣，报告里标出来。

use std::collections::BTreeSet;

use super::pdftext::{norm, PageText};
use super::raster::{iou, Mask};

#[derive(Debug, Clone, Default)]
pub struct Compare {
    pub pages_ours: usize,
    pub pages_ref: usize,
    pub text_equal: bool,
    pub text_page_diff: usize,
    pub bd_max: usize,
    pub bd_sum: usize,
    /// 参照里定位不到的分页点（文字对不上）。
    pub bd_unmatched: usize,
    pub ly_median: f32,
    pub ly_max: f32,
    pub lx_median: f32,
    pub lx_max: f32,
    /// 文字匹配上的行数 / 参照的行数（只计两个字以上的行）。
    pub matched: usize,
    pub ref_lines: usize,
    pub ri_mean: f64,
    pub ri_min: f64,
    pub fonts_ours: BTreeSet<String>,
    pub fonts_ref: BTreeSet<String>,
}

impl Compare {
    pub fn font_parity(&self) -> bool {
        self.fonts_ours == self.fonts_ref
    }

    pub fn header() -> String {
        format!(
            "{:<28} {:>7} {:>4} {:>4} {:>7} {:>6} {:>6} {:>6} {:>6} {:>9} {:>6} {:>6} {}",
            "文档",
            "PC",
            "TX",
            "BDx",
            "BDsum",
            "LYmed",
            "LYmax",
            "LXmed",
            "LXmax",
            "匹配行",
            "RI",
            "RImin",
            "字体"
        )
    }

    pub fn row(&self, name: &str) -> String {
        let pc = format!("{}/{}", self.pages_ours, self.pages_ref);
        let tx = if self.text_equal {
            "=".to_string()
        } else {
            format!("≠{}", self.text_page_diff)
        };
        let bd_max = if self.bd_unmatched > 0 {
            format!("{}?{}", self.bd_max, self.bd_unmatched)
        } else {
            self.bd_max.to_string()
        };
        format!(
            "{:<28} {:>7} {:>4} {:>4} {:>7} {:>6.2} {:>6.2} {:>6.2} {:>6.2} {:>9} {:>6.3} {:>6.3} {}",
            truncate(name, 28),
            pc,
            tx,
            bd_max,
            self.bd_sum,
            self.ly_median,
            self.ly_max,
            self.lx_median,
            self.lx_max,
            format!("{}/{}", self.matched, self.ref_lines),
            self.ri_mean,
            self.ri_min,
            if self.font_parity() {
                "一致".to_string()
            } else {
                format!(
                    "不一致 ours={:?} ref={:?}",
                    self.fonts_ours, self.fonts_ref
                )
            }
        )
    }
}

fn truncate(s: &str, n: usize) -> String {
    let c: Vec<char> = s.chars().collect();
    if c.len() <= n {
        s.to_string()
    } else {
        c[..n].iter().collect()
    }
}

/// 字体名规整：去掉大小写、标点、`Regular` 后缀，
/// 让「NotoSerifCJKsc-Regular」与「Noto Serif CJK SC」算作同一个。
pub fn font_key(name: &str) -> String {
    let s: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    s.strip_suffix("regular").map(str::to_string).unwrap_or(s)
}

struct Flat {
    text: Vec<char>,
    line_starts: Vec<usize>,
    page_starts: Vec<usize>,
}

fn flatten(pages: &[PageText]) -> Flat {
    let mut f = Flat {
        text: Vec::new(),
        line_starts: Vec::new(),
        page_starts: Vec::new(),
    };
    for p in pages {
        f.page_starts.push(f.text.len());
        for l in &p.lines {
            let t = norm(&l.text);
            if t.is_empty() {
                continue;
            }
            f.line_starts.push(f.text.len());
            f.text.extend(t.chars());
        }
    }
    f
}

fn find_all(hay: &[char], needle: &[char]) -> Vec<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return Vec::new();
    }
    (0..=hay.len() - needle.len())
        .filter(|&i| hay[i..i + needle.len()] == *needle)
        .collect()
}

fn median(mut v: Vec<f32>) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

/// `masks` 为 None 时不算 RI（渲染比较慢，可以跳过）。
pub fn compare(
    ours: &[PageText],
    reference: &[PageText],
    masks: Option<(&[Mask], &[Mask])>,
) -> Compare {
    let mut c = Compare {
        pages_ours: ours.len(),
        pages_ref: reference.len(),
        ..Default::default()
    };

    // ---- TX
    let (fo, fr) = (flatten(ours), flatten(reference));
    c.text_equal = fo.text == fr.text;
    c.text_page_diff = ours
        .iter()
        .zip(reference)
        .filter(|(a, b)| a.text() != b.text())
        .count()
        + ours.len().abs_diff(reference.len());

    // ---- BD
    for k in 1..reference.len() {
        let r_off = fr.page_starts[k];
        let snippet: Vec<char> = fr.text[r_off..(r_off + 12).min(fr.text.len())].to_vec();
        if snippet.is_empty() {
            continue;
        }
        let pos = if c.text_equal {
            Some(r_off)
        } else {
            let want = r_off as f64 * fo.text.len() as f64 / fr.text.len().max(1) as f64;
            find_all(&fo.text, &snippet).into_iter().min_by(|a, b| {
                (*a as f64 - want)
                    .abs()
                    .partial_cmp(&(*b as f64 - want).abs())
                    .unwrap()
            })
        };
        let Some(pos) = pos else {
            c.bd_unmatched += 1;
            continue;
        };
        let our_start = fo.page_starts.get(k).copied().unwrap_or(fo.text.len());
        let (lo, hi) = (pos.min(our_start), pos.max(our_start));
        let bd = fo
            .line_starts
            .iter()
            .filter(|&&l| l > lo && l <= hi)
            .count();
        c.bd_max = c.bd_max.max(bd);
        c.bd_sum += bd;
    }

    // ---- LY / LX：按文字顺序对齐两边的行
    struct L {
        page: usize,
        y_top: f32,
        x0: f32,
        text: String,
    }
    let lines = |pages: &[PageText]| -> Vec<L> {
        pages
            .iter()
            .enumerate()
            .flat_map(|(pi, p)| {
                p.lines.iter().map(move |l| L {
                    page: pi,
                    y_top: p.height - l.y,
                    x0: l.x0,
                    text: norm(&l.text),
                })
            })
            .filter(|l| l.text.chars().count() >= 2)
            .collect()
    };
    let (lo, lr) = (lines(ours), lines(reference));
    c.ref_lines = lr.len();
    let (mut dy, mut dx) = (Vec::new(), Vec::new());
    let mut j = 0usize;
    for r in &lr {
        let end = (j + 60).min(lo.len());
        if let Some(k) = (j..end).find(|&k| lo[k].text == r.text) {
            c.matched += 1;
            if lo[k].page == r.page {
                dy.push((lo[k].y_top - r.y_top).abs());
                dx.push((lo[k].x0 - r.x0).abs());
            }
            j = k + 1;
        }
    }
    c.ly_max = dy.iter().copied().fold(0.0, f32::max);
    c.lx_max = dx.iter().copied().fold(0.0, f32::max);
    c.ly_median = median(dy);
    c.lx_median = median(dx);

    // ---- RI
    if let Some((mo, mr)) = masks {
        let scores: Vec<f64> = mo.iter().zip(mr).map(|(a, b)| iou(a, b)).collect();
        c.ri_mean = if scores.is_empty() {
            1.0
        } else {
            scores.iter().sum::<f64>() / scores.len() as f64
        };
        c.ri_min = scores.iter().copied().fold(1.0, f64::min);
    } else {
        c.ri_mean = f64::NAN;
        c.ri_min = f64::NAN;
    }

    // ---- 字体
    let fonts = |pages: &[PageText]| -> BTreeSet<String> {
        pages
            .iter()
            .flat_map(|p| p.lines.iter().flat_map(|l| l.frags.iter()))
            .map(|f| font_key(&f.font))
            .collect()
    };
    c.fonts_ours = fonts(ours);
    c.fonts_ref = fonts(reference);
    c
}

/// 两份（通常都是我们自己产出的）PDF 的排版是否完全一致：页数、行数、
/// 每个片段的位置（容差 `tol`）、字体、字号、文字。返回第一处差异。
///
/// 引擎重写的第一步要求「行为零变化」，就靠它判定。
pub fn same_layout(a: &[PageText], b: &[PageText], tol: f32) -> Result<(), String> {
    if a.len() != b.len() {
        return Err(format!("页数不同：{} vs {}", a.len(), b.len()));
    }
    for (pi, (pa, pb)) in a.iter().zip(b).enumerate() {
        let p = pi + 1;
        if (pa.width - pb.width).abs() > tol || (pa.height - pb.height).abs() > tol {
            return Err(format!(
                "第 {p} 页尺寸不同：{}x{} vs {}x{}",
                pa.width, pa.height, pb.width, pb.height
            ));
        }
        if pa.lines.len() != pb.lines.len() {
            return Err(format!(
                "第 {p} 页行数不同：{} vs {}",
                pa.lines.len(),
                pb.lines.len()
            ));
        }
        for (li, (la, lb)) in pa.lines.iter().zip(&pb.lines).enumerate() {
            let at = format!("第 {p} 页第 {} 行", li + 1);
            if (la.y - lb.y).abs() > tol {
                return Err(format!("{at} 基线不同：{} vs {}", la.y, lb.y));
            }
            if la.frags.len() != lb.frags.len() {
                return Err(format!(
                    "{at} 片段数不同：{} vs {}（「{}」vs「{}」）",
                    la.frags.len(),
                    lb.frags.len(),
                    la.text,
                    lb.text
                ));
            }
            for (fa, fb) in la.frags.iter().zip(&lb.frags) {
                if (fa.x - fb.x).abs() > tol
                    || (fa.size - fb.size).abs() > tol
                    || fa.font != fb.font
                    || fa.text != fb.text
                {
                    return Err(format!(
                        "{at} 片段不同：x={} {} {} {:?} vs x={} {} {} {:?}",
                        fa.x, fa.font, fa.size, fa.text, fb.x, fb.font, fb.size, fb.text
                    ));
                }
            }
        }
    }
    Ok(())
}
