//! 从 PDF 内容流里取出「每一行在哪、是什么字」。
//!
//! 同一套代码要读两种 PDF：我们自己的（Type0 + Identity-H，双字节码，`Tm` 定位）和
//! LibreOffice 的（简单字体、单字节码、`Td` 定位、TJ 里带字距调整）。
//!
//! 不用 `pdftotext -bbox`：它给的 yMin/yMax 是按字体外框估的，推不出基线；
//! 这里自己跟踪文本矩阵，拿到的是精确的基线原点。
//!
//! 本文件只依赖 lopdf，也被 `examples/pdfdump.rs` 通过 `#[path]` 引用。

use std::collections::HashMap;

use lopdf::{Dictionary, Document, Object};

/// 一次文本绘制操作（Tj / TJ / ' / "）。
#[derive(Debug, Clone)]
pub struct Frag {
    /// 基线起点，PDF 用户空间（原点在左下角）。不含 `Ts` 上浮。
    pub x: f32,
    pub y: f32,
    /// 绘制后的宽度（设备空间）。
    pub width: f32,
    /// 有效字号（`Tf` 字号 × 文本矩阵与 CTM 的竖向缩放）。
    pub size: f32,
    /// BaseFont，去掉了子集前缀（`ABCDEF+`）。
    pub font: String,
    pub text: String,
    /// 每个字形的原文与起点 x。量两端对齐把空间分到了哪里时用。
    pub glyphs: Vec<(String, f32)>,
}

#[derive(Debug, Clone)]
pub struct Line {
    pub y: f32,
    pub x0: f32,
    pub x1: f32,
    pub text: String,
    pub frags: Vec<Frag>,
}

#[derive(Debug, Clone)]
pub struct PageText {
    pub width: f32,
    pub height: f32,
    pub lines: Vec<Line>,
}

impl PageText {
    /// 去掉空白后的整页文字。
    pub fn text(&self) -> String {
        self.lines.iter().map(|l| norm(&l.text)).collect()
    }
}

/// 比对用的文字规整：去掉所有空白与控制字符。
///
/// 空格在两边的 PDF 里表现形式不同（真空格字形 / 定位跳过），比较时不应计较。
pub fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .collect()
}

pub fn extract(pdf: &[u8]) -> Vec<PageText> {
    let doc = Document::load_mem(pdf).expect("PDF 无法解析");
    doc.get_pages()
        .values()
        .map(|&page_id| extract_page(&doc, page_id))
        .collect()
}

fn extract_page(doc: &Document, page_id: lopdf::ObjectId) -> PageText {
    let (width, height) = media_box(doc, page_id);
    let fonts: HashMap<Vec<u8>, FontDec> = doc
        .get_page_fonts(page_id)
        .unwrap_or_default()
        .into_iter()
        .map(|(name, dict)| (name, FontDec::new(doc, dict)))
        .collect();

    let content = doc
        .get_and_decode_page_content(page_id)
        .expect("内容流无法解码");

    let mut st = State::default();
    let mut stack: Vec<[f32; 6]> = Vec::new();
    let mut frags: Vec<Frag> = Vec::new();

    for op in &content.operations {
        let n = |i: usize| {
            op.operands
                .get(i)
                .and_then(|o| o.as_float().ok())
                .unwrap_or(0.0)
        };
        match op.operator.as_str() {
            "q" => stack.push(st.ctm),
            "Q" => st.ctm = stack.pop().unwrap_or(IDENTITY),
            "cm" => st.ctm = mul([n(0), n(1), n(2), n(3), n(4), n(5)], st.ctm),
            "BT" => {
                st.tm = IDENTITY;
                st.tlm = IDENTITY;
            }
            "Tf" => {
                st.font = op
                    .operands
                    .first()
                    .and_then(|o| o.as_name().ok())
                    .map(|n| n.to_vec())
                    .unwrap_or_default();
                st.size = n(1);
            }
            "Tc" => st.tc = n(0),
            "Tw" => st.tw = n(0),
            "Tz" => st.th = n(0) / 100.0,
            "TL" => st.tl = n(0),
            // `Ts`（上浮）不跟踪：上下标应当和正文归在同一行里。
            "Td" => st.td(n(0), n(1)),
            "TD" => {
                st.tl = -n(1);
                st.td(n(0), n(1));
            }
            "Tm" => {
                st.tlm = [n(0), n(1), n(2), n(3), n(4), n(5)];
                st.tm = st.tlm;
            }
            "T*" => st.td(0.0, -st.tl),
            "Tj" | "'" | "\"" | "TJ" => {
                if op.operator == "'" || op.operator == "\"" {
                    if op.operator == "\"" {
                        st.tw = n(0);
                        st.tc = n(1);
                    }
                    st.td(0.0, -st.tl);
                }
                let Some(font) = fonts.get(&st.font) else {
                    continue;
                };
                if let Some(f) = st.show(font, &op.operands) {
                    frags.push(f);
                }
            }
            _ => {}
        }
    }

    PageText {
        width,
        height,
        lines: group_lines(frags),
    }
}

