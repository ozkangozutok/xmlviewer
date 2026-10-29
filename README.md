# XML / JSON Viewer (Rust)

A desktop viewer built for very large (multi-GB) XML and JSON files.

- JSON is detected automatically (first character `{` or `[`); JSON Lines (one value per line) also works. Tree rows show `key: value`, collapsed objects show a preview of their first fields, and the path is shown as `$.items[3].name`. Note: in JSON every value is a node, so dense JSON can need ~1.5x the file size in RAM.

- The file is memory-mapped (`memmap2`) and never loaded fully into RAM.
- A single pass with `quick-xml` builds a structural index (24 bytes per node, ~650 MB/s).
- The tree is virtualized: a node with millions of children scrolls smoothly.
- Names, attributes and text are decoded only for the rows on screen (UTF-8, windows-1254 / ISO-8859-9, ...).
- Full-text / regex search runs in the background; F3 / Shift+F3 move between results.
- For malformed XML it reports the error line and column, and the part read so far stays viewable.

## Build

```bash
cargo build --release
```

Executable: `target/release/xmlviewer.exe`

## Usage

- `xmlviewer.exe file.xml`, the **Aç…** button (Ctrl+O), or drag and drop onto the window.
- From the internet: type the address in the URL box at the top and press Enter / **İndir** (Ctrl+L), or `xmlviewer.exe https://site.com/data.xml`. The file streams to a temp folder (`%TEMP%/xmlviewer-downloads`) with a progress bar, then opens like a local file; temp files are cleaned up automatically.
- Keys: ↑ ↓ PgUp PgDn Home End, → expand, ← collapse / go to parent, Enter toggle, Ctrl+F search, F3 next result.
- Right-click: copy name / path / source.
- Benchmark: `xmlviewer.exe --index-only file.xml > out.txt`

## Test data

```bash
cargo run --release --example gen -- testdata/big.xml 1024            # 1 GB
cargo run --release --example gen -- testdata/broken.xml 50 --broken  # malformed
cargo run --release --example genjson -- testdata/big.json 1024       # 1 GB JSON
```

Limits: UTF-16 files are not supported (convert to UTF-8). Do not modify the file while it is open.
