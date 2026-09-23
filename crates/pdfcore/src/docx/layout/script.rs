//! 每个字用西文字体还是东亚字体（[`CharClass::Blocks`](super::CharClass::Blocks)）。
//!
//! 按 Unicode 区段定归属，区段表与 LibreOffice 一致；标点、符号这类没有归属的字
//! 跟随整段里前一个有归属的字。`w:hint="eastAsia"` 按 Word 的规则处理
//! （ECMA-376 §17.3.2.26），LibreOffice 不认它。

use std::ops::Range;

use crate::fonts::{attaches_to_previous, ScriptClass};

/// 按区段定的归属。`None` 是没有归属、要看上下文的字：空格、控制字符、
/// 通用标点（“”—…）、符号（℃①□→）、组合符号。
fn block_class(c: char) -> Option<ScriptClass> {
    use unicode_script::{Script, UnicodeScript};
    use ScriptClass::{EastAsian, Latin};

    if c.is_control() || c == ' ' || c == '\u{A0}' {
        return None;
    }
    match c as u32 {
        // 基本拉丁、拉丁补充（含 ×、·、°、§）、拉丁扩展、IPA、修饰符号；
        // 希腊、西里尔、亚美尼亚；格鲁吉亚；拉丁扩展附加、希腊扩展。
        0x21..=0x7E | 0xA1..=0x2FF | 0x370..=0x58F | 0x10A0..=0x10FF | 0x1E00..=0x1FFF => {
            Some(Latin)
        }
        // 通用标点到杂项符号与箭头：整块都没有归属。按 script 分的话罗马数字 Ⅱ、
        // 欧姆符号 Ω 会被当成西文字母，LibreOffice 不这么分。
        0x2000..=0x2BFF => None,
        // 谚文字母；中日韩部首到 CJK 扩展 A（含 ㎡、㈠ 这类带圈、组合字符）；
        // 基本汉字；彝文；谚文音节；兼容汉字；兼容形式；全角半角形式。
        0x1100..=0x11FF
        | 0x2E80..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF
        | 0xAC00..=0xD7AF
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFFEF => Some(EastAsian),
        _ => match c.script() {
            Script::Han
            | Script::Hiragana
            | Script::Katakana
            | Script::Hangul
            | Script::Bopomofo => Some(EastAsian),
            Script::Common | Script::Inherited | Script::Unknown => None,
            // 复杂文种（阿拉伯、泰文……）本版本不单独处理，用西文字体。
            _ => Some(Latin),
        },
    }
}

/// run 写了 `w:hint="eastAsia"` 时改用东亚字体的字：没有归属的字（空格、控制字符、
/// 组合符号除外），以及拉丁补充里的符号、IPA 到西里尔、拉丁扩展附加。
///
/// 拉丁补充里的字母（é、ü）在 Word 里还要看 run 的东亚语言是不是中文，这里不做，
/// 仍用西文字体。
fn east_asian_under_hint(c: char, own: Option<ScriptClass>) -> bool {
    match own {
        Some(ScriptClass::EastAsian) => true,
        Some(ScriptClass::Latin) => matches!(
            c as u32,
            0xA1 | 0xA4
                | 0xA7
                | 0xA8
                | 0xAA
                | 0xAD
                | 0xAF
                | 0xB0..=0xB4
                | 0xB6..=0xBA
                | 0xBC..=0xBF
                | 0xD7
                | 0xF7
                | 0x250..=0x4FF
                | 0x1E00..=0x1EFF
        ),
        None => !(c.is_control() || c == ' ' || c == '\u{A0}' || attaches_to_previous(c)),
    }
}

/// 整段文字逐字定归属，合并成同归属的连续片段。
///
/// `hints` 是各 span 的区间与它有没有写 `w:hint="eastAsia"`，按位置升序、首尾相接。
/// 没有归属的字跟随前一个字；段首的算西文 —— LibreOffice 按界面语言定（en-US 下
/// 是西文），Word 没写 hint 时这些字用 `w:hAnsi` 字体，也是西文。
pub(super) fn classify(
    text: &str,
    hints: &[(Range<usize>, bool)],
) -> Vec<(Range<usize>, ScriptClass)> {
    let mut out: Vec<(Range<usize>, ScriptClass)> = Vec::new();
    let mut last = ScriptClass::Latin;
    let mut span = 0;
    for (i, c) in text.char_indices() {
        while span + 1 < hints.len() && hints[span].0.end <= i {
            span += 1;
        }
        let hint = hints.get(span).is_some_and(|(r, h)| *h && r.contains(&i));
        let own = block_class(c);
        let class = if hint && east_asian_under_hint(c, own) {
            ScriptClass::EastAsian
        } else {
            own.unwrap_or(last)
        };
        last = class;
        let end = i + c.len_utf8();
        match out.last_mut() {
            Some((r, cls)) if *cls == class => r.end = end,
            _ => out.push((i..end, class)),
        }
    }
    out
}

