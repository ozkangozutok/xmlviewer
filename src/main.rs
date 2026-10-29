#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod download;
mod index;
mod json;
mod search;
mod text;

use std::time::Instant;

fn headless_index(path: &str) {
    let file = std::fs::File::open(path).expect("open");
    // SAFETY: read-only mapping of a file we just opened.
    let data = unsafe { memmap2::Mmap::map(&file) }.expect("mmap");
    let t = Instant::now();
    let idx = index::build(&data, &index::Progress::default());
    let secs = t.elapsed().as_secs_f64();
    println!("bytes: {}", data.len());
    println!("nodes: {}", idx.nodes.len());
    println!("top-level: {}", idx.top.len());
    println!("seconds: {secs:.3}");
    println!("MB/s: {:.0}", data.len() as f64 / 1_048_576.0 / secs);
    println!(
        "index memory: {}",
        text::fmt_bytes((idx.nodes.memory()) as u64)
    );
    if idx.cancelled {
        println!("cancelled");
    }
    if let Some(e) = &idx.error {
        println!("error at line {} col {} (offset {}): {}", e.line, e.column, e.offset, e.message);
    }
}

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 3 && args[1] == "--index-only" {
        headless_index(&args[2]);
        return Ok(());
    }
    let arg = args.get(1).cloned();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("XML / JSON Viewer")
            .with_inner_size([1400.0, 850.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("XML Viewer", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, arg)))))
}