/// 同一基线（容差 0.5pt）的片段归成一行，行内按 x 排序，行按从上到下排。
fn group_lines(mut frags: Vec<Frag>) -> Vec<Line> {
    frags.sort_by(|a, b| {
        b.y.partial_cmp(&a.y)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal))
    });
    let mut lines: Vec<Line> = Vec::new();
    for f in frags {
        match lines.last_mut() {
            Some(l) if (l.y - f.y).abs() <= 0.5 => l.frags.push(f),
            _ => lines.push(Line {
                y: f.y,
                x0: 0.0,
                x1: 0.0,
                text: String::new(),
                frags: vec![f],
            }),
        }
    }
    for l in &mut lines {
        l.frags
            .sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));
        l.x0 = l.frags.first().map(|f| f.x).unwrap_or(0.0);
        l.x1 = l.frags.last().map(|f| f.x + f.width).unwrap_or(0.0);
        l.text = l.frags.iter().map(|f| f.text.as_str()).collect();
    }
    lines
}

/// 调试 dump：一行一条，稳定、可 diff。
///
/// `p<页> y=<基线> | <x> <字体> <字号> "<文字>" | …`
pub fn dump(pages: &[PageText]) -> String {
    let mut out = String::new();
    for (i, p) in pages.iter().enumerate() {
        out.push_str(&format!(
            "# page {} {:.2}x{:.2}\n",
            i + 1,
            p.width,
            p.height
        ));
        for l in &p.lines {
            out.push_str(&format!("p{} y={:.3}", i + 1, l.y));
            for f in &l.frags {
                out.push_str(&format!(
                    " | {:.3} {} {:.2} {:?}",
                    f.x, f.font, f.size, f.text
                ));
            }
            out.push('\n');
        }
    }
    out
}

// ---------------------------------------------------------------- 文本状态

