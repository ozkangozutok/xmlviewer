//! Streams a URL to a temporary file so it can be memory-mapped like any local file.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Default)]
pub struct Download {
    pub url: String,
    pub received: AtomicU64,
    /// 0 when the server sends no Content-Length.
    pub total: AtomicU64,
    pub cancel: AtomicBool,
    pub result: Mutex<Option<Result<PathBuf, String>>>,
}

pub fn dir() -> PathBuf {
    std::env::temp_dir().join("xmlviewer-downloads")
}

pub fn is_temp(path: &std::path::Path) -> bool {
    path.starts_with(dir())
}

/// Removes downloads left over from earlier runs; files still open elsewhere are skipped.
pub fn clean_old() {
    if let Ok(entries) = fs::read_dir(dir()) {
        for e in entries.flatten() {
            let _ = fs::remove_file(e.path());
        }
    }
}

pub fn normalize_url(input: &str) -> Result<String, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("URL boş".into());
    }
    let s = if s.contains("://") { s.to_string() } else { format!("https://{s}") };
    if !(s.starts_with("http://") || s.starts_with("https://")) {
        return Err("Yalnızca http:// ve https:// adresleri desteklenir".into());
    }
    Ok(s)
}

fn file_name(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let last = path.rsplit('/').find(|s| !s.is_empty() && !s.contains(':')).unwrap_or("indirilen");
    let clean: String = last
        .chars()
        .map(|c| if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
        .take(80)
        .collect();
    let clean = if clean.contains('.') { clean } else { format!("{clean}.xml") };
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
    format!("{stamp}-{clean}")
}

pub fn start(url: String, ctx: eframe::egui::Context) -> Arc<Download> {
    let job = Arc::new(Download { url, ..Default::default() });
    let j = job.clone();
    thread::spawn(move || {
        let r = fetch(&j);
        if r.is_err() {
            j.received.store(0, Ordering::Relaxed);
        }
        *j.result.lock().unwrap() = Some(r);
        ctx.request_repaint();
    });
    job
}

fn fetch(job: &Download) -> Result<PathBuf, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(120))
        .user_agent(concat!("xmlviewer/", env!("CARGO_PKG_VERSION")))
        .build();
    let resp = match agent.get(&job.url).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => return Err(format!("Sunucu {code} {} döndürdü", r.status_text())),
        Err(e) => return Err(e.to_string()),
    };
    if let Some(len) = resp.header("Content-Length").and_then(|v| v.parse().ok()) {
        job.total.store(len, Ordering::Relaxed);
    }
    fs::create_dir_all(dir()).map_err(|e| e.to_string())?;
    let path = dir().join(file_name(resp.get_url()));
    let mut out = File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut reader = resp.into_reader();
    let mut buf = vec![0u8; 1 << 20];
    let res = loop {
        if job.cancel.load(Ordering::Relaxed) {
            break Err("İndirme iptal edildi".to_string());
        }
        match reader.read(&mut buf) {
            Ok(0) => break Ok(()),
            Ok(n) => {
                if let Err(e) = out.write_all(&buf[..n]) {
                    break Err(format!("Diske yazılamadı: {e}"));
                }
                job.received.fetch_add(n as u64, Ordering::Relaxed);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => break Err(format!("İndirme kesildi: {e}")),
        }
    };
    drop(out);
    match res {
        Ok(()) => Ok(path),
        Err(e) => {
            let _ = fs::remove_file(&path);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(normalize_url(" example.com/a.xml ").unwrap(), "https://example.com/a.xml");
        assert!(normalize_url("ftp://x/y").is_err());
        assert!(file_name("https://x.com/data/feed.xml?k=1").ends_with("-feed.xml"));
        assert!(file_name("https://x.com/").ends_with("-x.com"));
        assert!(file_name("https://x.com/api/items").ends_with("-items.xml"));
    }
}
