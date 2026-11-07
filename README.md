# Pressa

Fast desktop image converter and compressor for Windows and macOS, written in Rust.

- **GUI** (`Pressa`): drag & drop files or folders, pick a format and quality, convert in parallel.
- **CLI** (`pressa-cli`): the same engine for scripts and batch jobs.

## Formats

| Format | Read | Write | Encoder |
|---|---|---|---|
| JPEG | ✓ | ✓ | mozjpeg (progressive, optimized scans) |
| PNG | ✓ | ✓ | lossy: libimagequant palette + oxipng; lossless: oxipng |
| WebP | ✓ | ✓ | libwebp (lossy and lossless) |
| AVIF | ✓ | ✓ | rav1e via ravif; decoding via rav1d |
| GIF | ✓ (first frame) | ✓ | libimagequant palette |
| TIFF | ✓ | ✓ | Deflate + predictor |
| BMP, ICO, TGA, QOI | ✓ | ✓ | `image` crate |
| HEIC / HEIF (iPhone photos) | ✓ | — | `heic` (pure Rust HEVC, tile-parallel) |
| JPEG XL | ✓ | — | jxl-oxide |
| SVG | ✓ | — | resvg (system fonts) |
| Camera RAW (CR2/CR3/NEF/ARW/DNG/RAF/ORF/RW2…) | ✓ | — | rawler |
| PNM, DDS, OpenEXR, HDR, Farbfeld | ✓ | — | `image` crate |

Processing pipeline: decode → apply EXIF orientation → convert embedded ICC profile to sRGB → resize (SIMD Lanczos3) → encode.
Metadata (EXIF/GPS) is stripped from outputs.

Compression features:
- **Target file size**: binary search for the highest quality that fits.
- **Keep original if not smaller**: re-compressing a file in the same format never makes it bigger.
- **Effort** levels (Fast / Balanced / Max compression) map to each encoder's speed settings.
- Batches run on all CPU cores (rayon).

## Building

Prerequisites:

- Rust (stable, via [rustup](https://rustup.rs))
- **Windows**: Visual Studio Build Tools with the "Desktop development with C++" workload, and [NASM](https://www.nasm.us)
  either on `PATH` or set in `~/.cargo/config.toml`:
  ```toml
  [env]
  NASM = 'C:\path\to\nasm.exe'
  ```
- **macOS**: Xcode Command Line Tools (`xcode-select --install`), `brew install nasm`

```bash
cargo build --release
```

Binaries land in `target/release/`: `Pressa(.exe)` (GUI) and `pressa-cli(.exe)`.

## Installers

**Windows** (needs [Inno Setup 6](https://jrsoftware.org/isinfo.php): `winget install JRSoftware.InnoSetup`):

```powershell
powershell -ExecutionPolicy Bypass -File installer\windows\build.ps1
```

Produces `target\installer\Pressa-Setup-<version>.exe`: a per-user install (no admin prompt) into
`%LocalAppData%\Programs\Pressa` with a Start menu entry, an uninstaller, and optional
"Open with Pressa" right-click menu, "Send to" entry (handles many files at once), desktop icon,
and `pressa-cli` on `PATH`. Silent install: `Pressa-Setup-<version>.exe /VERYSILENT`.

**macOS** (run on a Mac):

```bash
./installer/macos/build-dmg.sh            # current architecture
UNIVERSAL=1 ./installer/macos/build-dmg.sh # Apple Silicon + Intel
```

Produces `target/installer/Pressa.app` and `Pressa-<version>.dmg` (ad-hoc signed, not notarized:
fine on the Mac that built it; on another Mac, right-click → Open the first time).

App icons are generated from `assets/icon.svg` with `python assets/build_icons.py`.

## CLI usage

```bash
# Convert a folder to WebP at quality 80, writing into ./out
pressa-cli photos/ -r -f webp -q 80 -o out

# Compress JPEGs in place to fit 300 KB, max 1920 px wide
pressa-cli *.jpg -t 300KB --max-width 1920 --overwrite

# Lossless PNG optimization with maximum effort
pressa-cli screenshots/ -f png --lossless -e max
```

Run `pressa-cli --help` for all options.

## Licensing note

Pressa's own code is MIT. The HEIC decoder (`heic` crate) is AGPL-3.0, so binaries built with it
(including the ones on the Releases page) are covered by AGPL-3.0 as a whole; publishing the source,
as this repository does, satisfies that. For a closed-source build, disable it
(`--no-default-features --features raw,svg,jxl,avif-decode`) or get Imazen's commercial license.
HEVC may also be covered by patents.

## Continuous builds

`.github/workflows/installers.yml` builds the Windows installer and a universal macOS DMG on
GitHub's machines. Start it from the **Actions** tab ("Installers" → **Run workflow**) and download the
files from the run's **Artifacts**, or push a tag (`git tag v0.2.0 && git push origin v0.2.0`) to also
publish them as a GitHub Release.

## Project layout

- `crates/core`: decoding, transforms, encoders, batch pipeline
- `crates/cli`: command-line front end
- `crates/app`: egui desktop app (OpenGL via glow)
