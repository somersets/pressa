#!/usr/bin/env bash
# Builds Pressa.app and target/installer/Pressa-<version>.dmg. Run on macOS.
# Requires: Xcode Command Line Tools, nasm (brew install nasm).
# UNIVERSAL=1 builds one binary for both Apple Silicon and Intel
# (needs: rustup target add aarch64-apple-darwin x86_64-apple-darwin).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
VERSION="$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)"
OUT="$ROOT/target/installer"
APP="$OUT/Pressa.app"

if [[ "${UNIVERSAL:-0}" == "1" ]]; then
    for t in aarch64-apple-darwin x86_64-apple-darwin; do
        cargo build --release --target "$t"
    done
    BIN="$ROOT/target/universal"
    mkdir -p "$BIN"
    for b in Pressa pressa-cli; do
        lipo -create -output "$BIN/$b" \
            "target/aarch64-apple-darwin/release/$b" "target/x86_64-apple-darwin/release/$b"
    done
else
    cargo build --release
    BIN="$ROOT/target/release"
fi

# Static FFmpeg for video compression (https://ffmpeg.martin-riedl.de, GPL build).
fetch_ffmpeg() { # $1 = arm64 | amd64
    local dir="$ROOT/target/ffmpeg-macos/$1"
    if [[ ! -x "$dir/ffmpeg" ]]; then
        mkdir -p "$dir"
        curl -sSfL -o "$dir/ffmpeg.zip" "https://ffmpeg.martin-riedl.de/redirect/latest/macos/$1/release/ffmpeg.zip"
        unzip -oq "$dir/ffmpeg.zip" -d "$dir"
        rm "$dir/ffmpeg.zip"
        chmod +x "$dir/ffmpeg"
    fi
}
FFMPEG="$ROOT/target/ffmpeg-macos/ffmpeg"
if [[ "${UNIVERSAL:-0}" == "1" ]]; then
    fetch_ffmpeg arm64
    fetch_ffmpeg amd64
    lipo -create -output "$FFMPEG" "$ROOT/target/ffmpeg-macos/arm64/ffmpeg" "$ROOT/target/ffmpeg-macos/amd64/ffmpeg"
else
    ARCH="$([[ "$(uname -m)" == "arm64" ]] && echo arm64 || echo amd64)"
    fetch_ffmpeg "$ARCH"
    cp "$ROOT/target/ffmpeg-macos/$ARCH/ffmpeg" "$FFMPEG"
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN/Pressa" "$BIN/pressa-cli" "$FFMPEG" "$APP/Contents/MacOS/"

# Icon: build an .iconset from the 1024 px master.
ICONSET="$(mktemp -d)/Pressa.iconset"
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
    sips -z $s $s assets/icon-1024.png --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
    sips -z $((s * 2)) $((s * 2)) assets/icon-1024.png --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/Pressa.icns"

cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Pressa</string>
    <key>CFBundleDisplayName</key><string>Pressa</string>
    <key>CFBundleIdentifier</key><string>app.pressa.Pressa</string>
    <key>CFBundleExecutable</key><string>Pressa</string>
    <key>CFBundleIconFile</key><string>Pressa</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>LSApplicationCategoryType</key><string>public.app-category.photography</string>
</dict>
</plist>
EOF

# Ad-hoc signature: required for binaries to run on Apple Silicon.
codesign --force --deep --sign - "$APP"

STAGE="$(mktemp -d)"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
DMG="$OUT/Pressa-$VERSION.dmg"
rm -f "$DMG"
hdiutil create -volname "Pressa" -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null
echo "Built $DMG"
