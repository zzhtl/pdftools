pub mod common;
pub mod docx2pdf;
pub mod img2pdf;
pub mod img_compress;
pub mod pdf2img;
pub mod pdf_compress;
pub mod pdf_pages;

/// 页签底部留给开始按钮的高度。
pub const FOOTER: f32 = 44.0;

pub fn file_label(path: &std::path::Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}
