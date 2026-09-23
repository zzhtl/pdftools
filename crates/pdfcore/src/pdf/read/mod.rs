//! 读取已有的 PDF：找出页面用到的图片与它们画多大（PDF 压缩用），以及改写前的
//! 把关（压缩、页面操作共用）。
//!
//! 与 `writer` 的根本区别：那边每个字节都是我们自己写的，这边则要在**不破坏
//! 我们看不懂的字节**的前提下做局部替换。所以这里的默认动作永远是「原样保留」。

pub(crate) mod placement;

use lopdf::Document;

use crate::error::{CoreError, Result};

/// 读进来准备改写的 PDF。加了密的、带数字签名的不接：前者解不对，后者一改签名就失效。
pub(crate) fn load_for_rewrite(data: &[u8]) -> Result<Document> {
    let doc =
        Document::load_mem(data).map_err(|e| CoreError::Pdf(format!("无法解析该 PDF：{e}")))?;
    if doc.was_encrypted() || doc.is_encrypted() {
        return Err(CoreError::Unsupported(
            "该 PDF 已加密。请先在其他工具里去掉密码保护再来处理。".into(),
        ));
    }
    if has_signature(&doc) {
        return Err(CoreError::Unsupported(
            "该 PDF 带有数字签名。任何改写都会让签名失效，因此不做处理。".into(),
        ));
    }
    Ok(doc)
}

fn has_signature(doc: &Document) -> bool {
    doc.objects.values().any(|o| {
        o.as_dict()
            .ok()
            .and_then(|d| d.get(b"Type").ok())
            .and_then(|t| t.as_name().ok())
            == Some(b"Sig".as_ref())
    })
}
