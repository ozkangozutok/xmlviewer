//! Single-pass structural index for JSON (and JSON Lines: several top-level
//! values in a row). Produces the same node array as the XML indexer:
//! objects and arrays are containers, scalars are leaves, and an object
//! member's node starts at its key so labels can show `"key": value`.

use std::borrow::Cow;
use std::sync::atomic::Ordering;

use crate::index::{Builder, Index, Kind, NONE, Progress, TOO_MANY, bom_len, parse_error};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// After `{`: a key or `}`.
    FirstKey,
    Key,
    Colon,
    /// After `[`: a value or `]`.
    FirstValue,
    Value,
    /// After a value: `,` or the closing bracket.
    Next,
}

/// Index one past the closing quote of the string starting at `i`.
pub fn string_end(data: &[u8], i: usize) -> Option<usize> {
    let mut p = i + 1;
    loop {
        let q = p + memchr::memchr2(b'"', b'\\', &data[p..])?;
        if data[q] == b'"' {
            return Some(q + 1);
        }
        p = q + 2;
        if p > data.len() {
            return None;
        }
    }
}

fn is_delim(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b',' | b'}' | b']' | b':')
}

pub fn build(data: &[u8], progress: &Progress) -> Index {
    let len = data.len();
    let mut b = Builder::default();
    // Per open container: (is object, what comes next).
    let mut frames: Vec<(bool, Expect)> = Vec::new();
    let mut key_start: Option<u64> = None;
    let mut error = None;
    let mut cancelled = false;
    let mut steps: u32 = 0;
    let mut i = bom_len(data);

    macro_rules! fail {
        ($msg:expr) => {{
            error = Some(parse_error(data, i as u64, $msg.to_string()));
            break;
        }};
    }
    macro_rules! add {
        ($kind:expr, $start:expr, $end:expr) => {
            match b.add($kind, $start, $end) {
                Some(id) => id,
                None => fail!(TOO_MANY),
            }
        };
    }

    loop {
        while i < len && matches!(data[i], b' ' | b'\t' | b'\n' | b'\r') {
            i += 1;
        }
        if i >= len {
            break;
        }
        let expect = frames.last().map_or(Expect::Value, |f| f.1);
        let wants_value = matches!(expect, Expect::Value | Expect::FirstValue);
        let c = data[i];
        match c {
            b'{' | b'[' => {
                if !wants_value {
                    fail!(format!("unexpected '{}'", c as char));
                }
                let start = key_start.take().unwrap_or(i as u64);
                let kind = if c == b'{' { Kind::Object } else { Kind::Array };
                let id = add!(kind, start, i as u64 + 1);
                if let Some(f) = frames.last_mut() {
                    f.1 = Expect::Next;
                }
                b.stack.push((id, NONE));
                frames.push((c == b'{', if c == b'{' { Expect::FirstKey } else { Expect::FirstValue }));
                i += 1;
            }
            b'}' | b']' => {
                let Some(&(obj, e)) = frames.last() else { fail!(format!("unexpected '{}'", c as char)) };
                if obj != (c == b'}') {
                    fail!(format!("expected '{}' but found '{}'", if obj { '}' } else { ']' }, c as char));
                }
                if !matches!(e, Expect::Next | Expect::FirstKey | Expect::FirstValue) {
                    fail!(format!("unexpected '{}'", c as char));
                }
                frames.pop();
                b.close(i as u64 + 1);
                i += 1;
            }
            b'"' => {
                let Some(end) = string_end(data, i) else { fail!("unterminated string") };
                match expect {
                    Expect::Key | Expect::FirstKey => {
                        key_start = Some(i as u64);
                        frames.last_mut().unwrap().1 = Expect::Colon;
                    }
                    Expect::Value | Expect::FirstValue => {
                        let start = key_start.take().unwrap_or(i as u64);
                        add!(Kind::Value, start, end as u64);
                        if let Some(f) = frames.last_mut() {
                            f.1 = Expect::Next;
                        }
                    }
                    _ => fail!("unexpected string"),
                }
                i = end;
            }
            b':' => {
                if expect != Expect::Colon {
                    fail!("unexpected ':'");
                }
                frames.last_mut().unwrap().1 = Expect::Value;
                i += 1;
            }
            b',' => {
                let Some(f) = frames.last_mut() else { fail!("unexpected ','") };
                if f.1 != Expect::Next {
                    fail!("unexpected ','");
                }
                f.1 = if f.0 { Expect::Key } else { Expect::Value };
                i += 1;
            }
            _ => {
                if !wants_value {
                    fail!(format!("unexpected '{}'", char::from(c)));
                }
                if !matches!(c, b'-' | b'0'..=b'9' | b't' | b'f' | b'n') {
                    fail!(format!("invalid value starting with '{}'", char::from(c)));
                }
                let s = i;
                while i < len && !is_delim(data[i]) {
                    i += 1;
                }
                let start = key_start.take().unwrap_or(s as u64);
                add!(Kind::Value, start, i as u64);
                if let Some(f) = frames.last_mut() {
                    f.1 = Expect::Next;
                }
            }
        }
        steps = steps.wrapping_add(1);
        if steps & 0x3FFF == 0 {
            progress.bytes.store(i as u64, Ordering::Relaxed);
            if progress.cancel.load(Ordering::Relaxed) {
                cancelled = true;
                break;
            }
        }
    }
    if error.is_none() && !cancelled && !frames.is_empty() {
        error = Some(parse_error(data, len as u64, "unexpected end of file".into()));
    }
    b.finish(data, progress, error, cancelled, true)
}