const IDENTITY: [f32; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// 行向量约定下的 m × n（先 m 后 n）。
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

struct State {
    ctm: [f32; 6],
    tm: [f32; 6],
    tlm: [f32; 6],
    font: Vec<u8>,
    size: f32,
    tc: f32,
    tw: f32,
    th: f32,
    tl: f32,
}

impl Default for State {
    fn default() -> Self {
        Self {
            ctm: IDENTITY,
            tm: IDENTITY,
            tlm: IDENTITY,
            font: Vec::new(),
            size: 0.0,
            tc: 0.0,
            tw: 0.0,
            th: 1.0,
            tl: 0.0,
        }
    }
}

impl State {
    fn td(&mut self, tx: f32, ty: f32) {
        self.tlm = mul([1.0, 0.0, 0.0, 1.0, tx, ty], self.tlm);
        self.tm = self.tlm;
    }

    fn origin(&self) -> (f32, f32) {
        let m = mul(self.tm, self.ctm);
        (m[4], m[5])
    }

    fn advance(&mut self, tx: f32) {
        self.tm = mul([1.0, 0.0, 0.0, 1.0, tx, 0.0], self.tm);
    }

    fn show_bytes(
        &mut self,
        font: &FontDec,
        bytes: &[u8],
        text: &mut String,
        glyphs: &mut Vec<(String, f32)>,
    ) {
        let step = if font.two_byte { 2 } else { 1 };
        for chunk in bytes.chunks(step) {
            let code = be(chunk);
            let before = text.len();
            match font.to_unicode.get(&code) {
                Some(s) => text.push_str(s),
                // 没有 ToUnicode 的简单字体：按 Latin-1 近似（WinAnsi 在可打印区与之一致）。
                None if !font.two_byte => text.push(code as u8 as char),
                None => {}
            }
            glyphs.push((text[before..].to_string(), self.origin().0));
            let w = font.width(code) / 1000.0;
            let word = if !font.two_byte && code == 32 {
                self.tw
            } else {
                0.0
            };
            self.advance((w * self.size + self.tc + word) * self.th);
        }
    }

    fn show(&mut self, font: &FontDec, operands: &[Object]) -> Option<Frag> {
        let m = mul(self.tm, self.ctm);
        let size = self.size * (m[2] * m[2] + m[3] * m[3]).sqrt();
        let (x, y) = self.origin();
        let mut text = String::new();
        let mut glyphs = Vec::new();

        for o in operands {
            match o {
                Object::String(bytes, _) => self.show_bytes(font, bytes, &mut text, &mut glyphs),
                Object::Array(items) => {
                    for it in items {
                        match it {
                            Object::String(bytes, _) => {
                                self.show_bytes(font, bytes, &mut text, &mut glyphs)
                            }
                            other => {
                                if let Ok(adj) = other.as_float() {
                                    self.advance(-adj / 1000.0 * self.size * self.th);
                                }
                            }
                        }
                    }
                }
                // `"` 的前两个操作数是数字，调用方已经处理过了。
                _ => {}
            }
        }
        if text.is_empty() {
            return None;
        }
        let (x_end, _) = self.origin();
        Some(Frag {
            x,
            y,
            width: x_end - x,
            size,
            font: font.base.clone(),
            text,
            glyphs,
        })
    }
}

// ---------------------------------------------------------------- 字体

struct FontDec {
    base: String,
    two_byte: bool,
    to_unicode: HashMap<u32, String>,
    simple_widths: Option<(u32, Vec<f32>)>,
    cid_widths: HashMap<u32, f32>,
    default_width: f32,
}

impl FontDec {
    fn new(doc: &Document, dict: &Dictionary) -> Self {
        let base = dict
            .get(b"BaseFont")
            .and_then(Object::as_name)
            .map(|n| strip_subset(&String::from_utf8_lossy(n)))
            .unwrap_or_default();
        let two_byte = dict
            .get(b"Subtype")
            .and_then(Object::as_name)
            .map(|s| s == b"Type0")
            .unwrap_or(false);

        let to_unicode = dict
            .get(b"ToUnicode")
            .ok()
            .and_then(|o| doc.dereference(o).ok())
            .and_then(|(_, o)| o.as_stream().ok())
            .and_then(|s| {
                s.decompressed_content()
                    .ok()
                    .or_else(|| Some(s.content.clone()))
            })
            .map(|data| parse_cmap(&data))
            .unwrap_or_default();

        let mut me = Self {
            base,
            two_byte,
            to_unicode,
            simple_widths: None,
            cid_widths: HashMap::new(),
            default_width: if two_byte { 1000.0 } else { 0.0 },
        };

        if two_byte {
            // Type0：宽度在 DescendantFonts[0] 的 /W 与 /DW 里。Identity-H 下 CID == 码值。
            let desc = dict
                .get(b"DescendantFonts")
                .ok()
                .and_then(|o| doc.dereference(o).ok())
                .and_then(|(_, o)| o.as_array().ok())
                .and_then(|a| a.first())
                .and_then(|o| doc.dereference(o).ok())
                .and_then(|(_, o)| o.as_dict().ok());
            if let Some(d) = desc {
                if let Ok(dw) = d.get(b"DW").and_then(Object::as_float) {
                    me.default_width = dw;
                }
                if let Some(w) = d
                    .get(b"W")
                    .ok()
                    .and_then(|o| doc.dereference(o).ok())
                    .and_then(|(_, o)| o.as_array().ok())
                {
                    me.cid_widths = parse_w_array(doc, w);
                }
            }
        } else {
            let first = dict.get(b"FirstChar").and_then(Object::as_i64).unwrap_or(0) as u32;
            let widths: Vec<f32> = dict
                .get(b"Widths")
                .ok()
                .and_then(|o| doc.dereference(o).ok())
                .and_then(|(_, o)| o.as_array().ok())
                .map(|a| {
                    a.iter()
                        .map(|o| {
                            doc.dereference(o)
                                .ok()
                                .and_then(|(_, o)| o.as_float().ok())
                                .unwrap_or(0.0)
                        })
                        .collect()
                })
                .unwrap_or_default();
            me.simple_widths = Some((first, widths));
        }
        me
    }

    fn width(&self, code: u32) -> f32 {
        if self.two_byte {
            return self
                .cid_widths
                .get(&code)
                .copied()
                .unwrap_or(self.default_width);
        }
        match &self.simple_widths {
            Some((first, w)) if code >= *first => w
                .get((code - first) as usize)
                .copied()
                .unwrap_or(self.default_width),
            _ => self.default_width,
        }
    }
}

fn strip_subset(name: &str) -> String {
    match name.split_once('+') {
        Some((tag, rest)) if tag.len() == 6 && tag.chars().all(|c| c.is_ascii_uppercase()) => {
            rest.to_string()
        }
        _ => name.to_string(),
    }
}

/// `/W [c [w1 w2 …] c_first c_last w …]`
fn parse_w_array(doc: &Document, w: &[Object]) -> HashMap<u32, f32> {
    let num = |o: &Object| doc.dereference(o).ok().and_then(|(_, o)| o.as_float().ok());
    let mut out = HashMap::new();
    let mut i = 0;
    while i < w.len() {
        let Some(c) = num(&w[i]) else { break };
        let c = c as u32;
        match w.get(i + 1).map(|o| doc.dereference(o).map(|(_, o)| o)) {
            Some(Ok(Object::Array(ws))) => {
                for (k, v) in ws.iter().enumerate() {
                    if let Some(v) = num(v) {
                        out.insert(c + k as u32, v);
                    }
                }
                i += 2;
            }
            Some(Ok(_)) => {
                let (Some(last), Some(v)) = (num(&w[i + 1]), w.get(i + 2).and_then(num)) else {
                    break;
                };
                for code in c..=last as u32 {
                    out.insert(code, v);
                }
                i += 3;
            }
            _ => break,
        }
    }
    out
}

#[derive(Debug)]
enum Tok {
    Hex(Vec<u8>),
    Word(String),
    Open,
    Close,
}

fn tokenize(s: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'<' => {
                let end = s[i + 1..].find('>').map(|e| i + 1 + e).unwrap_or(b.len());
                let hex: String = s[i + 1..end]
                    .chars()
                    .filter(|c| c.is_ascii_hexdigit())
                    .collect();
                let bytes = (0..hex.len() / 2)
                    .filter_map(|k| u8::from_str_radix(&hex[2 * k..2 * k + 2], 16).ok())
                    .collect();
                out.push(Tok::Hex(bytes));
                i = end + 1;
            }
            b'[' => {
                out.push(Tok::Open);
                i += 1;
            }
            b']' => {
                out.push(Tok::Close);
                i += 1;
            }
            c if c.is_ascii_whitespace() => i += 1,
            _ => {
                let start = i;
                while i < b.len()
                    && !b[i].is_ascii_whitespace()
                    && !matches!(b[i], b'<' | b'[' | b']')
                {
                    i += 1;
                }
                out.push(Tok::Word(s[start..i].to_string()));
            }
        }
    }
    out
}

