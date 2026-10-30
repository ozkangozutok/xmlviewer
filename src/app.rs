use std::collections::HashSet;
use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Color32, FontId, Key, Pos2, Rect, Sense, Shape, Stroke, TextFormat, Vec2, text::LayoutJob,
};
use encoding_rs::Encoding;
use memmap2::Mmap;

use crate::download::{self, Download};
use crate::json;
use crate::index::{self, Index, Kind, NONE, Progress};
use crate::search::{self, SearchJob};
use crate::text::{self, compact, decode, fmt_bytes, fmt_num, unescape};

const ROW_H: f32 = 20.0;
const FONT_SIZE: f32 = 13.0;
const INDENT: f32 = 16.0;
const SCROLLBAR_W: f32 = 14.0;
const LABEL_SRC_MAX: usize = 4096;
const DETAIL_SRC_MAX: usize = 32 * 1024;

#[derive(Clone, Copy)]
struct Row {
    node: u32,
    depth: u32,
}

struct Palette {
    punct: Color32,
    tag: Color32,
    attr: Color32,
    value: Color32,
    text: Color32,
    comment: Color32,
    weak: Color32,
}

impl Palette {
    fn new(visuals: &egui::Visuals) -> Self {
        if visuals.dark_mode {
            Self {
                punct: Color32::from_rgb(128, 128, 128),
                tag: Color32::from_rgb(86, 156, 214),
                attr: Color32::from_rgb(156, 220, 254),
                value: Color32::from_rgb(206, 145, 120),
                text: visuals.text_color(),
                comment: Color32::from_rgb(106, 153, 85),
                weak: visuals.weak_text_color(),
            }
        } else {
            Self {
                punct: Color32::from_rgb(128, 128, 128),
                tag: Color32::from_rgb(0, 0, 170),
                attr: Color32::from_rgb(160, 40, 40),
                value: Color32::from_rgb(20, 110, 20),
                text: visuals.text_color(),
                comment: Color32::from_rgb(0, 128, 0),
                weak: visuals.weak_text_color(),
            }
        }
    }
}

enum State {
    Indexing { progress: Arc<Progress>, rx: mpsc::Receiver<(Index, f64)> },
    Ready { index: Arc<Index>, secs: f64 },
}

struct Detail {
    node: u32,
    path: String,
    attrs: Vec<(String, String)>,
    source: String,
    source_truncated: bool,
}

struct Doc {
    path: PathBuf,
    /// URL the file was downloaded from.
    source: Option<String>,
    data: Arc<Mmap>,
    enc: &'static Encoding,
    opened: Instant,
    state: State,
    rows: Vec<Row>,
    expanded: HashSet<u32>,
    top: usize,
    scroll_accum: f32,
    selected: Option<usize>,
    detail: Option<Detail>,
    search: Option<Arc<SearchJob>>,
    search_pos: Option<usize>,
}

impl Doc {
    fn open(path: PathBuf, ctx: &egui::Context) -> Result<Self, String> {
        let file = File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let len = file.metadata().map_err(|e| e.to_string())?.len();
        if len >= index::MAX_FILE_SIZE {
            return Err(format!("{}: files up to 1 TB are supported", path.display()));
        }
        if len == 0 {
            return Err(format!("{}: file is empty", path.display()));
        }
        // SAFETY: the file is opened read-only; Windows prevents truncation while mapped.
        let mmap = unsafe { Mmap::map(&file) }.map_err(|e| format!("{}: {e}", path.display()))?;
        let enc = text::detect_encoding(&mmap)?;
        let data = Arc::new(mmap);
        let progress = Arc::new(Progress::default());
        let (tx, rx) = mpsc::channel();
        {
            let data = data.clone();
            let progress = progress.clone();
            let ctx = ctx.clone();
            thread::spawn(move || {
                let t = Instant::now();
                let index = index::build(&data, &progress);
                let _ = tx.send((index, t.elapsed().as_secs_f64()));
                ctx.request_repaint();
            });
        }
        Ok(Self {
            path,
            source: None,
            data,
            enc,
            opened: Instant::now(),
            state: State::Indexing { progress, rx },
            rows: Vec::new(),
            expanded: HashSet::new(),
            top: 0,
            scroll_accum: 0.0,
            selected: None,
            detail: None,
            search: None,
            search_pos: None,
        })
    }

