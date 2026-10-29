//! Single-pass structural index over a memory-mapped XML document.
//!
//! Nodes are stored in document (preorder) order, so the first child of a
//! node with children is always `id + 1` and node start offsets are sorted.
//! Names, attributes and text are never copied; they are read back from the
//! mapped bytes on demand.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use quick_xml::Reader;
use quick_xml::events::Event;

pub const NONE: u32 = u32::MAX;
/// Offsets are stored in 40 bits.
pub const MAX_FILE_SIZE: u64 = 1 << 40;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Kind {
    Element,
    Text,
    CData,
    Comment,
    PI,
    Decl,
    DocType,
    /// JSON `{...}`; for object members the node starts at the key.
    Object,
    /// JSON `[...]`
    Array,
    /// JSON string, number, `true`, `false` or `null`.
    Value,
}

const KINDS: [Kind; 10] = [
    Kind::Element,
    Kind::Text,
    Kind::CData,
    Kind::Comment,
    Kind::PI,
    Kind::Decl,
    Kind::DocType,
    Kind::Object,
    Kind::Array,
    Kind::Value,
];
const START_MASK: u64 = (1 << 48) - 1;

/// 24 bytes per node; tens of millions of these live in memory at once.
#[derive(Clone, Copy, Debug)]
pub struct Node {
    /// start (bits 0..48) | kind (48..56) | end bits 32..40 (56..64)
    packed: u64,
    end_lo: u32,
    pub parent: u32,
    pub next_sibling: u32,
    pub child_count: u32,
}

impl Node {
    fn new(kind: Kind, start: u64, end: u64, parent: u32) -> Self {
        let mut n = Self {
            packed: start | (kind as u64) << 48,
            end_lo: 0,
            parent,
            next_sibling: NONE,
            child_count: 0,
        };
        n.set_end(end);
        n
    }

    /// Byte offset of the first byte (`<` for markup).
    #[inline]
    pub fn start(&self) -> u64 {
        self.packed & START_MASK
    }

    /// Byte offset one past the last byte (end of the closing tag for elements).
    #[inline]
    pub fn end(&self) -> u64 {
        (self.packed >> 56) << 32 | self.end_lo as u64
    }

    #[inline]
    pub fn kind(&self) -> Kind {
        KINDS[((self.packed >> 48) & 0xFF) as usize]
    }

    fn set_end(&mut self, end: u64) {
        self.end_lo = end as u32;
        self.packed = (self.packed & !(0xFF << 56)) | ((end >> 32) & 0xFF) << 56;
    }
}

const CHUNK_BITS: u32 = 20;
const CHUNK: usize = 1 << CHUNK_BITS;
const CHUNK_MASK: u32 = (1 << CHUNK_BITS) - 1;

/// Node storage in fixed chunks: growing never copies existing nodes, so
/// peak memory stays at the final size instead of old + new capacity.
#[derive(Default)]
pub struct Nodes {
    chunks: Vec<Vec<Node>>,
    len: usize,
}

impl Nodes {
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn memory(&self) -> usize {
        self.chunks.iter().map(|c| c.capacity() * std::mem::size_of::<Node>()).sum()
    }

    #[inline]
    pub fn get(&self, id: u32) -> &Node {
        &self.chunks[(id >> CHUNK_BITS) as usize][(id & CHUNK_MASK) as usize]
    }

    #[inline]
    fn get_mut(&mut self, id: u32) -> &mut Node {
        &mut self.chunks[(id >> CHUNK_BITS) as usize][(id & CHUNK_MASK) as usize]
    }

    fn push(&mut self, n: Node) {
        if self.chunks.last().is_none_or(|c| c.len() == CHUNK) {
            self.chunks.push(Vec::with_capacity(CHUNK));
        }
        self.chunks.last_mut().unwrap().push(n);
        self.len += 1;
    }

    /// Number of leading nodes for which `pred` holds (like `slice::partition_point`).
    fn partition_point(&self, pred: impl Fn(&Node) -> bool) -> usize {
        let (mut lo, mut hi) = (0usize, self.len);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if pred(self.get(mid as u32)) { lo = mid + 1 } else { hi = mid }
        }
        lo
    }
}

#[derive(Debug)]
pub struct ParseError {
    pub offset: u64,
    pub line: u64,
    pub column: u64,
    pub message: String,
}

pub struct Index {
    pub nodes: Nodes,
    /// Top-level nodes (declaration, comments, root element, ...).
    pub top: Vec<u32>,
    pub error: Option<ParseError>,
    pub cancelled: bool,
    pub json: bool,
}

#[derive(Default)]
pub struct Progress {
    pub bytes: AtomicU64,
    pub cancel: AtomicBool,
}

impl Index {
    #[inline]
    pub fn node(&self, id: u32) -> &Node {
        self.nodes.get(id)
    }

    pub fn first_child(&self, id: u32) -> u32 {
        if self.node(id).child_count > 0 { id + 1 } else { NONE }
    }

