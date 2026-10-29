//! JSON test file generator: `cargo run --release --example genjson -- out.json 1024 [--broken]`
//! (size in MB). One big array of objects with nesting, Turkish text and escapes.

use std::fs::File;
use std::io::{BufWriter, Write};

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).map(String::as_str).unwrap_or("test.json");
    let mb: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1024);
    let broken = args.iter().any(|a| a == "--broken");
    let target = mb * 1024 * 1024;

    let mut w = BufWriter::with_capacity(1 << 20, File::create(path)?);
    let mut n: u64 = 0;
    macro_rules! out { ($($t:tt)*) => {{ let s = format!($($t)*); n += s.len() as u64; w.write_all(s.as_bytes())?; }} }

    out!("{{\n  \"başlık\": \"Büyük test — şğüöçıİ\",\n  \"kaçış\": \"tırnak \\\" ve \\u015f\\u0131 \\n satır\",\n");
    out!("  \"derin\": ");
    for _ in 0..2000 {
        out!("{{\"k\":");
    }
    out!("\"dip\"");
    for _ in 0..2000 {
        out!("}}");
    }
    out!(",\n  \"urunler\": [\n");
    let mut i: u64 = 0;
    while n < target {
        if i > 0 {
            out!(",\n");
        }
        out!(
            "    {{\"id\": {i}, \"ad\": \"Ürün {i}\", \"fiyat\": {}.{:02}, \"stokta\": {}, \"etiketler\": [\"a\", \"b\"], \"detay\": {{\"aciklama\": \"Açıklama #{i}\", \"not\": null}}}}",
            i % 1000,
            i % 100,
            i % 3 != 0
        );
        i += 1;
    }
    if broken {
        out!(",\n    {{\"id\": \"bozuk\" \"eksik\": 1}}");
    }
    out!("\n  ]\n}}\n");
    w.flush()?;
    eprintln!("{path}: {n} bytes, {i} urun");
    Ok(())
}
