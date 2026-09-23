//! 用户输入的页码范围：「1-3，5、8-」这样的写法。
//!
//! 中文输入法下逗号、顿号、波浪线常常是全角的，照样认；页码从 1 开始，与阅读器里
//! 看到的一致。

/// 把 `text` 解析成页码列表（从 1 开始，按写的先后，去重）。空白表示全部 `count` 页。
///
/// - `5-` 表示第 5 页到最后，`-3` 表示第 1 到第 3 页；
/// - 分隔符：半角/全角逗号、顿号、分号、空白；范围符：`-`、`–`、`—`、`~`、`～`。
///
/// 出错时返回一句能直接给用户看的话。
pub fn parse(text: &str, count: usize) -> Result<Vec<usize>, String> {
    if text.trim().is_empty() {
        return Ok((1..=count).collect());
    }
    let mut pages = Vec::new();
    let mut seen = vec![false; count + 1];
    for (from, to) in spans(text)? {
        let to = to.unwrap_or(count);
        for n in [from, to] {
            if n > count {
                return Err(format!("没有第 {n} 页（共 {count} 页）"));
            }
        }
        for (p, seen) in (from..=to).zip(&mut seen[from..=to]) {
            if !*seen {
                *seen = true;
                pages.push(p);
            }
        }
    }
    if pages.is_empty() {
        return Err("没有选中任何一页".into());
    }
    Ok(pages)
}

/// 只看写法对不对，不管页数（还没打开文件时用）。
pub fn check(text: &str) -> Result<(), String> {
    spans(text).map(|_| ())
}

/// 逐段拆成 (起, 止)，止为 None 表示到最后一页。
fn spans(text: &str) -> Result<Vec<(usize, Option<usize>)>, String> {
    let is_sep = |c: char| matches!(c, ',' | '，' | '、' | ';' | '；') || c.is_whitespace();
    let is_dash = |c: char| matches!(c, '-' | '–' | '—' | '~' | '～');
    let mut spans = Vec::new();
    for part in text.split(is_sep).filter(|p| !p.is_empty()) {
        let span = match part.split_once(is_dash) {
            None => {
                let n = number(part)?;
                (n, Some(n))
            }
            Some((a, b)) => {
                let from = if a.trim().is_empty() { 1 } else { number(a)? };
                let to = if b.trim().is_empty() {
                    None
                } else {
                    Some(number(b)?)
                };
                if to.is_some_and(|to| from > to) {
                    return Err(format!("「{part}」的起始页比结束页大"));
                }
                (from, to)
            }
        };
        spans.push(span);
    }
    if spans.is_empty() && !text.trim().is_empty() {
        return Err("没有选中任何一页".into());
    }
    Ok(spans)
}

fn number(s: &str) -> Result<usize, String> {
    let s = s.trim();
    match s.parse::<usize>() {
        Ok(0) => Err("页码从 1 开始".into()),
        Ok(n) => Ok(n),
        Err(_) => Err(format!("「{s}」不是页码")),
    }
}

#[cfg(test)]
mod tests {
    use super::{check, parse};

    #[test]
    fn ranges_lists_and_open_ends() {
        assert_eq!(parse("", 4), Ok(vec![1, 2, 3, 4]));
        assert_eq!(parse("  ", 2), Ok(vec![1, 2]));
        assert_eq!(parse("2", 5), Ok(vec![2]));
        assert_eq!(parse("1-3,5", 6), Ok(vec![1, 2, 3, 5]));
        assert_eq!(parse("4-", 6), Ok(vec![4, 5, 6]));
        assert_eq!(parse("-2", 6), Ok(vec![1, 2]));
        // 按写的先后，重复的只留第一次。
        assert_eq!(parse("5, 1-2, 2, 5", 6), Ok(vec![5, 1, 2]));
    }

    #[test]
    fn full_width_punctuation_from_chinese_input_methods() {
        assert_eq!(parse("1～3，5、7；8", 9), Ok(vec![1, 2, 3, 5, 7, 8]));
        assert_eq!(parse("2—3 6", 9), Ok(vec![2, 3, 6]));
    }

    #[test]
    fn mistakes_are_explained() {
        assert_eq!(parse("0", 3), Err("页码从 1 开始".into()));
        assert_eq!(parse("2-9", 3), Err("没有第 9 页（共 3 页）".into()));
        assert_eq!(parse("3-1", 3), Err("「3-1」的起始页比结束页大".into()));
        assert_eq!(parse("a", 3), Err("「a」不是页码".into()));
        assert_eq!(parse(",", 3), Err("没有选中任何一页".into()));
        // 开放的结尾从最后一页之后开始。
        assert_eq!(parse("5-", 3), Err("没有第 5 页（共 3 页）".into()));
    }

    /// 还不知道页数时只查写法：超出页数的要等打开文件才知道。
    #[test]
    fn syntax_is_checked_without_a_page_count() {
        assert_eq!(check(""), Ok(()));
        assert_eq!(check("1-3, 900-"), Ok(()));
        assert_eq!(check("3-1"), Err("「3-1」的起始页比结束页大".into()));
        assert_eq!(check("x"), Err("「x」不是页码".into()));
    }
}
