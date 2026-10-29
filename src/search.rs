//! Background full-text search over the raw mapped bytes.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use encoding_rs::{Encoding, UTF_8};
use regex::bytes::{Regex, RegexBuilder};

pub const MAX_RESULTS: usize = 1_000_000;
const CHUNK: usize = 16 << 20;
/// Matches longer than this that straddle a chunk boundary can be missed.
const OVERLAP: usize = 64 << 10;

#[derive(Default)]
pub struct SearchJob {
    /// Byte offsets of match starts, ascending.
    pub results: Mutex<Vec<u64>>,
    pub progress: AtomicU64,
    pub done: AtomicBool,
    pub cancel: AtomicBool,
    pub truncated: AtomicBool,
}

pub fn build_regex(pattern: &str, use_regex: bool, case_sensitive: bool, enc: &'static Encoding) -> Result<Regex, String> {
    if pattern.is_empty() {
        return Err("Arama metni boş".into());
    }
    let source = if use_regex || enc == UTF_8 {
        if use_regex { pattern.to_string() } else { regex::escape(pattern) }
    } else {
        // Plain text in a legacy encoding: match the encoded bytes exactly.
        let (bytes, _, _) = enc.encode(pattern);
        let mut s = String::from("(?-u)");
        for b in bytes.iter() {
            if b.is_ascii_alphanumeric() {
                s.push(*b as char);
            } else {
                s.push_str(&format!("\\x{b:02X}"));
            }
        }
        s
    };
    RegexBuilder::new(&source)
        .case_insensitive(!case_sensitive)
        .size_limit(64 << 20)
        .build()
        .map_err(|e| e.to_string())
}

pub fn run(data: &[u8], re: &Regex, job: &SearchJob) {
    let mut start = 0usize;
    let mut batch = Vec::new();
    let mut total = 0usize;
    'outer: while start < data.len() {
        let nominal_end = (start + CHUNK).min(data.len());
        let window_end = (nominal_end + OVERLAP).min(data.len());
        for m in re.find_iter(&data[start..window_end]) {
            let abs = start + m.start();
            if abs >= nominal_end {
                break;
            }
            batch.push(abs as u64);
            total += 1;
            if total >= MAX_RESULTS {
                job.truncated.store(true, Ordering::Relaxed);
                break 'outer;
            }
        }
        if !batch.is_empty() {
            job.results.lock().unwrap().append(&mut batch);
        }
        start = nominal_end;
        job.progress.store(start as u64, Ordering::Relaxed);
        if job.cancel.load(Ordering::Relaxed) {
            break;
        }
    }
    if !batch.is_empty() {
        job.results.lock().unwrap().append(&mut batch);
    }
    job.progress.store(data.len() as u64, Ordering::Relaxed);
    job.done.store(true, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(hay: &[u8], pat: &str, re: bool, cs: bool, enc: &'static Encoding) -> Vec<u64> {
        let r = build_regex(pat, re, cs, enc).unwrap();
        let job = SearchJob::default();
        run(hay, &r, &job);
        job.results.into_inner().unwrap()
    }

    #[test]
    fn plain_and_case() {
        let hay = "<a>Şehir şehir ŞEHİR</a>".as_bytes();
        assert_eq!(find(hay, "şehir", false, true, UTF_8).len(), 1);
        assert!(find(hay, "şehir", false, false, UTF_8).len() >= 2);
        assert_eq!(find(hay, "<a>", false, true, UTF_8), vec![0]);
    }

    #[test]
    fn legacy_encoding() {
        let enc = Encoding::for_label(b"windows-1254").unwrap();
        let (hay, _, _) = enc.encode("<a>Ağaç</a>");
        assert_eq!(find(&hay, "Ağaç", false, true, enc), vec![3]);
    }

    #[test]
    fn chunk_boundary() {
        let mut hay = vec![b'x'; CHUNK - 2];
        hay.extend_from_slice(b"needle");
        hay.extend(vec![b'y'; 100]);
        assert_eq!(find(&hay, "needle", false, true, UTF_8), vec![(CHUNK - 2) as u64]);
    }
}
