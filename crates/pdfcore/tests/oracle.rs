//! 与 LibreOffice 的排版比对。开发机专用：CI 上没有 LibreOffice，这些用例都标了 `#[ignore]`。
//!
//! ```text
//! # 真实语料闸门（相对基线：页数差、分页漂移、基线位置都不得劣化，排版 dump 不得变化）
//! PDFTOOLS_CORPUS=/path/to/docx-dir cargo test -p pdfcore --test oracle corpus -- --ignored --nocapture
//! # 记录新基线（校准步显式重新定基线时用）
//! PDFTOOLS_ORACLE_BLESS=1 PDFTOOLS_CORPUS=... cargo test -p pdfcore --test oracle corpus -- --ignored --nocapture
//! # 构造探针
//! cargo test -p pdfcore --test oracle probes -- --ignored --nocapture
//! ```
//!
//! 产物（PDF、dump、基线）默认写到 `~/.cache/pdftools-oracle/<语料目录名>/`，可用
//! `PDFTOOLS_ORACLE_OUT` 改。**不放进仓库**：真实语料里有身份证号、电话这类个人信息。

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use common::lo::Lo;
use common::metrics::{compare, same_layout, Compare};
use common::pdftext::{dump, extract, PageText};
use common::raster::masks;
use pdfcore::ops::docx_to_pdf;
use pdfcore::NoProgress;

fn out_root() -> PathBuf {
    if let Some(p) = std::env::var_os("PDFTOOLS_ORACLE_OUT") {
        return PathBuf::from(p);
    }
    match std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        Some(h) => PathBuf::from(h).join(".cache").join("pdftools-oracle"),
        None => common::tmp("oracle"),
    }
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v == "1")
}

/// 渲染比较慢，`PDFTOOLS_ORACLE_RASTER=0` 可以跳过 RI。
fn want_raster() -> bool {
    std::env::var("PDFTOOLS_ORACLE_RASTER").map_or(true, |v| v != "0")
}

struct Case {
    name: String,
    ours: Vec<u8>,
    reference: Vec<u8>,
}

struct Row {
    name: String,
    cmp: Compare,
    ours_pages: Vec<PageText>,
}

fn evaluate(cases: &[Case], out: &Path) -> Vec<Row> {
    std::fs::create_dir_all(out).unwrap();
    let raster = want_raster();
    cases
        .iter()
        .map(|c| {
            let (po, pr) = (extract(&c.ours), extract(&c.reference));
            let m = raster.then(|| (masks(&c.ours), masks(&c.reference)));
            let cmp = compare(
                &po,
                &pr,
                m.as_ref().map(|(a, b)| (a.as_slice(), b.as_slice())),
            );
            std::fs::write(out.join(format!("{}.ours.pdf", c.name)), &c.ours).unwrap();
            std::fs::write(out.join(format!("{}.ref.pdf", c.name)), &c.reference).unwrap();
            std::fs::write(out.join(format!("{}.ours.dump", c.name)), dump(&po)).unwrap();
            std::fs::write(out.join(format!("{}.ref.dump", c.name)), dump(&pr)).unwrap();
            Row {
                name: c.name.clone(),
                cmp,
                ours_pages: po,
            }
        })
        .collect()
}

fn print_table(title: &str, rows: &[Row], lo: &Lo) {
    println!("\n== {title}（LibreOffice 参照，locale {}）", lo.locale());
    println!("{}", Compare::header());
    for r in rows {
        println!("{}", r.cmp.row(&r.name));
    }
}

fn convert_ours(path: &Path) -> Vec<u8> {
    docx_to_pdf::run(path, &NoProgress)
        .unwrap_or_else(|e| panic!("{} 转换失败：{e}", path.display()))
        .value
        .pdf
}

// ---------------------------------------------------------------- 基线

/// 基线文件的一行：名字、我们的页数、参照页数、分页漂移、LY 中位数、RI 均值。
#[derive(Debug, Clone)]
struct Base {
    pages: usize,
    pages_ref: usize,
    bd_max: usize,
    bd_sum: usize,
    ly_median: f32,
    ri_mean: f64,
}

fn load_baseline(dir: &Path) -> Option<HashMap<String, Base>> {
    let text = std::fs::read_to_string(dir.join("baseline.tsv")).ok()?;
    let mut out = HashMap::new();
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 7 {
            continue;
        }
        out.insert(
            f[0].to_string(),
            Base {
                pages: f[1].parse().ok()?,
                pages_ref: f[2].parse().ok()?,
                bd_max: f[3].parse().ok()?,
                bd_sum: f[4].parse().ok()?,
                ly_median: f[5].parse().ok()?,
                ri_mean: f[6].parse().unwrap_or(f64::NAN),
            },
        );
    }
    Some(out)
}

fn write_baseline(dir: &Path, rows: &[Row], lo: &Lo) {
    let base = dir.join("baseline");
    std::fs::create_dir_all(&base).unwrap();
    let mut s = format!(
        "# pdftools oracle 基线；LibreOffice locale {}\n# 名字\t页数\t参照页数\tBD最大\tBD总和\tLY中位数\tRI均值\n",
        lo.locale()
    );
    for r in rows {
        s.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{:.4}\t{:.4}\n",
            r.name,
            r.cmp.pages_ours,
            r.cmp.pages_ref,
            r.cmp.bd_max,
            r.cmp.bd_sum,
            r.cmp.ly_median,
            r.cmp.ri_mean
        ));
        std::fs::copy(
            dir.join(format!("{}.ours.pdf", r.name)),
            base.join(format!("{}.ours.pdf", r.name)),
        )
        .unwrap();
    }
    std::fs::write(dir.join("baseline.tsv"), s).unwrap();
    println!("已写入基线：{}", dir.join("baseline.tsv").display());
}

