//! 找出页面用到的图片，并从内容流里推算它们实际画多大。
//!
//! 「有效 DPI」= 像素数 ÷ 实际显示的物理尺寸。后者必须从内容流里绘制该图时的
//! CTM 矩阵拿到 —— 同一张图可能在不同页以不同大小出现多次，取最大的那次，
//! 因为降采样要按最苛刻的用途来定。

use std::collections::{HashMap, HashSet};

use lopdf::content::Operation;
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};

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

/// 表单（Form XObject）最多套几层。正常文件一两层，再深多半是坏文件或故意构造的。
const MAX_FORM_DEPTH: usize = 16;

/// 一个表单的内容解压出来最多多大：防解压炸弹，正常的表单内容远小于这个数。
const MAX_FORM_CONTENT: usize = 64 << 20;

/// 扫描全文的结果。
pub struct Scan {
    /// 用到的图片，按第一次出现的顺序，不重复。
    pub images: Vec<ObjectId>,
    /// 每张图被放置过的最大尺寸。
    pub placements: Placements,
}

/// 扫描全文：找出所有图片，以及它们被放置的最大尺寸。
///
/// 资源（`/Resources`）可以写在页面上，也可以写在上级 Pages 节点里由页面继承；图片
/// 可以直接画在页面上，也可以画在 Form XObject 里（有的扫描软件每一页都包一层）。
/// 两种都要走到，不然整份文件的图一张都压不到。资源里列了、内容里没画的图也算上。
///
/// 放置尺寸是尽力而为：扫不到的图不会出现在 `placements` 里，调用方要退化成
/// 「按整页铺满」估算。
pub fn scan(doc: &Document) -> Scan {
    let mut scanner = Scanner {
        doc,
        images: Vec::new(),
        seen: HashSet::new(),
        placements: HashMap::new(),
    };
    for (_, page_id) in doc.get_pages() {
        let Some(resources) = page_resources(doc, page_id) else {
            continue;
        };
        let ops = doc
            .get_and_decode_page_content(page_id)
            .map(|c| c.operations)
            .unwrap_or_default();
        scanner.walk(resources, &ops, IDENTITY, &mut Vec::new());
    }
    Scan {
        images: scanner.images,
        placements: scanner.placements,
    }
}

const IDENTITY: [f32; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// 页面的资源：自己没写，就用最近的上级 Pages 节点的。
fn page_resources(doc: &Document, page_id: ObjectId) -> Option<&Dictionary> {
    let mut node = doc.get_dictionary(page_id).ok()?;
    for _ in 0..MAX_FORM_DEPTH * 4 {
        if let Ok(r) = node.get(b"Resources") {
            return doc.dereference(r).ok()?.1.as_dict().ok();
        }
        let parent = node.get(b"Parent").and_then(Object::as_reference).ok()?;
        node = doc.get_dictionary(parent).ok()?;
    }
    None
}

struct Scanner<'a> {
    doc: &'a Document,
    images: Vec<ObjectId>,
    seen: HashSet<ObjectId>,
    placements: Placements,
}

impl<'a> Scanner<'a> {
    /// 走一段内容。`resources` 是它用的资源，`ctm` 是开头的变换，`forms` 是正在里面的
    /// 表单（表单互相引用时不转圈）。
    fn walk(
        &mut self,
        resources: &'a Dictionary,
        ops: &[Operation],
        ctm: [f32; 6],
        forms: &mut Vec<ObjectId>,
    ) {
        let doc = self.doc;
        // 资源名 → XObject。
        let mut xobjects: HashMap<&[u8], (ObjectId, &'a Stream)> = HashMap::new();
        if let Some(dict) = resources
            .get(b"XObject")
            .ok()
            .and_then(|o| doc.dereference(o).ok())
            .and_then(|(_, o)| o.as_dict().ok())
        {
            for (name, value) in dict.iter() {
                let Ok(id) = value.as_reference() else {
                    continue;
                };
                let Ok(stream) = doc.get_object(id).and_then(Object::as_stream) else {
                    continue;
                };
                if subtype(stream) == Some(b"Image".as_ref()) && self.seen.insert(id) {
                    self.images.push(id);
                }
                xobjects.insert(name.as_slice(), (id, stream));
            }
        }

        let mut ctm = ctm;
        let mut stack: Vec<[f32; 6]> = Vec::new();
        for op in ops {
            match op.operator.as_str() {
                "q" => stack.push(ctm),
                "Q" => ctm = stack.pop().unwrap_or(IDENTITY),
                "cm" => {
                    if let Some(m) = matrix(&op.operands) {
                        ctm = mul(&m, &ctm);
                    }
                }
                "Do" => {
                    let Some(Object::Name(n)) = op.operands.first() else {
                        continue;
                    };
                    let Some(&(id, stream)) = xobjects.get(n.as_slice()) else {
                        continue;
                    };
                    match subtype(stream) {
                        Some(b"Image") => {
                            let (w, h) = unit_square_extent(&ctm);
                            let e = self.placements.entry(id).or_insert((0.0, 0.0));
                            e.0 = e.0.max(w);
                            e.1 = e.1.max(h);
                        }
                        Some(b"Form") if forms.len() < MAX_FORM_DEPTH && !forms.contains(&id) => {
                            self.form(id, stream, resources, ctm, forms)
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    /// 走进一个表单：它的内容按 `/Matrix` 画在当前变换下；没写 `/Resources` 的用画它的
    /// 那段内容的资源。
    fn form(
        &mut self,
        id: ObjectId,
        stream: &'a Stream,
        outer: &'a Dictionary,
        ctm: [f32; 6],
        forms: &mut Vec<ObjectId>,
    ) {
        let doc = self.doc;
        let Ok(content) = stream.get_plain_content_with_limit(MAX_FORM_CONTENT) else {
            return;
        };
        let Ok(content) = lopdf::content::Content::decode(&content) else {
            return;
        };
        let resources = stream
            .dict
            .get(b"Resources")
            .ok()
            .and_then(|o| doc.dereference(o).ok())
            .and_then(|(_, o)| o.as_dict().ok())
            .unwrap_or(outer);
        let m = stream
            .dict
            .get(b"Matrix")
            .ok()
            .and_then(|o| o.as_array().ok())
            .and_then(|a| matrix(a))
            .unwrap_or(IDENTITY);
        forms.push(id);
        self.walk(resources, &content.operations, mul(&m, &ctm), forms);
        forms.pop();
    }
}

fn subtype(stream: &Stream) -> Option<&[u8]> {
    stream.dict.get(b"Subtype").and_then(Object::as_name).ok()
}

/// 六个数的变换矩阵（`cm` 的操作数、表单的 `/Matrix`）。
fn matrix(operands: &[Object]) -> Option<[f32; 6]> {
    if operands.len() != 6 {
        return None;
    }
    let mut m = [0.0f32; 6];
    for (slot, o) in m.iter_mut().zip(operands) {
        *slot = operand_f32(o)?;
    }
    Some(m)
}
