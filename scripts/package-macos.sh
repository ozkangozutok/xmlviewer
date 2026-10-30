#!/usr/bin/env bash
# Builds a universal (Apple Silicon + Intel) XML Viewer.app and zips it into dist/.
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
APP=dist/XmlViewer.app

cargo build --release --target aarch64-apple-darwin
cargo build --release --target x86_64-apple-darwin

rm -rf dist && mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
lipo -create -output "$APP/Contents/MacOS/xmlviewer" \
    target/aarch64-apple-darwin/release/xmlviewer \
    target/x86_64-apple-darwin/release/xmlviewer

cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>XmlViewer</string>
    <key>CFBundleDisplayName</key><string>XML / JSON Viewer</string>
    <key>CFBundleIdentifier</key><string>com.github.ozkangozutok.xmlviewer</string>
    <key>CFBundleExecutable</key><string>xmlviewer</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>${VERSION}</string>
    <key>CFBundleVersion</key><string>${VERSION}</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>CFBundleDocumentTypes</key>
    <array>
        <dict>
            <key>CFBundleTypeName</key><string>XML / JSON</string>
            <key>CFBundleTypeRole</key><string>Viewer</string>
            <key>LSHandlerRank</key><string>Alternate</string>
            <key>LSItemContentTypes</key>
            <array><string>public.xml</string><string>public.json</string></array>
        </dict>
    </array>
</dict>
</plist>
EOF

# Ad-hoc signature so the universal binary runs on Apple Silicon.
codesign --force --deep --sign - "$APP"

ZIP="dist/xmlviewer-${VERSION}-macos-universal.zip"
ditto -c -k --keepParent "$APP" "$ZIP"
echo "$ZIP"