    fn cancel(&self) {
        if let State::Indexing { progress, .. } = &self.state {
            progress.cancel.store(true, Ordering::Relaxed);
        }
        if let Some(job) = &self.search {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn index(&self) -> Option<Arc<Index>> {
        match &self.state {
            State::Ready { index, .. } => Some(index.clone()),
            _ => None,
        }
    }

    fn poll(&mut self) {
        let State::Indexing { rx, .. } = &self.state else { return };
        let Ok((index, secs)) = rx.try_recv() else { return };
        let index = Arc::new(index);
        self.rows = index.top.iter().map(|&node| Row { node, depth: 0 }).collect();
        self.state = State::Ready { index: index.clone(), secs };
        if let Some(p) = self.rows.iter().position(|r| index.is_expandable(r.node)) {
            self.expand(&index, p);
            self.selected = Some(p);
        }
    }

    // ---- tree rows -------------------------------------------------------

    fn collect_rows(&self, index: &Index, node: u32, depth: u32, out: &mut Vec<Row>) {
        let mut stack = vec![(index.first_child(node), depth)];
        while let Some((c, d)) = stack.pop() {
            if c == NONE {
                continue;
            }
            out.push(Row { node: c, depth: d });
            stack.push((index.node(c).next_sibling, d));
            if self.expanded.contains(&c) && index.is_expandable(c) {
                stack.push((index.first_child(c), d + 1));
            }
        }
    }

    fn is_open(&self, node: u32) -> bool {
        self.expanded.contains(&node)
    }

    fn expand(&mut self, index: &Index, p: usize) {
        let r = self.rows[p];
        if !index.is_expandable(r.node) || self.is_open(r.node) {
            return;
        }
        self.expanded.insert(r.node);
        let mut new_rows = Vec::new();
        self.collect_rows(index, r.node, r.depth + 1, &mut new_rows);
        let n = new_rows.len();
        self.rows.splice(p + 1..p + 1, new_rows);
        if let Some(s) = self.selected.as_mut() {
            if *s > p {
                *s += n;
            }
        }
        if self.top > p {
            self.top += n;
        }
    }

    fn collapse(&mut self, p: usize) {
        let r = self.rows[p];
        if !self.expanded.remove(&r.node) {
            return;
        }
        let mut e = p + 1;
        while e < self.rows.len() && self.rows[e].depth > r.depth {
            e += 1;
        }
        let n = e - p - 1;
        self.rows.drain(p + 1..e);
        if let Some(s) = self.selected.as_mut() {
            if *s > p && *s < e {
                *s = p;
            } else if *s >= e {
                *s -= n;
            }
        }
        if self.top > p {
            self.top = if self.top < e { p } else { self.top - n };
        }
    }

    fn toggle(&mut self, index: &Index, p: usize) {
        if self.is_open(self.rows[p].node) {
            self.collapse(p);
        } else {
            self.expand(index, p);
        }
    }

    fn row_of(&self, node: u32) -> Option<usize> {
        // Visible rows are a subsequence of preorder, so node ids are sorted.
        self.rows.binary_search_by_key(&node, |r| r.node).ok()
    }

    fn reveal(&mut self, index: &Index, mut node: u32, visible_rows: usize) {
        let parent = index.node(node).parent;
        if parent != NONE && index.is_inline(parent) {
            node = parent;
        }
        for a in index.ancestors(node) {
            if let Some(p) = self.row_of(a) {
                self.expand(index, p);
            }
        }
        if let Some(p) = self.row_of(node) {
            self.selected = Some(p);
            self.top = p.saturating_sub(visible_rows / 3);
        }
    }

    fn ensure_visible(&mut self, visible_rows: usize) {
        if let Some(s) = self.selected {
            if s < self.top {
                self.top = s;
            } else if s >= self.top + visible_rows {
                self.top = s + 1 - visible_rows;
            }
        }
    }

    // ---- labels and details ------------------------------------------------

    fn slice(&self, start: u64, end: u64, max: usize) -> &[u8] {
        let s = start as usize;
        let e = (end as usize).min(s + max).min(self.data.len());
        &self.data[s..e]
    }

    fn element_name(&self, index: &Index, id: u32) -> String {
        let n = index.node(id);
        let tag = text::parse_start_tag(self.slice(n.start(), n.end(), 512), 0);
        decode(self.enc, tag.name).into_owned()
    }

    fn text_content(&self, index: &Index, id: u32, max_chars: usize) -> String {
        let n = index.node(id);
        let raw = self.slice(n.start(), n.end(), max_chars * 4 + 16);
        match n.kind() {
            Kind::CData => {
                let inner = raw.strip_prefix(b"<![CDATA[").unwrap_or(raw);
                let inner = inner.strip_suffix(b"]]>").unwrap_or(inner);
                compact(&decode(self.enc, inner), max_chars)
            }
            _ => compact(&unescape(&decode(self.enc, raw)), max_chars),
        }
    }

    /// Key and value offset of a JSON node (the key only for object members).
    fn json_member(&self, index: &Index, id: u32) -> (Option<String>, u64) {
        let n = index.node(id);
        let p = n.parent;
        let has_key = p != NONE && index.node(p).kind() == Kind::Object;
        let end = (n.end() as usize).min(n.start() as usize + LABEL_SRC_MAX);
        let (key, v) = json::split_member(&self.data, n.start() as usize, end, has_key);
        let key = key.map(|k| json::unescape(&String::from_utf8_lossy(k)).into_owned());
        (key, v as u64)
    }

    /// Display text of a JSON scalar: strings unescaped (without quotes).
    fn json_scalar(&self, index: &Index, id: u32, max_chars: usize) -> (String, bool) {
        let n = index.node(id);
        let (_, vs) = self.json_member(index, id);
        let raw = String::from_utf8_lossy(self.slice(vs, n.end(), max_chars * 4 + 16));
        match raw.strip_prefix('"') {
            Some(s) => {
                let s = s.strip_suffix('"').unwrap_or(s);
                (compact(&json::unescape(s), max_chars), true)
            }
            None => (compact(&raw, max_chars), false),
        }
    }

    /// First few members of a container, e.g. `id: 7, name: "x", detail: {…} …`.
    fn json_preview(&self, index: &Index, id: u32) -> String {
        let mut parts = Vec::new();
        let mut c = index.first_child(id);
        while c != NONE && parts.len() < 5 {
            let (key, _) = self.json_member(index, c);
            let value = match index.node(c).kind() {
                Kind::Object => "{…}".to_string(),
                Kind::Array => "[…]".to_string(),
                _ => match self.json_scalar(index, c, 40) {
                    (s, true) => format!("\"{s}\""),
                    (s, false) => s,
                },
            };
            parts.push(match key {
                Some(k) => format!("{}: {value}", compact(&k, 30)),
                None => value,
            });
            c = index.node(c).next_sibling;
        }
        let more = if c != NONE { ", …" } else { "" };
        compact(&format!("{}{more}", parts.join(", ")), 160)
    }

    fn json_job(&self, index: &Index, id: u32, pal: &Palette, push: &mut dyn FnMut(&str, Color32)) {
        let n = index.node(id);
        let (key, _) = self.json_member(index, id);
        if let Some(k) = key {
            push(&compact(&k, 120), pal.attr);
            push(": ", pal.punct);
        }
        match n.kind() {
            Kind::Object | Kind::Array => {
                let (open, close, unit) = if n.kind() == Kind::Object { ("{", "}", "fields") } else { ("[", "]", "items") };
                if n.child_count == 0 {
                    push(&format!("{open}{close}"), pal.punct);
                } else {
                    push(open, pal.punct);
                    push(&format!(" {}", self.json_preview(index, id)), pal.weak);
                    push(&format!("   {} {unit}", fmt_num(n.child_count as u64)), pal.weak);
                }
            }
            _ => {
                let (text, is_str) = self.json_scalar(index, id, 300);
                if is_str {
                    push(&format!("\"{text}\""), pal.value);
                } else if text.starts_with(['t', 'f', 'n']) {
                    push(&text, pal.comment);
                } else {
                    push(&text, pal.tag);
                }
            }
        }
    }

    fn row_job(&self, index: &Index, id: u32, pal: &Palette) -> LayoutJob {
        let mut job = LayoutJob::default();
        let font = FontId::monospace(FONT_SIZE);
        let mut push = |s: &str, c: Color32| job.append(s, 0.0, TextFormat::simple(font.clone(), c));
        let n = index.node(id);
        match n.kind() {
            Kind::Object | Kind::Array | Kind::Value => self.json_job(index, id, pal, &mut push),
            Kind::Element => {
                let tag = text::parse_start_tag(self.slice(n.start(), n.end(), LABEL_SRC_MAX), 12);
                let name = decode(self.enc, tag.name);
                push("<", pal.punct);
                push(&name, pal.tag);
                for (k, v) in &tag.attrs {
                    push(" ", pal.punct);
                    push(&decode(self.enc, k), pal.attr);
                    push("=\"", pal.punct);
                    push(&compact(&unescape(&decode(self.enc, v)), 80), pal.value);
                    push("\"", pal.punct);
                }
                if tag.truncated {
                    push(" …", pal.punct);
                }
                if n.child_count == 0 && tag.self_closing {
                    push("/>", pal.punct);
                } else if index.is_inline(id) {
                    push(">", pal.punct);
                    push(&self.text_content(index, id + 1, 300), pal.text);
                    push("</", pal.punct);
                    push(&name, pal.tag);
                    push(">", pal.punct);
                } else if n.child_count == 0 {
                    push("></", pal.punct);
                    push(&name, pal.tag);
                    push(">", pal.punct);
                } else {
                    push(">", pal.punct);
                    push(&format!("   {} children", fmt_num(n.child_count as u64)), pal.weak);
                }
            }
            Kind::Text => push(&self.text_content(index, id, 300), pal.text),
            Kind::CData => {
                push("<![CDATA[ ", pal.punct);
                push(&self.text_content(index, id, 300), pal.text);
                push(" ]]>", pal.punct);
            }
            Kind::Comment => push(&compact(&decode(self.enc, self.slice(n.start(), n.end(), 1200)), 300), pal.comment),
            Kind::PI | Kind::Decl | Kind::DocType => {
                push(&compact(&decode(self.enc, self.slice(n.start(), n.end(), 1200)), 300), pal.punct)
            }
        }
        job.wrap.max_rows = 1;
        job
    }

    fn json_path(&self, index: &Index, id: u32) -> String {
        let mut chain = index.ancestors(id);
        chain.push(id);
        let mut out = String::from("$");
        for &c in chain.iter().skip(1) {
            let parent = index.node(c).parent;
            if index.node(parent).kind() == Kind::Array {
                let mut pos = 0u64;
                let mut s = index.first_child(parent);
                while s != c && s != NONE {
                    pos += 1;
                    s = index.node(s).next_sibling;
                }
                out.push_str(&format!("[{pos}]"));
            } else {
                let key = self.json_member(index, c).0.unwrap_or_default();
                if !key.is_empty() && key.chars().all(|ch| ch.is_alphanumeric() || ch == '_') {
                    out.push('.');
                    out.push_str(&key);
                } else {
                    out.push_str(&format!("[\"{}\"]", key.replace('"', "\\\"")));
                }
            }
        }
        out
    }

    fn node_path(&self, index: &Index, id: u32) -> String {
        if index.json {
            return self.json_path(index, id);
        }
        let mut parts: Vec<String> = index.ancestors(id).iter().map(|&a| self.element_name(index, a)).collect();
        parts.push(match index.node(id).kind() {
            Kind::Element => self.element_name(index, id),
            Kind::Text => "text()".into(),
            Kind::CData => "#cdata".into(),
            Kind::Comment => "comment()".into(),
            _ => "#node".into(),
        });
        format!("/{}", parts.join("/"))
    }

    fn make_detail(&self, index: &Index, id: u32) -> Detail {
        let n = index.node(id);
        let attrs = if n.kind() == Kind::Element {
            let tag = text::parse_start_tag(self.slice(n.start(), n.end(), 1 << 20), 10_000);
            tag.attrs
                .iter()
                .map(|(k, v)| {
                    let v = unescape(&decode(self.enc, v)).into_owned();
                    let v = if v.chars().count() > 20_000 { compact(&v, 20_000) } else { v };
                    (decode(self.enc, k).into_owned(), v)
                })
                .collect()
        } else if n.kind() == Kind::Object {
            // Scalar members of an object, like attributes of an element.
            let mut fields = Vec::new();
            let mut c = index.first_child(id);
            while c != NONE && fields.len() < 1000 {
                if index.node(c).kind() == Kind::Value {
                    let key = self.json_member(index, c).0.unwrap_or_default();
                    fields.push((key, self.json_scalar(index, c, 20_000).0));
                }
                c = index.node(c).next_sibling;
            }
            fields
        } else {
            Vec::new()
        };
        let raw = self.slice(n.start(), n.end(), DETAIL_SRC_MAX);
        Detail {
            node: id,
            path: self.node_path(index, id),
            attrs,
            source: decode(self.enc, raw).into_owned(),
            source_truncated: (n.end() - n.start()) as usize > raw.len(),
        }
    }

    fn copy_source(&self, index: &Index, id: u32, ctx: &egui::Context) {
        let n = index.node(id);
        ctx.copy_text(decode(self.enc, self.slice(n.start(), n.end(), 50 << 20)).into_owned());
    }
}

pub struct App {
    doc: Option<Doc>,
    message: Option<String>,
    search_text: String,
    case_sensitive: bool,
    use_regex: bool,
    focus_search: bool,
    visible_rows: usize,
    url_text: String,
    focus_url: bool,
    download: Option<Arc<Download>>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, arg: Option<String>) -> Self {
        download::clean_old();
        let mut app = Self {
            doc: None,
            message: None,
            search_text: String::new(),
            case_sensitive: false,
            use_regex: false,
            focus_search: false,
            visible_rows: 30,
            url_text: String::new(),
            focus_url: false,
            download: None,
        };
        match arg {
            Some(a) if a.starts_with("http://") || a.starts_with("https://") => {
                app.url_text = a;
                app.start_download(&cc.egui_ctx);
            }
            Some(a) => app.open(PathBuf::from(a), &cc.egui_ctx),
            None => {}
        }
        app
    }

    fn open(&mut self, path: PathBuf, ctx: &egui::Context) {
        if let Some(old) = self.doc.take() {
            old.cancel();
            let old_path = old.path.clone();
            drop(old);
            if download::is_temp(&old_path) {
                // Fails while the indexer thread still holds the mapping; clean_old() catches it next run.
                let _ = std::fs::remove_file(old_path);
            }
        }
        match Doc::open(path, ctx) {
            Ok(doc) => {
                let name = doc.path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("{name} — XML / JSON Viewer")));
                self.doc = Some(doc);
                self.message = None;
            }
            Err(e) => self.message = Some(e),
        }
    }

    fn start_download(&mut self, ctx: &egui::Context) {
        if let Some(old) = self.download.take() {
            old.cancel.store(true, Ordering::Relaxed);
        }
        match download::normalize_url(&self.url_text) {
            Ok(url) => {
                self.url_text = url.clone();
                self.download = Some(download::start(url, ctx.clone()));
                self.message = None;
            }
            Err(e) => self.message = Some(e),
        }
    }

    fn poll_download(&mut self, ctx: &egui::Context) {
        let Some(job) = self.download.clone() else { return };
        let Some(result) = job.result.lock().unwrap().take() else {
            ctx.request_repaint_after(Duration::from_millis(100));
            return;
        };
        self.download = None;
        match result {
            Ok(path) => {
                self.open(path, ctx);
                if let Some(doc) = self.doc.as_mut() {
                    doc.source = Some(job.url.clone());
                    ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("{} — XML / JSON Viewer", job.url)));
                }
            }
            Err(e) => self.message = Some(format!("{}: {e}", job.url)),
        }
    }

    fn pick_file(&mut self, ctx: &egui::Context) {
        if let Some(p) = rfd::FileDialog::new()
            .add_filter("XML / JSON", &["xml", "json", "geojson", "jsonl", "ndjson", "xsd", "xsl", "xslt", "svg", "rss", "atom", "config", "gpx", "kml"])
            .add_filter("All files", &["*"])
            .pick_file()
        {
            self.open(p, ctx);
        }
    }

    fn start_search(&mut self) {
        let Some(doc) = self.doc.as_mut() else { return };
        if doc.index().is_none() {
            return;
        }
        if let Some(old) = doc.search.take() {
            old.cancel.store(true, Ordering::Relaxed);
        }
        doc.search_pos = None;
        match search::build_regex(&self.search_text, self.use_regex, self.case_sensitive, doc.enc) {
            Ok(re) => {
                let job = Arc::new(SearchJob::default());
                let data = doc.data.clone();
                let j = job.clone();
                thread::spawn(move || search::run(&data, &re, &j));
                doc.search = Some(job);
                self.message = None;
            }
            Err(e) => self.message = Some(format!("Search error: {e}")),
        }
    }

    fn goto_result(&mut self, forward: bool) {
        let visible = self.visible_rows;
        let Some(doc) = self.doc.as_mut() else { return };
        let Some(index) = doc.index() else { return };
        let Some(job) = doc.search.clone() else { return };
        let results = job.results.lock().unwrap();
        if results.is_empty() {
            return;
        }
        let len = results.len();
        let mut pos = match doc.search_pos {
            None => 0,
            Some(p) if forward => (p + 1) % len,
            Some(p) => (p + len - 1) % len,
        };
        // Skip hits that fall outside any node (whitespace between top-level nodes).
        for _ in 0..len.min(1000) {
            if let Some(node) = index.node_at_offset(results[pos]) {
                drop(results);
                doc.search_pos = Some(pos);
                doc.reveal(&index, node, visible);
                return;
            }
            pos = if forward { (pos + 1) % len } else { (pos + len - 1) % len };
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        let (open, find, url, next, prev) = ctx.input(|i| {
            (
                i.modifiers.command && i.key_pressed(Key::O),
                i.modifiers.command && i.key_pressed(Key::F),
                i.modifiers.command && i.key_pressed(Key::L),
                i.key_pressed(Key::F3) && !i.modifiers.shift,
                i.key_pressed(Key::F3) && i.modifiers.shift,
            )
        });
        if open {
            self.pick_file(ctx);
        }
        if find {
            self.focus_search = true;
        }
        if url {
            self.focus_url = true;
        }
        if next {
            self.goto_result(true);
        }
        if prev {
            self.goto_result(false);
        }

        if ctx.wants_keyboard_input() {
            return;
        }
        let visible = self.visible_rows;
        let Some(doc) = self.doc.as_mut() else { return };
        let Some(index) = doc.index() else { return };
        if doc.rows.is_empty() {
            return;
        }
        let last = doc.rows.len() - 1;
        let page = visible.saturating_sub(1).max(1);
        let cur = doc.selected.unwrap_or(0);
        let mut moved = false;
        ctx.input(|i| {
            let mut sel = cur;
            if i.key_pressed(Key::ArrowDown) {
                sel = (cur + 1).min(last);
            }
            if i.key_pressed(Key::ArrowUp) {
                sel = cur.saturating_sub(1);
            }
            if i.key_pressed(Key::PageDown) {
                sel = (cur + page).min(last);
            }
            if i.key_pressed(Key::PageUp) {
                sel = cur.saturating_sub(page);
            }
            if i.key_pressed(Key::Home) {
                sel = 0;
            }
            if i.key_pressed(Key::End) {
                sel = last;
            }
            if sel != cur || (doc.selected.is_none() && i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. }))) {
                doc.selected = Some(sel);
                moved = true;
            }
        });
        let (right, left, enter) =
            ctx.input(|i| (i.key_pressed(Key::ArrowRight), i.key_pressed(Key::ArrowLeft), i.key_pressed(Key::Enter)));
        if let Some(p) = doc.selected {
            let node = doc.rows[p].node;
            if right {
                if index.is_expandable(node) && !doc.is_open(node) {
                    doc.expand(&index, p);
                } else if doc.is_open(node) && p < last {
                    doc.selected = Some(p + 1);
                }
                moved = true;
            }
            if left {
                if doc.is_open(node) {
                    doc.collapse(p);
                } else if let Some(pr) = doc.row_of(index.node(node).parent) {
                    doc.selected = Some(pr);
                }
                moved = true;
            }
            if enter {
                doc.toggle(&index, p);
                moved = true;
            }
        }
        if moved {
            doc.ensure_visible(visible);
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("Open…").on_hover_text("Ctrl+O").clicked() {
                self.pick_file(ui.ctx());
            }
            let url_resp = ui.add(
                egui::TextEdit::singleline(&mut self.url_text)
                    .hint_text("https://… XML or JSON URL (Ctrl+L)")
                    .desired_width(300.0),
            );
            if self.focus_url {
                url_resp.request_focus();
                self.focus_url = false;
            }
            let url_enter = url_resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if let Some(job) = self.download.clone() {
                let got = job.received.load(Ordering::Relaxed);
                let total = job.total.load(Ordering::Relaxed);
                if total > 0 {
                    ui.add(
                        egui::ProgressBar::new(got as f32 / total as f32)
                            .desired_width(160.0)
                            .text(format!("{} / {}", fmt_bytes(got), fmt_bytes(total))),
                    );
                } else {
                    ui.spinner();
                    ui.label(fmt_bytes(got));
                }
                if ui.small_button("✖").on_hover_text("Stop download").clicked() {
                    job.cancel.store(true, Ordering::Relaxed);
                }
            } else if ui.button("Download").on_hover_text("Download and open the file at this URL").clicked() || url_enter {
                self.start_download(ui.ctx());
            }
            ui.separator();
            let ready = self.doc.as_ref().is_some_and(|d| d.index().is_some());
            let resp = ui.add_enabled(
                ready,
                egui::TextEdit::singleline(&mut self.search_text)
                    .hint_text("Search (Ctrl+F)")
                    .desired_width(280.0),
            );
            if self.focus_search {
                resp.request_focus();
                self.focus_search = false;
            }
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            ui.toggle_value(&mut self.case_sensitive, "Aa").on_hover_text("Match case");
            ui.toggle_value(&mut self.use_regex, ".*").on_hover_text("Regular expression");
            if (ui.add_enabled(ready, egui::Button::new("Search")).clicked() || enter) && ready {
                self.start_search();
            }

            let mut nav = None;
            if let Some(doc) = &self.doc {
                if let Some(job) = &doc.search {
                    let count = job.results.lock().unwrap().len();
                    if ui.add_enabled(count > 0, egui::Button::new("◀")).on_hover_text("Previous (Shift+F3)").clicked() {
                        nav = Some(false);
                    }
                    if ui.add_enabled(count > 0, egui::Button::new("▶")).on_hover_text("Next (F3)").clicked() {
                        nav = Some(true);
                    }
                    let pos = doc.search_pos.map_or("-".to_string(), |p| fmt_num(p as u64 + 1));
                    let more = if job.truncated.load(Ordering::Relaxed) { "+" } else { "" };
                    ui.label(format!("{pos} / {}{more}", fmt_num(count as u64)));
                    if !job.done.load(Ordering::Acquire) {
                        let f = job.progress.load(Ordering::Relaxed) as f32 / doc.data.len().max(1) as f32;
                        ui.add(egui::ProgressBar::new(f).desired_width(120.0).show_percentage());
                        if ui.small_button("✖").on_hover_text("Stop search").clicked() {
                            job.cancel.store(true, Ordering::Relaxed);
                        }
                        ui.ctx().request_repaint_after(Duration::from_millis(100));
                    }
                }
            }
            if let Some(f) = nav {
                self.goto_result(f);
            }
        });
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some(m) = &self.message {
                ui.colored_label(Color32::from_rgb(230, 80, 80), m);
                ui.separator();
            }
            let Some(doc) = &self.doc else {
                ui.label("Open an XML or JSON file, or drop one onto the window.");
                return;
            };
            match &doc.source {
                Some(url) => ui.label(url).on_hover_text(doc.path.display().to_string()),
                None => ui.label(doc.path.display().to_string()),
            };
            ui.separator();
            ui.label(fmt_bytes(doc.data.len() as u64));
            ui.separator();
            match &doc.state {
                State::Ready { index, .. } if index.json => ui.label("JSON"),
                _ => ui.label(doc.enc.name()),
            };
            if let State::Ready { index, secs } = &doc.state {
                ui.separator();
                ui.label(format!(
                    "{} nodes · {:.2} s · {:.0} MB/s",
                    fmt_num(index.nodes.len() as u64),
                    secs,
                    doc.data.len() as f64 / 1_048_576.0 / secs.max(1e-6)
                ));
                if let Some(e) = &index.error {
                    ui.separator();
                    ui.colored_label(
                        Color32::from_rgb(230, 80, 80),
                        format!("Parse error at line {}, column {}: {}", fmt_num(e.line), e.column, e.message),
                    );
                }
            }
        });
    }

    fn detail_panel(&mut self, ui: &mut egui::Ui) {
        let Some(doc) = self.doc.as_mut() else { return };
        let Some(index) = doc.index() else { return };
        let Some(sel) = doc.selected else {
            ui.weak("Select a node.");
            return;
        };
        let node = doc.rows[sel].node;
        if doc.detail.as_ref().is_none_or(|d| d.node != node) {
            doc.detail = Some(doc.make_detail(&index, node));
        }
        let n = *index.node(node);
        let mut copy_src = false;
        let detail = doc.detail.as_ref().unwrap();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.strong("Path:");
                ui.add(egui::Label::new(egui::RichText::new(&detail.path).monospace()).wrap());
            });
            if ui.small_button("Copy path").clicked() {
                ui.ctx().copy_text(detail.path.clone());
            }
            ui.label(format!(
                "Bytes {} – {} ({}) · {} children",
                fmt_num(n.start()),
                fmt_num(n.end()),
                fmt_bytes(n.end() - n.start()),
                fmt_num(n.child_count as u64)
            ));
            if !detail.attrs.is_empty() {
                ui.separator();
                let title = if index.json { "Fields" } else { "Attributes" };
                ui.strong(format!("{title} ({})", detail.attrs.len()));
                egui::Grid::new("attrs").striped(true).num_columns(2).show(ui, |ui| {
                    for (k, v) in &detail.attrs {
                        ui.label(egui::RichText::new(k).monospace());
                        ui.add(egui::Label::new(egui::RichText::new(v).monospace()).wrap().selectable(true));
                        ui.end_row();
                    }
                });
            }
            ui.separator();
            ui.horizontal(|ui| {
                ui.strong("Source");
                if detail.source_truncated {
                    ui.weak(format!("(showing first {})", fmt_bytes(DETAIL_SRC_MAX as u64)));
                }
                if ui.small_button("Copy").on_hover_text("Up to 50 MB").clicked() {
                    copy_src = true;
                }
            });
            let mut src = detail.source.as_str();
            ui.add(
                egui::TextEdit::multiline(&mut src)
                    .code_editor()
                    .desired_width(f32::INFINITY),
            );
        });
        if copy_src {
            doc.copy_source(&index, node, ui.ctx());
        }
    }

    fn tree(&mut self, ui: &mut egui::Ui) {
        let Some(doc) = self.doc.as_mut() else {
            ui.centered_and_justified(|ui| {
                ui.heading("Drop an XML or JSON file here, or choose one with \"Open…\"");
            });
            return;
        };
        let index = match &doc.state {
            State::Indexing { progress, .. } => {
                let done = progress.bytes.load(Ordering::Relaxed);
                let total = doc.data.len() as u64;
                let secs = doc.opened.elapsed().as_secs_f64();
                ui.add_space(ui.available_height() / 3.0);
                ui.vertical_centered(|ui| {
                    ui.heading("Indexing…");
                    ui.add(
                        egui::ProgressBar::new(done as f32 / total.max(1) as f32)
                            .desired_width(420.0)
                            .show_percentage(),
                    );
                    ui.label(format!(
                        "{} / {} · {:.0} MB/sn",
                        fmt_bytes(done),
                        fmt_bytes(total),
                        done as f64 / 1_048_576.0 / secs.max(1e-3)
                    ));
                });
                ui.ctx().request_repaint_after(Duration::from_millis(50));
                return;
            }
            State::Ready { index, .. } => index.clone(),
        };

        let pal = Palette::new(ui.visuals());
        let area = ui.available_rect_before_wrap();
        ui.allocate_rect(area, Sense::hover());
        let list = Rect::from_min_max(area.min, Pos2::new(area.max.x - SCROLLBAR_W, area.max.y));
        let bar = Rect::from_min_max(Pos2::new(area.max.x - SCROLLBAR_W, area.min.y), area.max);
        let visible = ((list.height() / ROW_H).floor() as usize).max(1);
        self.visible_rows = visible;
        let total = doc.rows.len();
        let max_top = total.saturating_sub(visible);

        // Mouse wheel: rows are tracked as an integer index, never as pixels,
        // so millions of rows scroll precisely.
        if ui.rect_contains_pointer(area) {
            let dy = ui.input(|i| i.smooth_scroll_delta.y);
            if dy != 0.0 {
                doc.scroll_accum -= dy / ROW_H;
                let lines = doc.scroll_accum.trunc();
                doc.scroll_accum -= lines;
                doc.top = (doc.top as i64 + lines as i64).clamp(0, max_top as i64) as usize;
            }
        }
        doc.top = doc.top.min(max_top);

        // Scrollbar.
        let bar_resp = ui.interact(bar, ui.id().with("scrollbar"), Sense::click_and_drag());
        let painter = ui.painter();
        painter.rect_filled(bar, 0.0, ui.visuals().extreme_bg_color);
        if total > visible {
            let thumb_h = (bar.height() * visible as f32 / total as f32).max(24.0).min(bar.height());
            let track = (bar.height() - thumb_h).max(1.0);
            if let Some(p) = bar_resp.interact_pointer_pos() {
                let f = (((p.y - bar.top() - thumb_h / 2.0) / track) as f64).clamp(0.0, 1.0);
                doc.top = (f * max_top as f64).round() as usize;
            }
            let f = doc.top as f64 / max_top.max(1) as f64;
            let y = bar.top() + (f * track as f64) as f32;
            let thumb = Rect::from_min_size(Pos2::new(bar.left() + 2.0, y), Vec2::new(SCROLLBAR_W - 4.0, thumb_h));
            let col = if bar_resp.hovered() || bar_resp.dragged() {
                ui.visuals().widgets.active.bg_fill
            } else {
                ui.visuals().widgets.inactive.bg_fill
            };
            painter.rect_filled(thumb, 4.0, col);
        }

        let painter = ui.painter_at(list);
        let mut toggle_row = None;
        let mut select_row = None;
        let mut context: Option<(usize, egui::Response)> = None;
        for i in 0..=visible {
            let r = doc.top + i;
            if r >= total {
                break;
            }
            let row = doc.rows[r];
            let y = list.top() + i as f32 * ROW_H;
            let row_rect = Rect::from_min_size(Pos2::new(list.left(), y), Vec2::new(list.width(), ROW_H));
            let resp = ui.interact(row_rect.intersect(list), ui.id().with(("row", r)), Sense::click());
            if doc.selected == Some(r) {
                painter.rect_filled(row_rect, 0.0, ui.visuals().selection.bg_fill);
            } else if resp.hovered() {
                painter.rect_filled(row_rect, 0.0, ui.visuals().widgets.hovered.weak_bg_fill);
            }
            let x0 = list.left() + 6.0 + row.depth as f32 * INDENT;
            // Indentation guides.
            for d in 0..row.depth.min(64) {
                let gx = list.left() + 6.0 + d as f32 * INDENT + 5.0;
                painter.line_segment(
                    [Pos2::new(gx, y), Pos2::new(gx, y + ROW_H)],
                    Stroke::new(1.0_f32, ui.visuals().widgets.noninteractive.bg_stroke.color),
                );
            }
            let expandable = index.is_expandable(row.node);
            if expandable {
                let c = Pos2::new(x0 + 5.0, y + ROW_H / 2.0);
                let col = ui.visuals().text_color();
                let pts = if doc.is_open(row.node) {
                    vec![c + Vec2::new(-4.0, -2.0), c + Vec2::new(4.0, -2.0), c + Vec2::new(0.0, 3.0)]
                } else {
                    vec![c + Vec2::new(-2.0, -4.0), c + Vec2::new(3.0, 0.0), c + Vec2::new(-2.0, 4.0)]
                };
                painter.add(Shape::convex_polygon(pts, col, Stroke::NONE));
            }
            let galley = ui.fonts(|f| f.layout_job(doc.row_job(&index, row.node, &pal)));
            painter.galley(Pos2::new(x0 + 14.0, y + (ROW_H - galley.size().y) / 2.0), galley, pal.text);

            if resp.clicked() {
                let on_arrow = resp.interact_pointer_pos().is_some_and(|p| p.x < x0 + 14.0);
                if expandable && on_arrow {
                    toggle_row = Some(r);
                }
                select_row = Some(r);
            }
            if resp.double_clicked() && expandable {
                toggle_row = Some(r);
            }
            if resp.secondary_clicked() {
                select_row = Some(r);
            }
            if resp.context_menu_opened() || resp.secondary_clicked() {
                context = Some((r, resp));
            } else {
                let _ = resp;
            }
        }
        if let Some(r) = select_row {
            doc.selected = Some(r);
        }
        if let Some(r) = toggle_row {
            doc.toggle(&index, r);
        }
        if let Some((r, resp)) = context {
            let node = doc.rows.get(r).map(|row| row.node);
            if let Some(node) = node {
                resp.context_menu(|ui| {
                    if index.node(node).kind() == Kind::Element && ui.button("Copy name").clicked() {
                        ui.ctx().copy_text(doc.element_name(&index, node));
                        ui.close_menu();
                    }
                    if index.json {
                        if let (Some(key), _) = doc.json_member(&index, node) {
                            if ui.button("Copy key").clicked() {
                                ui.ctx().copy_text(key);
                                ui.close_menu();
                            }
                        }
                        if index.node(node).kind() == Kind::Value && ui.button("Copy value").clicked() {
                            ui.ctx().copy_text(doc.json_scalar(&index, node, usize::MAX / 8).0);
                            ui.close_menu();
                        }
                    }
                    if ui.button("Copy path").clicked() {
                        ui.ctx().copy_text(doc.node_path(&index, node));
                        ui.close_menu();
                    }
                    if ui.button("Copy source").clicked() {
                        doc.copy_source(&index, node, ui.ctx());
                        ui.close_menu();
                    }
                    if index.is_expandable(node) {
                        let open = doc.is_open(node);
                        if ui.button(if open { "Collapse" } else { "Expand" }).clicked() {
                            if let Some(p) = doc.row_of(node) {
                                doc.toggle(&index, p);
                            }
                            ui.close_menu();
                        }
                    }
                });
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(p) = ctx.input(|i| i.raw.dropped_files.first().and_then(|f| f.path.clone())) {
            self.open(p, ctx);
        }
        self.poll_download(ctx);
        if let Some(doc) = self.doc.as_mut() {
            doc.poll();
        }
        self.handle_keys(ctx);

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(4.0);
            self.top_bar(ui);
            ui.add_space(2.0);
        });
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| self.status_bar(ui));
        if self.doc.as_ref().is_some_and(|d| d.index().is_some()) {
            egui::SidePanel::right("detail")
                .resizable(true)
                .default_width(420.0)
                .show(ctx, |ui| self.detail_panel(ui));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(&ctx.style()).inner_margin(0.0))
            .show(ctx, |ui| self.tree(ui));
    }
}