    /// Element whose only child is a text/CDATA node: shown on a single row.
    pub fn is_inline(&self, id: u32) -> bool {
        let n = self.node(id);
        n.kind() == Kind::Element
            && n.child_count == 1
            && matches!(self.node(id + 1).kind(), Kind::Text | Kind::CData)
    }

    pub fn is_expandable(&self, id: u32) -> bool {
        let n = self.node(id);
        match n.kind() {
            Kind::Element => n.child_count > 0 && !self.is_inline(id),
            Kind::Object | Kind::Array => n.child_count > 0,
            _ => false,
        }
    }

    /// Deepest node whose byte range contains `offset`.
    pub fn node_at_offset(&self, offset: u64) -> Option<u32> {
        let i = self.nodes.partition_point(|n| n.start() <= offset);
        if i == 0 {
            return None;
        }
        let mut id = (i - 1) as u32;
        loop {
            let n = self.node(id);
            if offset < n.end() {
                return Some(id);
            }
            if n.parent == NONE {
                return None;
            }
            id = n.parent;
        }
    }

    /// Ancestors of `id`, root first, excluding `id` itself.
    pub fn ancestors(&self, id: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut p = self.node(id).parent;
        while p != NONE {
            out.push(p);
            p = self.node(p).parent;
        }
        out.reverse();
        out
    }
}

#[derive(Default)]
pub(crate) struct Builder {
    pub(crate) nodes: Nodes,
    /// Open containers: (node, last child so far).
    pub(crate) stack: Vec<(u32, u32)>,
    pub(crate) top: Vec<u32>,
}

impl Builder {
    pub(crate) fn close(&mut self, end: u64) {
        if let Some((id, _)) = self.stack.pop() {
            self.nodes.get_mut(id).set_end(end);
        }
    }

    pub(crate) fn add(&mut self, kind: Kind, start: u64, end: u64) -> Option<u32> {
        if self.nodes.len() >= (NONE - 1) as usize {
            return None;
        }
        let id = self.nodes.len() as u32;
        let parent = match self.stack.last_mut() {
            Some((p, last)) => {
                if *last != NONE {
                    self.nodes.get_mut(*last).next_sibling = id;
                }
                *last = id;
                self.nodes.get_mut(*p).child_count += 1;
                *p
            }
            None => {
                self.top.push(id);
                NONE
            }
        };
        self.nodes.push(Node::new(kind, start, end, parent));
        Some(id)
    }
}

pub(crate) fn parse_error(data: &[u8], offset: u64, message: String) -> ParseError {
    let head = &data[..(offset as usize).min(data.len())];
    let line = memchr::memchr_iter(b'\n', head).count() as u64 + 1;
    let line_start = memchr::memrchr(b'\n', head).map_or(0, |p| p + 1);
    ParseError { offset, line, column: (head.len() - line_start) as u64 + 1, message }
}

pub(crate) fn bom_len(data: &[u8]) -> usize {
    if data.starts_with(&[0xEF, 0xBB, 0xBF]) { 3 } else { 0 }
}

/// JSON when the first non-whitespace byte opens an object or array.
pub fn is_json(data: &[u8]) -> bool {
    data[bom_len(data)..]
        .iter()
        .find(|b| !b.is_ascii_whitespace())
        .is_some_and(|&b| b == b'{' || b == b'[')
}

pub fn build(data: &[u8], progress: &Progress) -> Index {
    if is_json(data) { crate::json::build(data, progress) } else { build_xml(data, progress) }
}

fn build_xml(data: &[u8], progress: &Progress) -> Index {
    let base = bom_len(data);
    let mut reader = Reader::from_reader(&data[base..]);
    reader.config_mut().check_end_names = true;

    let mut b = Builder::default();
    let base = base as u64;
    let mut error = None;
    let mut cancelled = false;
    let mut events: u32 = 0;

    loop {
        let start = reader.buffer_position() as u64 + base;
        let ev = reader.read_event();
        let end = reader.buffer_position() as u64 + base;
        let added = match ev {
            Ok(Event::Start(_)) => {
                let id = b.add(Kind::Element, start, end);
                if let Some(id) = id {
                    b.stack.push((id, NONE));
                }
                id
            }
            Ok(Event::End(_)) => {
                b.close(end);
                Some(0)
            }
            Ok(Event::Empty(_)) => b.add(Kind::Element, start, end),
            Ok(Event::Text(t)) => {
                if t.iter().all(u8::is_ascii_whitespace) {
                    Some(0)
                } else {
                    b.add(Kind::Text, start, end)
                }
            }
            Ok(Event::CData(_)) => b.add(Kind::CData, start, end),
            Ok(Event::Comment(_)) => b.add(Kind::Comment, start, end),
            Ok(Event::PI(_)) => b.add(Kind::PI, start, end),
            Ok(Event::Decl(_)) => b.add(Kind::Decl, start, end),
            Ok(Event::DocType(_)) => b.add(Kind::DocType, start, end),
            Ok(Event::Eof) => break,
            #[allow(unreachable_patterns)]
            Ok(_) => Some(0),
            Err(e) => {
                error = Some(parse_error(data, reader.error_position() as u64 + base, e.to_string()));
                break;
            }
        };
        if added.is_none() {
            error = Some(parse_error(data, start, TOO_MANY.into()));
            break;
        }
        events = events.wrapping_add(1);
        if events & 0x3FFF == 0 {
            progress.bytes.store(end, Ordering::Relaxed);
            if progress.cancel.load(Ordering::Relaxed) {
                cancelled = true;
                break;
            }
        }
    }

    b.finish(data, progress, error, cancelled, false)
}

