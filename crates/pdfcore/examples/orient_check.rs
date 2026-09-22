//! 开发期校验：PDF 变换矩阵施加的 EXIF 方向，是否与 image crate 的参照实现一致。
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = std::path::PathBuf::from(std::env::args().nth(1).unwrap());
    std::fs::create_dir_all(&out)?;

    // 一张左右上下都不对称的图，任何方向错误都能一眼看出
    let w = 240u32;
    let h = 160u32;
    let src = image::RgbImage::from_fn(w, h, |x, y| {
        if x < w / 6 && y < h / 6 {
            image::Rgb([220, 40, 40]) // 左上角红块
        } else if x > w * 5 / 6 && y < h / 6 {
            image::Rgb([40, 160, 60]) // 右上角绿块
        } else if x < w / 6 && y > h * 5 / 6 {
            image::Rgb([50, 90, 220]) // 左下角蓝块
        } else {
            let g = (200 - (x * 120 / w) as i32 - (y * 60 / h) as i32).clamp(0, 255) as u8;
            image::Rgb([g, g, g])
        }
    });

    for o in 1..=8u16 {
        let mut jpeg = Vec::new();
        image::DynamicImage::ImageRgb8(src.clone()).write_to(
            &mut std::io::Cursor::new(&mut jpeg),
            image::ImageFormat::Jpeg,
        )?;
        let mut file = jpeg[..2].to_vec();
        file.extend_from_slice(&exif_app1(o));
        file.extend_from_slice(&jpeg[2..]);
        let p = out.join(format!("o{o}.jpg"));
        std::fs::write(&p, &file)?;

        // 参照：image crate 自己施加方向
        let mut reference = image::load_from_memory(&file)?;
        reference.apply_orientation(image::metadata::Orientation::from_exif(o as u8).unwrap());
        reference.save(out.join(format!("ref{o}.png")))?;

        // 我们的实现：走 PDF 的变换矩阵
        let r = pdfcore::ops::images_to_pdf::run(
            &[p],
            pdfcore::imaging::Tier::Lossless,
            &pdfcore::NoProgress,
        )?;
        std::fs::write(out.join(format!("pdf{o}.pdf")), &r.value.pdf)?;
    }
    println!("已生成 8 个方向的参照图与 PDF");
    Ok(())
}

fn exif_app1(orientation: u16) -> Vec<u8> {
    let mut t = Vec::new();
    t.extend_from_slice(b"MM\x00\x2a");
    t.extend_from_slice(&8u32.to_be_bytes());
    t.extend_from_slice(&1u16.to_be_bytes());
    t.extend_from_slice(&0x0112u16.to_be_bytes());
    t.extend_from_slice(&3u16.to_be_bytes());
    t.extend_from_slice(&1u32.to_be_bytes());
    t.extend_from_slice(&orientation.to_be_bytes());
    t.extend_from_slice(&[0, 0]);
    t.extend_from_slice(&0u32.to_be_bytes());
    let mut payload = b"Exif\x00\x00".to_vec();
    payload.extend_from_slice(&t);
    let mut app1 = vec![0xFF, 0xE1];
    app1.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    app1.extend_from_slice(&payload);
    app1
}
