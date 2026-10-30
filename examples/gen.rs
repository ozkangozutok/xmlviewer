//! Test file generator: `cargo run --release --example gen -- out.xml 1024 [--broken]`
//! (size in MB). Produces deep nesting, a minified single-line section,
//! CDATA, comments, non-ASCII text and one parent with millions of children.

use std::fs::File;
use std::io::{BufWriter, Write};

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).map(String::as_str).unwrap_or("test.xml");
    let mb: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1024);
    let broken = args.iter().any(|a| a == "--broken");
    let target = mb * 1024 * 1024;

    let mut w = BufWriter::with_capacity(1 << 20, File::create(path)?);
    let mut n: u64 = 0;
    macro_rules! out { ($($t:tt)*) => {{ let s = format!($($t)*); n += s.len() as u64; w.write_all(s.as_bytes())?; }} }

    out!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!-- XML Viewer test file -->\n");
    out!("<catalog xmlns:x=\"urn:test\" versión=\"1\">\n");
    out!("  <meta>\n    <título>Big test — şğüöçıİ ŞĞÜÖÇ äß ñ 日本語</título>\n");
    out!("    <raw><![CDATA[ <tag> & markup ]]></raw>\n    <?process instruction?>\n  </meta>\n");

    out!("  <deep>");
    for d in 0..2000 {
        out!("<k{} d=\"{}\">", d % 10, d);
    }
    out!("bottom");
    for d in (0..2000).rev() {
        out!("</k{}>", d % 10);
    }
    out!("</deep>\n");

    out!("  <single-line>");
    for i in 0..20000 {
        out!("<v i=\"{i}\">{}</v>", i * 7);
    }
    out!("</single-line>\n");

    out!("  <products>\n");
    let mut i: u64 = 0;
    while n < target {
        out!(
            "    <product id=\"{i}\" name=\"Product {i}\" price=\"{}.{:02}\" x:code=\"K-{:08X}\"><description>Description &amp; details #{i} — café</description><stock/></product>\n",
            i % 1000,
            i % 100,
            i * 2654435761 % 0xFFFF_FFFF
        );
        if i % 100_000 == 0 {
            out!("    <!-- block {i} -->\n");
        }
        i += 1;
    }
    if broken {
        out!("    <product id=\"broken\"><description>unclosed</wrong>\n");
    }
    out!("  </products>\n</catalog>\n");
    w.flush()?;
    eprintln!("{path}: {} bytes, {i} products", n);
    Ok(())
}