/// Decodes JSON string escapes (the bytes between the quotes).
pub fn unescape(raw: &str) -> Cow<'_, str> {
    if !raw.contains('\\') {
        return Cow::Borrowed(raw);
    }
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                let Ok(hi) = u32::from_str_radix(&hex, 16) else {
                    out.push_str("\\u");
                    out.push_str(&hex);
                    continue;
                };
                let code = if (0xD800..0xDC00).contains(&hi) {
                    // Surrogate pair: expect \uDC00-\uDFFF next.
                    let rest = chars.as_str();
                    match rest.strip_prefix("\\u").and_then(|r| r.get(..4)).and_then(|h| u32::from_str_radix(h, 16).ok()) {
                        Some(lo) if (0xDC00..0xE000).contains(&lo) => {
                            chars = rest[6..].chars();
                            0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                        }
                        _ => 0xFFFD,
                    }
                } else {
                    hi
                };
                out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
            }
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    Cow::Owned(out)
}

/// For a node that may start with `"key":`, returns (key bytes without quotes, value offset).
pub fn split_member(data: &[u8], start: usize, end: usize, has_key: bool) -> (Option<&[u8]>, usize) {
    if !has_key || data.get(start) != Some(&b'"') {
        return (None, start);
    }
    let Some(kend) = string_end(&data[..end], start) else { return (None, start) };
    let mut v = kend;
    while v < end && (data[v].is_ascii_whitespace() || data[v] == b':') {
        v += 1;
    }
    (Some(&data[start + 1..kend - 1]), v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idx(s: &str) -> Index {
        crate::index::build(s.as_bytes(), &Progress::default())
    }

    fn src<'a>(s: &'a str, i: &Index, id: u32) -> &'a str {
        &s[i.node(id).start() as usize..i.node(id).end() as usize]
    }

    #[test]
    fn structure() {
        let s = r#"{"ad": "Şehir \"x\"", "n": -1.5e3, "liste": [1, true, null, {"a": {}}], "boş": []}"#;
        let i = idx(s);
        assert!(i.json);
        assert!(i.error.is_none(), "{:?}", i.error);
        let root = i.top[0];
        assert_eq!(i.node(root).kind(), Kind::Object);
        assert_eq!(i.node(root).child_count, 4);
        assert_eq!(src(s, &i, 1), r#""ad": "Şehir \"x\"""#);
        assert_eq!(src(s, &i, 2), r#""n": -1.5e3"#);
        let liste = i.node(2).next_sibling;
        assert_eq!(i.node(liste).kind(), Kind::Array);
        assert_eq!(i.node(liste).child_count, 4);
        assert!(src(s, &i, liste).ends_with("}}]"));
        let bos = i.node(liste).next_sibling;
        assert!(!i.is_expandable(bos));
        let (key, v) = split_member(s.as_bytes(), i.node(bos).start() as usize, i.node(bos).end() as usize, true);
        assert_eq!(key.unwrap(), "boş".as_bytes());
        assert_eq!(&s[v..i.node(bos).end() as usize], "[]");
    }

    #[test]
    fn json_lines() {
        let i = idx("{\"a\":1}\n{\"a\":2}\n[3]\n");
        assert!(i.error.is_none());
        assert_eq!(i.top.len(), 3);
    }

    #[test]
    fn errors() {
        let s = "{\n  \"a\": 1,\n  \"b\" 2\n}";
        let e = idx(s).error.unwrap();
        assert_eq!((e.line, e.column), (3, 7));
        let i = idx("[1, 2, [3");
        assert_eq!(i.error.as_ref().unwrap().message, "unexpected end of file");
        assert_eq!(i.node(i.top[0]).end(), 9);
        assert!(idx("[1,]").error.is_some());
        assert!(idx("{\"a\" : 1]").error.is_some());
    }

    #[test]
    fn escapes() {
        assert_eq!(unescape(r#"a\n\"b\" şı 😀"#), "a\n\"b\" şı 😀");
        assert_eq!(string_end(br#""a\\" x"#, 0), Some(5));
        assert_eq!(string_end(br#""a\"b"#, 0), None);
    }
}