pub(crate) const TOO_MANY: &str = "too many nodes (limit is 4 billion)";

impl Builder {
    pub(crate) fn finish(
        mut self,
        data: &[u8],
        progress: &Progress,
        error: Option<ParseError>,
        cancelled: bool,
        json: bool,
    ) -> Index {
        // Containers left open (truncated / malformed file) extend to the end.
        while !self.stack.is_empty() {
            self.close(data.len() as u64);
        }
        progress.bytes.store(data.len() as u64, Ordering::Relaxed);
        Index { nodes: self.nodes, top: self.top, error, cancelled, json }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idx(s: &str) -> Index {
        build(s.as_bytes(), &Progress::default())
    }

    fn children(i: &Index, id: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut c = i.first_child(id);
        while c != NONE {
            out.push(c);
            c = i.node(c).next_sibling;
        }
        out
    }

    fn src<'a>(s: &'a str, i: &Index, id: u32) -> &'a str {
        &s[i.node(id).start() as usize..i.node(id).end() as usize]
    }

    #[test]
    fn packing() {
        let n = Node::new(Kind::Comment, (1 << 47) + 5, (1 << 39) + 7, 3);
        assert_eq!(n.start(), (1 << 47) + 5);
        assert_eq!(n.end(), (1 << 39) + 7);
        assert_eq!(n.kind(), Kind::Comment);
        assert_eq!(std::mem::size_of::<Node>(), 24);
    }

    #[test]
    fn structure() {
        let s = "<?xml version=\"1.0\"?>\n<root a=\"1\">\n  <x>hi</x>\n  <y/>\n  <!-- c -->\n  <z><w/></z>\n</root>\n";
        let i = idx(s);
        assert!(i.error.is_none());
        assert_eq!(i.top.len(), 2);
        let root = i.top[1];
        assert_eq!(i.node(root).kind(), Kind::Element);
        let kids = children(&i, root);
        assert_eq!(kids.len(), 4);
        assert!(i.is_inline(kids[0]));
        assert_eq!(i.node(kids[2]).kind(), Kind::Comment);
        assert!(i.is_expandable(kids[3]));
        assert_eq!(src(s, &i, kids[0]), "<x>hi</x>");
        assert!(src(s, &i, root).ends_with("</root>"));
    }

    #[test]
    fn many_nodes_cross_chunks() {
        let mut s = String::from("<r>");
        for k in 0..(CHUNK + 10) {
            s.push_str(&format!("<i n=\"{k}\"/>"));
        }
        s.push_str("</r>");
        let i = idx(&s);
        assert_eq!(i.nodes.len(), CHUNK + 11);
        assert_eq!(i.node(i.top[0]).child_count as usize, CHUNK + 10);
        let off = s.find("n=\"1048580\"").unwrap() as u64;
        let n = i.node_at_offset(off).unwrap();
        assert_eq!(src(&s, &i, n), "<i n=\"1048580\"/>");
    }

    #[test]
    fn offsets_to_nodes() {
        let s = "<a><b>text</b><c/></a>";
        let i = idx(s);
        let pos = s.find("text").unwrap() as u64;
        let n = i.node_at_offset(pos).unwrap();
        assert_eq!(i.node(n).kind(), Kind::Text);
        assert_eq!(i.ancestors(n).len(), 2);
        let c = i.node_at_offset(s.find("<c").unwrap() as u64).unwrap();
        assert_eq!(src(s, &i, c), "<c/>");
    }

    #[test]
    fn broken_document_keeps_partial_index() {
        let s = "<a>\n<b>1</b>\n<c>2</d>\n</a>";
        let i = idx(s);
        let e = i.error.as_ref().expect("error");
        assert_eq!(e.line, 3);
        assert!(i.nodes.len() >= 3);
        assert_eq!(i.node(i.top[0]).end(), s.len() as u64);
    }

    #[test]
    fn bom_is_skipped() {
        let s = "\u{feff}<a>x</a>";
        let i = idx(s);
        assert!(i.error.is_none());
        assert_eq!(i.node(i.top[0]).start(), 3);
    }
}
