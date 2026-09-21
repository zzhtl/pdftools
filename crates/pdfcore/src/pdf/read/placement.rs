//! 从内容流里推算图片的实际放置尺寸。
//!
//! 「有效 DPI」= 像素数 ÷ 实际显示的物理尺寸。后者必须从内容流里绘制该图时的
//! CTM 矩阵拿到 —— 同一张图可能在不同页以不同大小出现多次，取最大的那次，
//! 因为降采样要按最苛刻的用途来定。

use std::collections::HashMap;

use lopdf::{Document, Object, ObjectId};

/// 图片对象号 → 它在全文中被放置过的最大尺寸（点）。
pub type Placements = HashMap<ObjectId, (f32, f32)>;

/// 单位方块经过矩阵变换后的宽高。
/// image XObject 永远画在 (0,0)-(1,1) 的单位方块里，由 cm 矩阵拉伸到实际大小。
fn unit_square_extent(m: &[f32; 6]) -> (f32, f32) {
    (m[0].hypot(m[1]), m[2].hypot(m[3]))
}

fn mul(a: &[f32; 6], b: &[f32; 6]) -> [f32; 6] {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
        a[4] * b[0] + a[5] * b[2] + b[4],
        a[4] * b[1] + a[5] * b[3] + b[5],
    ]
}

fn operand_f32(o: &Object) -> Option<f32> {
    match o {
        Object::Integer(i) => Some(*i as f32),
        Object::Real(r) => Some(*r),
        _ => None,
    }
}

/// 扫描全文，得出每张图被放置的最大尺寸。
///
/// 这是尽力而为：扫不到的图（例如画在 Form XObject 里的）不会出现在结果中，
/// 调用方要退化成「按整页铺满」估算。
pub fn scan(doc: &Document) -> Placements {
    let mut out: Placements = HashMap::new();

    for (_, page_id) in doc.get_pages() {
        // 资源名 → 对象号
        let mut names: HashMap<Vec<u8>, ObjectId> = HashMap::new();
        if let Ok((Some(resources), _)) = doc.get_page_resources(page_id) {
            if let Ok(xo) = resources.get(b"XObject").and_then(|o| o.as_dict()) {
                for (name, value) in xo.iter() {
                    if let Ok(id) = value.as_reference() {
                        names.insert(name.to_vec(), id);
                    }
                }
            }
        }
        if names.is_empty() {
            continue;
        }

        let Ok(content) = doc.get_and_decode_page_content(page_id) else {
            continue;
        };

        let identity = [1.0f32, 0.0, 0.0, 1.0, 0.0, 0.0];
        let mut ctm = identity;
        let mut stack: Vec<[f32; 6]> = Vec::new();

        for op in &content.operations {
            match op.operator.as_str() {
                "q" => stack.push(ctm),
                "Q" => ctm = stack.pop().unwrap_or(identity),
                "cm" => {
                    if op.operands.len() == 6 {
                        let mut m = [0.0f32; 6];
                        let mut ok = true;
                        for (i, o) in op.operands.iter().enumerate() {
                            match operand_f32(o) {
                                Some(v) => m[i] = v,
                                None => {
                                    ok = false;
                                    break;
                                }
                            }
                        }
                        if ok {
                            ctm = mul(&m, &ctm);
                        }
                    }
                }
                "Do" => {
                    let Some(Object::Name(n)) = op.operands.first() else {
                        continue;
                    };
                    let Some(id) = names.get(n.as_slice()) else {
                        continue;
                    };
                    let (w, h) = unit_square_extent(&ctm);
                    let e = out.entry(*id).or_insert((0.0, 0.0));
                    e.0 = e.0.max(w);
                    e.1 = e.1.max(h);
                }
                _ => {}
            }
        }
    }
    out
}
