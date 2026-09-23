//! 四个用例的编排层。
//!
//! 界面层只调用这里的函数。把编排放在核心 crate 里，是为了让「选文件 → 解码 →
//! 缩放 → 组装 → 保存」这整条链路可以在没有显示器的环境下跑测试。
//! 取消检查点和警告累积也都收在这一层。

pub mod docx_to_pdf;
pub mod images_compress;
pub mod images_to_pdf;
pub mod pdf_compress;
