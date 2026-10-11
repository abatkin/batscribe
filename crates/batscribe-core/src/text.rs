//! Position conversion between Rust UTF-8 byte offsets and Qt UTF-16 code-unit offsets, and
//! small text helpers shared by the editor and the voice pipeline.

/// Length of `s` in UTF-16 code units (Qt string positions).
pub fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// Byte offset in `s` of UTF-16 position `pos`, or `None` when `pos` is past the end or splits a
/// surrogate pair.
pub fn utf16_to_byte(s: &str, pos: usize) -> Option<usize> {
    let mut units = 0;
    for (byte, ch) in s.char_indices() {
        if units == pos {
            return Some(byte);
        }
        units += ch.len_utf16();
        if units > pos {
            return None;
        }
    }
    (units == pos).then_some(s.len())
}

/// UTF-16 position of byte offset `byte` in `s`. Offsets inside a character round down.
pub fn byte_to_utf16(s: &str, byte: usize) -> usize {
    let mut byte = byte.min(s.len());
    while !s.is_char_boundary(byte) {
        byte -= 1;
    }
    utf16_len(&s[..byte])
}

/// Converts every line ending (CRLF, CR, and Qt's paragraph/line separators) to `\n`.
pub fn normalize_newlines(s: &str) -> String {
    let s = s.replace("\r\n", "\n");
    s.chars()
        .map(|c| match c {
            '\r' | '\u{2029}' | '\u{2028}' => '\n',
            c => c,
        })
        .collect()
}

/// Smallest replacement turning `old` into `new`: `(start, old_end, new_end)` byte offsets, so
/// `old[start..old_end]` becomes `new[start..new_end]`. Boundaries fall on characters.
pub fn minimal_diff(old: &str, new: &str) -> (usize, usize, usize) {
    let mut prefix = 0;
    for ((ia, a), (_, b)) in old.char_indices().zip(new.char_indices()) {
        if a != b {
            break;
        }
        prefix = ia + a.len_utf8();
    }
    let mut suffix = 0;
    for (a, b) in old[prefix..].chars().rev().zip(new[prefix..].chars().rev()) {
        if a != b {
            break;
        }
        suffix += a.len_utf8();
    }
    (prefix, old.len() - suffix, new.len() - suffix)
}

/// Number of whitespace-separated words.
pub fn word_count(s: &str) -> usize {
    s.split_whitespace().count()
}

/// Byte offset just past the first complete sentence in `s` (its terminal punctuation, any
/// closing quotes/brackets, and the following whitespace), if the sentence is followed by
/// whitespace or ends the text.
pub fn first_sentence_end(s: &str) -> Option<usize> {
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if !matches!(c, '.' | '?' | '!') {
            continue;
        }
        let mut end = i + c.len_utf8();
        while let Some(&(j, d)) = chars.peek() {
            if matches!(d, '"' | '\'' | ')' | ']' | '”' | '’' | '.' | '?' | '!') {
                end = j + d.len_utf8();
                chars.next();
            } else {
                break;
            }
        }
        match chars.peek() {
            None => return Some(end),
            Some(&(_, d)) if d.is_whitespace() => {
                let rest = &s[end..];
                let ws = rest.len() - rest.trim_start().len();
                return Some(end + ws);
            }
            // "3.5", "e.g.x": not a sentence end.
            Some(_) => {}
        }
    }
    None
}

/// Number of complete sentences in `s`.
pub fn sentence_count(s: &str) -> usize {
    let mut rest = s;
    let mut n = 0;
    while let Some(end) = first_sentence_end(rest) {
        n += 1;
        rest = &rest[end..];
        if rest.is_empty() {
            break;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_conversions() {
        let s = "a😀é\nz";
        assert_eq!(utf16_len(s), 6);
        assert_eq!(utf16_to_byte(s, 0), Some(0));
        assert_eq!(utf16_to_byte(s, 1), Some(1));
        assert_eq!(utf16_to_byte(s, 2), None, "inside the surrogate pair");
        assert_eq!(utf16_to_byte(s, 3), Some(5));
        assert_eq!(utf16_to_byte(s, 4), Some(7));
        assert_eq!(utf16_to_byte(s, 6), Some(s.len()));
        assert_eq!(utf16_to_byte(s, 7), None);
        assert_eq!(byte_to_utf16(s, 5), 3);
        assert_eq!(byte_to_utf16(s, 2), 1, "inside a char rounds down");
    }

    #[test]
    fn newline_normalization() {
        assert_eq!(normalize_newlines("a\r\nb\rc\u{2029}d"), "a\nb\nc\nd");
    }

    #[test]
    fn diffs() {
        assert_eq!(minimal_diff("abc", "abc"), (3, 3, 3));
        assert_eq!(minimal_diff("seven oranges", "six oranges"), (1, 5, 3));
        assert_eq!(minimal_diff("", "x"), (0, 0, 1));
        assert_eq!(minimal_diff("aé", "aè"), (1, 3, 3));
        let (p, o, n) = minimal_diff("aaa", "aaaa");
        assert_eq!((p, o, n), (3, 3, 4));
    }

    #[test]
    fn sentences() {
        assert_eq!(first_sentence_end("Hello there. Next"), Some(13));
        assert_eq!(first_sentence_end("Pi is 3.14 today"), None);
        assert_eq!(first_sentence_end("He said \"stop.\" Then"), Some(16));
        assert_eq!(first_sentence_end("Done!"), Some(5));
        assert_eq!(sentence_count("One. Two? Three"), 2);
        assert_eq!(word_count("  a b\nc "), 3);
    }
}
