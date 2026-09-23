//! Symbol / Wingdings 这类「符号字体」的私用区码位 → 意思最接近的 Unicode 字符。
//!
//! Word 用这些字体画项目符号和勾选框时，文字里存的是 U+F0xx 私用区码位，
//! 只有装了原字体才显示得出来。本机没有这些字体时（Linux、macOS 上很常见），
//! 换成 Unicode 里对应的字符，再交给回退字体去画 —— 否则就是一个方框，
//! 而且复制出来是乱码。
//!
//! 只收录有把握的常用字符；表里没有的保持原样（会作为缺字如实报告）。
//! 映射目标全部在 U+0800–U+FFFF：与 U+F0xx 同为 3 字节 UTF-8，替换后字节偏移不变。

/// `family` 是 run 上写的字体名。不是已知的符号字体、或码位不在表里时返回 None。
pub fn symbol_to_unicode(family: &str, c: char) -> Option<char> {
    let code = c as u32;
    if !(0xF020..=0xF0FF).contains(&code) {
        return None;
    }
    let low = (code - 0xF000) as u8;
    let table: &[(u8, char)] = match family.trim().to_ascii_lowercase().as_str() {
        "symbol" => SYMBOL,
        "wingdings" => WINGDINGS,
        _ => return None,
    };
    table.iter().find(|(k, _)| *k == low).map(|(_, v)| *v)
}

/// 符号字体的名字。run 用了这些字体、而本机又没有时，才需要映射。
pub fn is_symbol_font(family: &str) -> bool {
    matches!(
        family.trim().to_ascii_lowercase().as_str(),
        "symbol" | "wingdings"
    )
}

const SYMBOL: &[(u8, char)] = &[
    (0xA2, '′'),
    (0xA3, '≤'),
    (0xA5, '∞'),
    (0xA7, '♣'),
    (0xA8, '♦'),
    (0xA9, '♥'),
    (0xAA, '♠'),
    (0xAB, '↔'),
    (0xAC, '←'),
    (0xAD, '↑'),
    (0xAE, '→'),
    (0xAF, '↓'),
    (0xB2, '″'),
    (0xB3, '≥'),
    (0xB6, '∂'),
    // Word 默认的圆点项目符号。
    (0xB7, '•'),
    (0xB9, '≠'),
    (0xBB, '≈'),
    (0xBC, '…'),
    (0xC6, '∅'),
    (0xC7, '∩'),
    (0xC8, '∪'),
    (0xC9, '⊃'),
    (0xCC, '⊂'),
    (0xCE, '∈'),
    (0xD1, '∇'),
    (0xD5, '∏'),
    (0xD6, '√'),
    (0xDB, '⇔'),
    (0xDC, '⇐'),
    (0xDE, '⇒'),
    (0xE5, '∑'),
    (0xF2, '∫'),
];

const WINGDINGS: &[(u8, char)] = &[
    (0x6C, '●'),
    (0x6E, '■'),
    (0x6F, '□'),
    (0x75, '◆'),
    (0x76, '❖'),
    (0xA1, '○'),
    (0xA7, '▪'),
    (0xA8, '◻'),
    (0xD8, '➢'),
    (0xFB, '✗'),
    (0xFC, '✓'),
    (0xFD, '☒'),
    (0xFE, '☑'),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_bullets_and_checkboxes_map() {
        assert_eq!(symbol_to_unicode("Symbol", '\u{F0B7}'), Some('•'));
        assert_eq!(symbol_to_unicode("Wingdings", '\u{F0FE}'), Some('☑'));
        assert_eq!(symbol_to_unicode("wingdings ", '\u{F0A7}'), Some('▪'));
        // 同一个码位在两个字体里是不同的字符。
        assert_eq!(symbol_to_unicode("Symbol", '\u{F0A7}'), Some('♣'));
        assert_eq!(symbol_to_unicode("Arial", '\u{F0B7}'), None);
        assert_eq!(symbol_to_unicode("Symbol", 'a'), None);
    }

    #[test]
    fn replacements_keep_utf8_length() {
        for (_, c) in SYMBOL.iter().chain(WINGDINGS) {
            assert_eq!(c.len_utf8(), 3, "{c} 替换后字节偏移会错位");
        }
    }
}