/// [`classify`] 的结果里落在 `span` 之内的部分。
pub(super) fn within(
    runs: &[(Range<usize>, ScriptClass)],
    span: Range<usize>,
) -> impl Iterator<Item = (Range<usize>, ScriptClass)> + '_ {
    let first = runs.partition_point(|(r, _)| r.end <= span.start);
    runs[first..]
        .iter()
        .take_while(move |(r, _)| r.start < span.end)
        .map(move |(r, c)| (r.start.max(span.start)..r.end.min(span.end), *c))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ScriptClass::{EastAsian as E, Latin as L};

    /// 每个字的归属，按字排开，方便对照。
    fn classes(text: &str, hints: &[(Range<usize>, bool)]) -> Vec<(char, ScriptClass)> {
        let runs = classify(text, hints);
        text.char_indices()
            .map(|(i, c)| {
                let (_, cls) = runs.iter().find(|(r, _)| r.contains(&i)).unwrap();
                (c, *cls)
            })
            .collect()
    }

    fn plain(text: &str) -> Vec<(char, ScriptClass)> {
        classes(text, &[(0..text.len(), false)])
    }

    #[test]
    fn symbols_follow_the_previous_character() {
        assert_eq!(plain("中“文”"), [('中', E), ('“', E), ('文', E), ('”', E)]);
        assert_eq!(plain("AB“C"), [('A', L), ('B', L), ('“', L), ('C', L)]);
        assert_eq!(plain("25℃，"), [('2', L), ('5', L), ('℃', L), ('，', E)]);
        assert_eq!(plain("中——文")[1..3], [('—', E), ('—', E)]);
    }

    /// 段首没有前一个字可跟，算西文；不看后一个字。
    #[test]
    fn leading_symbols_are_latin() {
        assert_eq!(plain("“开")[0], ('“', L));
        assert_eq!(plain("□选")[0], ('□', L));
        assert_eq!(plain("①②")[1], ('②', L));
    }

    /// 拉丁补充里的符号是西文字符，不跟随上下文。
    #[test]
    fn latin_1_symbols_are_latin() {
        assert_eq!(plain("中×文")[1], ('×', L));
        assert_eq!(plain("中·文")[1], ('·', L));
        assert_eq!(plain("中α文")[1], ('α', L));
    }

    /// 罗马数字、欧姆符号有拉丁或希腊的 script 属性，但所在区段整块没有归属。
    /// 希腊字母 Ω（U+03A9）与欧姆符号（U+2126）长得一样，归属不同。
    #[test]
    fn number_forms_and_letterlike_symbols_follow_context() {
        assert_eq!(plain("第\u{2161}部")[1], ('\u{2161}', E));
        assert_eq!(plain("中\u{2126}")[1], ('\u{2126}', E));
        assert_eq!(plain("中\u{3A9}")[1], ('\u{3A9}', L));
    }

    #[test]
    fn cjk_compatibility_characters_are_east_asian() {
        assert_eq!(plain("100㎡")[3], ('㎡', E));
        assert_eq!(plain("A㈠")[1], ('㈠', E));
    }

    /// 跟随整段里的前一个字，不管它在不在同一个 run 里。
    #[test]
    fn context_crosses_run_boundaries() {
        let text = "中“AB";
        let split = "中".len();
        let runs = classify(text, &[(0..split, false), (split..text.len(), false)]);
        let second: Vec<_> = within(&runs, split..text.len()).collect();
        assert_eq!(second[0], (split..split + "“".len(), E));
        assert_eq!(second[1].1, L);
    }

    /// `w:hint="eastAsia"`：没有归属的字、拉丁补充里的符号、希腊字母改用东亚字体；
    /// 字母、数字、空格不受影响。
    #[test]
    fn east_asia_hint_takes_ambiguous_characters() {
        let text = "AB“C”×é α 1";
        let got = classes(text, &[(0..text.len(), true)]);
        let class_of = |ch| got.iter().find(|(c, _)| *c == ch).unwrap().1;
        assert_eq!(class_of('“'), E);
        assert_eq!(class_of('”'), E);
        assert_eq!(class_of('×'), E);
        assert_eq!(class_of('α'), E);
        assert_eq!(class_of('é'), L);
        assert_eq!(class_of('A'), L);
        assert_eq!(class_of('1'), L);
        // 空格跟随前一个字：「α 」后面那个空格跟着 α 走。
        assert_eq!(got[9], (' ', E));
    }

    #[test]
    fn hint_applies_only_to_its_own_run() {
        let text = "“A“";
        let first = "“".len();
        let got = classes(text, &[(0..first, true), (first..text.len(), false)]);
        assert_eq!(got, [('“', E), ('A', L), ('“', L)]);
    }

    #[test]
    fn within_clips_runs_to_the_span() {
        let runs = vec![(0..3, L), (3..9, E), (9..10, L)];
        let got: Vec<_> = within(&runs, 2..9).collect();
        assert_eq!(got, [(2..3, L), (3..9, E)]);
        assert_eq!(within(&runs, 9..10).collect::<Vec<_>>(), [(9..10, L)]);
    }
}
