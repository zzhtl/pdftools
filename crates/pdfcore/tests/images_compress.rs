//! 图片批量压缩。
//!
//! 这个功能承诺「不改动原文件」。最要紧的是输出永远不能落在任何一个原文件上 ——
//! 用户把输出目录选成源目录是很自然的事。

mod common;

use std::path::{Path, PathBuf};

use pdfcore::imaging::Tier;
use pdfcore::ops::images_compress;
use pdfcore::NoProgress;

fn fresh_dir(name: &str) -> PathBuf {
    let d = common::tmp("img_compress").join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_photo_jpeg(path: &Path, w: u32, h: u32, q: u8) {
    let img = image::DynamicImage::ImageRgb8(common::images::photo(w, h));
    std::fs::write(path, common::images::jpeg_q(&img, q)).unwrap();
}

#[test]
fn never_overwrites_sources_even_in_the_same_folder() {
    let dir = fresh_dir("same_folder");
    let a = dir.join("a.jpg");
    let b = dir.join("b.png");
    write_photo_jpeg(&a, 3000, 2000, 97);
    common::images::photo(1200, 800).save(&b).unwrap();
    let (a_before, b_before) = (std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());

    let report =
        images_compress::run(&[a.clone(), b.clone()], &dir, Tier::Balanced, &NoProgress).unwrap();

    assert_eq!(std::fs::read(&a).unwrap(), a_before, "a.jpg 被改动了");
    assert_eq!(std::fs::read(&b).unwrap(), b_before, "b.png 被改动了");
    for item in &report.value.items {
        assert_ne!(item.output, a);
        assert_ne!(item.output, b);
        assert!(item.output.exists(), "{} 没有写出来", item.output.display());
    }
    assert_eq!(report.value.items.len(), 2);
}

#[test]
fn same_names_from_different_folders_do_not_overwrite_each_other() {
    let root = fresh_dir("collide");
    let (x, y, out) = (root.join("x"), root.join("y"), root.join("out"));
    std::fs::create_dir_all(&x).unwrap();
    std::fs::create_dir_all(&y).unwrap();
    common::images::photo(300, 200)
        .save(x.join("a.png"))
        .unwrap();
    common::images::photo(200, 300)
        .save(y.join("a.png"))
        .unwrap();

    let report = images_compress::run(
        &[x.join("a.png"), y.join("a.png")],
        &out,
        Tier::Lossless,
        &NoProgress,
    )
    .unwrap();

    let outputs: Vec<&PathBuf> = report.value.items.iter().map(|i| &i.output).collect();
    assert_eq!(outputs.len(), 2);
    assert_ne!(outputs[0], outputs[1], "两个同名文件写到了同一个输出上");
    for (item, src) in report
        .value
        .items
        .iter()
        .zip([x.join("a.png"), y.join("a.png")])
    {
        assert_eq!(
            std::fs::read(&item.output).unwrap(),
            std::fs::read(&src).unwrap(),
            "无损档应当原样复制 {}",
            src.display()
        );
    }
}

/// 重新编码会丢掉 EXIF，方向标记也就没了 —— 所以方向必须先作用到像素上，
/// 否则横拍的手机照片压完是躺着的。
#[test]
fn exif_orientation_is_applied_before_reencoding() {
    let src = common::images::write_jpeg_with_exif(
        "img_compress",
        "rotated.jpg",
        3200,
        1800,
        6,
        "2024:03:15 14:30:22",
    );
    let out = fresh_dir("rotated_out");
    let report = images_compress::run(
        std::slice::from_ref(&src),
        &out,
        Tier::Balanced,
        &NoProgress,
    )
    .unwrap();
    let item = &report.value.items[0];
    let (w, h) = image::image_dimensions(&item.output).unwrap();
    assert!(
        h > w,
        "方向 6（需顺时针转 90°）的横图压缩后应当是竖图，实际 {w}×{h}"
    );
}

/// 写到一半失败（磁盘满、被杀）不能在目标路径上留下残缺文件；成功后也不能留下临时文件。
#[test]
fn leaves_no_partial_files_behind() {
    let dir = fresh_dir("atomic");
    let src = dir.join("p.jpg");
    write_photo_jpeg(&src, 2400, 1600, 97);
    let out = dir.join("out");
    images_compress::run(
        std::slice::from_ref(&src),
        &out,
        Tier::Balanced,
        &NoProgress,
    )
    .unwrap();
    let leftovers: Vec<_> = std::fs::read_dir(&out)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
        .collect();
    assert!(leftovers.is_empty(), "留下了临时文件：{leftovers:?}");
}
