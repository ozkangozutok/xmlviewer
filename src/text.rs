//! Decoding and light-weight parsing of the bytes that are on screen.

use std::borrow::Cow;

use encoding_rs::{Encoding, UTF_8};

/// Picks the document encoding from the BOM or the XML declaration.
pub fn detect_encoding(data: &[u8]) -> Result<&'static Encoding, String> {
    if data.starts_with(&[0xFF, 0xFE]) || data.starts_with(&[0xFE, 0xFF]) {
        return Err("UTF-16 files are not supported. Convert the file to UTF-8 and open it again.".into());
    }
    if data.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Ok(UTF_8);
    }
    let head = &data[..data.len().min(512)];
    if !head.starts_with(b"<?xml") {
        return Ok(UTF_8);
    }
    let decl_end = memchr::memmem::find(head, b"?>").unwrap_or(head.len());
    let decl = &head[..decl_end];
    let Some(p) = memchr::memmem::find(decl, b"encoding") else {
        return Ok(UTF_8);
    };
    let rest = &decl[p + 8..];
    let Some(q) = rest.iter().position(|&c| c == b'"' || c == b'\'') else {
        return Ok(UTF_8);
    };
    let quote = rest[q];
    let value = &rest[q + 1..];
    let value = &value[..memchr::memchr(quote, value).unwrap_or(value.len())];
    match Encoding::for_label(value) {
        Some(enc) if enc.is_ascii_compatible() => Ok(enc),
        Some(enc) => Err(format!("The {} encoding is not supported. Convert the file to UTF-8 and open it again.", enc.name())),
        None => Ok(UTF_8),
    }
}

pub fn decode<'a>(enc: &'static Encoding, bytes: &'a [u8]) -> Cow<'a, str> {
    enc.decode_without_bom_handling(bytes).0
}

/// Replaces the predefined entities and character references.
pub fn unescape(s: &str) -> Cow<'_, str> {
    if !s.contains('&') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find('&') {
        out.push_str(&rest[..p]);
        rest = &rest[p..];
        let Some(semi) = rest[..rest.len().min(12)].find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..semi];
        let ch = match ent {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if ent.starts_with("#x") || ent.starts_with("#X") => {
                u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32)
            }
            _ if ent.starts_with('#') => ent[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// Collapses whitespace runs and cuts to `max` characters.
pub fn compact(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max * 2));
    let mut count = 0;
    let mut space = false;
    for c in s.trim().chars() {
        if c.is_whitespace() {
            space = true;
            continue;
        }
        if space {
            out.push(' ');
            count += 1;
            space = false;
        }
        if count >= max {
            out.push('…');
            return out;
        }
        out.push(c);
        count += 1;
    }
    out
}

pub struct StartTag<'a> {
    pub name: &'a [u8],
    pub attrs: Vec<(&'a [u8], &'a [u8])>,
    pub self_closing: bool,
    /// Input ended or `max_attrs` was hit before the closing `>`.
    pub truncated: bool,
}

/// Parses `<name a="v" ...>` from the start of `src`. Tolerant of truncation.
pub fn parse_start_tag(src: &[u8], max_attrs: usize) -> StartTag<'_> {
    let ws = |b: u8| b.is_ascii_whitespace();
    let len = src.len();
    let mut i = usize::from(src.first() == Some(&b'<'));
    let ns = i;
    while i < len && !ws(src[i]) && src[i] != b'>' && src[i] != b'/' {
        i += 1;
    }
    let name = &src[ns..i];
    let mut attrs = Vec::new();
    let mut self_closing = false;
    let mut truncated = false;
    loop {
        while i < len && ws(src[i]) {
            i += 1;
        }
        if i >= len {
            truncated = true;
            break;
        }
        match src[i] {
            b'>' => break,
            b'/' => {
                self_closing = true;
                i += 1;
                continue;
            }
            _ => {}
        }
        if attrs.len() >= max_attrs {
            truncated = true;
            break;
        }
        let ks = i;
        while i < len && !ws(src[i]) && !matches!(src[i], b'=' | b'>' | b'/') {
            i += 1;
        }
        let key = &src[ks..i];
        while i < len && ws(src[i]) {
            i += 1;
        }
        let mut val: &[u8] = b"";
        if i < len && src[i] == b'=' {
            i += 1;
            while i < len && ws(src[i]) {
                i += 1;
            }
            if i < len && (src[i] == b'"' || src[i] == b'\'') {
                let q = src[i];
                let vs = i + 1;
                match memchr::memchr(q, &src[vs..]) {
                    Some(p) => {
                        val = &src[vs..vs + p];
                        i = vs + p + 1;
                    }
                    None => {
                        val = &src[vs..];
                        truncated = true;
                        i = len;
                    }
                }
            } else {
                let vs = i;
                while i < len && !ws(src[i]) && src[i] != b'>' && src[i] != b'/' {
                    i += 1;
                }
                val = &src[vs..i];
            }
        }
        if !key.is_empty() {
            attrs.push((key, val));
        }
    }
    StartTag { name, attrs, self_closing, truncated }
}

pub fn fmt_num(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn fmt_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{n} B") } else { format!("{v:.1} {}", UNITS[u]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_tag() {
        let t = parse_start_tag(b"<item id=\"1\" name='x y' flag=z/>", 10);
        assert_eq!(t.name, b"item");
        assert_eq!(t.attrs.len(), 3);
        assert_eq!(t.attrs[1], (&b"name"[..], &b"x y"[..]));
        assert!(t.self_closing);
        assert!(!t.truncated);
        let t = parse_start_tag(b"<a b=\"unterminated", 10);
        assert!(t.truncated);
    }

    #[test]
    fn entities() {
        assert_eq!(unescape("a &lt;b&gt; &amp; &#x15F;&#305; &foo;"), "a <b> & şı &foo;");
    }

    #[test]
    fn encodings() {
        assert_eq!(detect_encoding(b"<?xml version=\"1.0\" encoding=\"windows-1254\"?><a/>").unwrap().name(), "windows-1254");
        assert_eq!(detect_encoding(b"<?xml version='1.0' encoding='ISO-8859-9'?>").unwrap().name(), "windows-1254");
        assert_eq!(detect_encoding(b"<a/>").unwrap(), UTF_8);
        assert!(detect_encoding(&[0xFF, 0xFE, b'<', 0]).is_err());
        let enc = detect_encoding(b"<?xml version=\"1.0\" encoding=\"windows-1254\"?>").unwrap();
        assert_eq!(decode(enc, &[0xFE, 0xF0, 0xFD]), "şğı");
    }

    #[test]
    fn compacting() {
        assert_eq!(compact("  a \n\t b  ", 10), "a b");
        assert_eq!(compact("abcdef", 3), "abc…");
        assert_eq!(fmt_num(1234567), "1,234,567");
    }
}