/// 与基线比：返回劣化项。排版 dump 与基线不同也算（除非是校准步显式重新定基线）。
fn regressions(dir: &Path, rows: &[Row], base: &HashMap<String, Base>) -> Vec<String> {
    let mut bad = Vec::new();
    for r in rows {
        let Some(b) = base.get(&r.name) else {
            bad.push(format!("{}：基线里没有这份文档", r.name));
            continue;
        };
        let c = &r.cmp;
        // 页数差不能比基线大。基线本身可以带着已知的不一致（比如 21 vs 20），
        // 它是「现状」而不是「理想」；往参照靠拢的变化照样会被下面的 dump 比对拦住，
        // 需要显式重新定基线，这样每一次变化都是被看见过的。
        let (was, now) = (
            b.pages.abs_diff(b.pages_ref),
            c.pages_ours.abs_diff(c.pages_ref),
        );
        if now > was {
            bad.push(format!(
                "{}：页数 {}（参照 {}）→ {}（参照 {}）",
                r.name, b.pages, b.pages_ref, c.pages_ours, c.pages_ref
            ));
        }
        if c.bd_max > b.bd_max || c.bd_sum > b.bd_sum {
            bad.push(format!(
                "{}：分页漂移劣化 max {}→{}，sum {}→{}",
                r.name, b.bd_max, c.bd_max, b.bd_sum, c.bd_sum
            ));
        }
        if c.ly_median > b.ly_median + 0.2 {
            bad.push(format!(
                "{}：基线位置中位差 {:.2} → {:.2}",
                r.name, b.ly_median, c.ly_median
            ));
        }
        if !b.ri_mean.is_nan() && !c.ri_mean.is_nan() && c.ri_mean < b.ri_mean - 0.02 {
            // 渲染重合度只提示不拦：字形细节（合成粗体等）的变化会让它小幅波动。
            println!(
                "提示 {}：墨迹重合度 {:.3} → {:.3}",
                r.name, b.ri_mean, c.ri_mean
            );
        }
        let base_pdf = dir.join("baseline").join(format!("{}.ours.pdf", r.name));
        if let Ok(bytes) = std::fs::read(&base_pdf) {
            if let Err(e) = same_layout(&extract(&bytes), &r.ours_pages, 1e-3) {
                bad.push(format!("{}：排版与基线不同 —— {e}", r.name));
            }
        }
    }
    bad
}

// ---------------------------------------------------------------- 用例

#[test]
#[ignore = "需要本机 LibreOffice 与 PDFTOOLS_CORPUS"]
fn corpus() {
    let Some(dir) = std::env::var_os("PDFTOOLS_CORPUS").map(PathBuf::from) else {
        eprintln!("跳过：没有设置 PDFTOOLS_CORPUS");
        return;
    };
    let Some(lo) = Lo::find() else {
        eprintln!("跳过：本机没有 soffice");
        return;
    };

    let mut docs: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("docx"))
        })
        .collect();
    docs.sort();
    assert!(!docs.is_empty(), "{} 里没有 .docx", dir.display());

    let refs = lo.convert(&docs).expect("LibreOffice 转换失败");
    let cases: Vec<Case> = docs
        .iter()
        .zip(&refs)
        .map(|(d, r)| Case {
            name: d.file_stem().unwrap().to_string_lossy().into_owned(),
            ours: convert_ours(d),
            reference: std::fs::read(r).unwrap(),
        })
        .collect();

    let corpus_name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "corpus".into());
    let out = out_root().join(corpus_name);
    let rows = evaluate(&cases, &out);
    print_table("真实语料", &rows, &lo);
    println!("产物目录：{}", out.display());

    let pc_ok = rows
        .iter()
        .filter(|r| r.cmp.pages_ours == r.cmp.pages_ref)
        .count();
    println!("页数与参照一致：{pc_ok}/{}", rows.len());

    if env_flag("PDFTOOLS_ORACLE_BLESS") {
        write_baseline(&out, &rows, &lo);
    } else if let Some(base) = load_baseline(&out) {
        let bad = regressions(&out, &rows, &base);
        assert!(bad.is_empty(), "相对基线有劣化：\n{}", bad.join("\n"));
    } else {
        println!("还没有基线；确认结果无误后用 PDFTOOLS_ORACLE_BLESS=1 记录。");
    }
}

#[test]
#[ignore = "需要本机 LibreOffice"]
fn probes() {
    let Some(lo) = Lo::find() else {
        eprintln!("跳过：本机没有 soffice");
        return;
    };
    let probes = common::probes::all();
    let paths: Vec<PathBuf> = probes
        .iter()
        .map(|p| p.doc.build(&format!("probe_{}.docx", p.name)))
        .collect();
    let refs = lo.convert(&paths).expect("LibreOffice 转换失败");
    let cases: Vec<Case> = probes
        .iter()
        .zip(paths.iter().zip(&refs))
        .map(|(p, (d, r))| Case {
            name: p.name.to_string(),
            ours: convert_ours(d),
            reference: std::fs::read(r).unwrap(),
        })
        .collect();
    let out = out_root().join("probes");
    let rows = evaluate(&cases, &out);
    print_table("构造探针", &rows, &lo);
    println!("产物目录：{}", out.display());
}
