# XML / JSON Viewer

A fast desktop viewer for very large (multi-GB) XML and JSON files, written in Rust.

Most editors and browsers freeze or run out of memory on files this size. XML / JSON Viewer memory-maps the file, indexes its structure in a single pass (around 650 MB/s), and shows it as a tree you can scroll, search and copy from right away.

## Features

- **Huge files:** the file is memory-mapped and never loaded fully into RAM. Files up to 1 TB are supported.
- **Instant tree view:** the tree is virtualized, so even a node with millions of children scrolls smoothly.
- **XML and JSON:** JSON is detected automatically (the first character is `{` or `[`). JSON Lines / NDJSON (one value per line) also works.
- **Search:** full-text or regex search runs in the background, with optional case matching. Move between results with F3 / Shift+F3.
- **Details panel:** the path of the selected node (`/catalog/products/product` or `$.items[3].name`), its attributes or fields, byte range and source.
- **Open from a URL:** paste an `http://` or `https://` address and the file is downloaded with a progress bar, then opened.
- **Encodings:** UTF-8, plus any ASCII-compatible encoding declared in the XML header (windows-1252, windows-1254 / ISO-8859-9, Shift_JIS, ...).
- **Malformed files:** the error line and column are shown, and everything read up to that point stays viewable.

## Download

Get the latest build from the [Releases page](https://github.com/ozkangozutok/xmlviewer/releases).

### macOS

1. Download `xmlviewer-<version>-macos-universal.zip` (Apple Silicon and Intel, macOS 11 or later).
2. Unzip it and move `XmlViewer.app` to Applications.
3. The app is not notarized, so on first launch right-click it and choose **Open**, or run:

   ```bash
   xattr -dr com.apple.quarantine /Applications/XmlViewer.app
   ```

### Windows and Linux

There are no prebuilt binaries yet. [Build from source](#build-from-source) instead.

## Usage

- **Open a file:** click **Open…** (Ctrl+O / ⌘O), drag a file onto the window, or pass it on the command line: `xmlviewer file.xml`.
- **Open a URL:** type the address in the URL box (Ctrl+L / ⌘L) and press Enter or **Download**, or run `xmlviewer https://example.com/data.xml`. Downloads go to a temporary `xmlviewer-downloads` folder and are cleaned up automatically.
- **Search:** Ctrl+F / ⌘F, then Enter. F3 goes to the next result, Shift+F3 to the previous one.

| Key | Action |
| --- | --- |
| ↑ ↓ PgUp PgDn Home End | Move the selection |
| → | Expand |
| ← | Collapse, or go to the parent |
| Enter | Toggle expand / collapse |

Right-click a row to copy its name, key, value, path or source.

### Memory use

The index takes 24 bytes per node. Dense JSON (where every value is a node) can need about 1.5× the file size in RAM; typical XML needs much less.

### Limitations

- UTF-16 files are not supported. Convert them to UTF-8 first.
- Do not modify a file while it is open in the viewer.

## Build from source

Requires a recent stable [Rust](https://rustup.rs) toolchain (edition 2024, Rust 1.85 or later).

```bash
cargo build --release
```

The binary is `target/release/xmlviewer` (`xmlviewer.exe` on Windows).

To build the universal macOS app bundle and zip used for releases:

```bash
./scripts/package-macos.sh
```

### Test data and benchmarks

Generate large test files (the size is in MB):

```bash
cargo run --release --example gen -- testdata/big.xml 1024            # 1 GB XML
cargo run --release --example gen -- testdata/broken.xml 50 --broken  # malformed XML
cargo run --release --example genjson -- testdata/big.json 1024       # 1 GB JSON
```

Measure indexing speed without opening the window:

```bash
cargo run --release -- --index-only testdata/big.xml
```

Run the tests:

```bash
cargo test
```

## Contributing

Issues and pull requests are welcome. Please run `cargo test` before opening a pull request.