fn be(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0u32, |acc, b| (acc << 8) | *b as u32)
}

fn utf16(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks(2)
        .map(|c| ((c[0] as u16) << 8) | c.get(1).copied().unwrap_or(0) as u16)
        .collect();
    String::from_utf16_lossy(&units)
}

/// ToUnicode CMap 里的 bfchar 与 bfrange。
pub fn parse_cmap(data: &[u8]) -> HashMap<u32, String> {
    let s = String::from_utf8_lossy(data);
    let toks = tokenize(&s);
    let mut map = HashMap::new();
    let mut i = 0;
    let is_word = |t: &Tok, w: &str| matches!(t, Tok::Word(x) if x == w);
    while i < toks.len() {
        if is_word(&toks[i], "beginbfchar") {
            i += 1;
            while i + 1 < toks.len() && !is_word(&toks[i], "endbfchar") {
                if let (Tok::Hex(a), Tok::Hex(b)) = (&toks[i], &toks[i + 1]) {
                    map.insert(be(a), utf16(b));
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if is_word(&toks[i], "beginbfrange") {
            i += 1;
            while i + 2 < toks.len() && !is_word(&toks[i], "endbfrange") {
                let (Tok::Hex(lo), Tok::Hex(hi)) = (&toks[i], &toks[i + 1]) else {
                    i += 1;
                    continue;
                };
                let (lo, hi) = (be(lo), be(hi));
                match &toks[i + 2] {
                    Tok::Hex(dst) => {
                        let mut units: Vec<u16> = dst
                            .chunks(2)
                            .map(|c| ((c[0] as u16) << 8) | c.get(1).copied().unwrap_or(0) as u16)
                            .collect();
                        for code in lo..=hi.min(lo + 0xFFFF) {
                            map.insert(code, String::from_utf16_lossy(&units));
                            if let Some(last) = units.last_mut() {
                                *last = last.wrapping_add(1);
                            }
                        }
                        i += 3;
                    }
                    Tok::Open => {
                        let mut k = i + 3;
                        let mut code = lo;
                        while k < toks.len() && !matches!(toks[k], Tok::Close) {
                            if let Tok::Hex(dst) = &toks[k] {
                                map.insert(code, utf16(dst));
                                code += 1;
                            }
                            k += 1;
                        }
                        i = k + 1;
                    }
                    _ => i += 1,
                }
            }
        }
        i += 1;
    }
    map
}

fn media_box(doc: &Document, page_id: lopdf::ObjectId) -> (f32, f32) {
    // MediaBox 可以从页树上级继承。
    let mut cur = Some(page_id);
    let mut guard = 0;
    while let (Some(id), true) = (cur, guard < 64) {
        guard += 1;
        let Ok(dict) = doc.get_dictionary(id) else {
            break;
        };
        if let Some(arr) = dict
            .get(b"MediaBox")
            .ok()
            .and_then(|o| doc.dereference(o).ok())
            .and_then(|(_, o)| o.as_array().ok())
        {
            let v: Vec<f32> = arr.iter().filter_map(|o| o.as_float().ok()).collect();
            if v.len() == 4 {
                return (v[2] - v[0], v[3] - v[1]);
            }
        }
        cur = dict.get(b"Parent").and_then(Object::as_reference).ok();
    }
    (595.0, 842.0)
}
