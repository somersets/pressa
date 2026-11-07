"""Regenerates the app icons from icon.svg using Pressa's own SVG renderer.

Run after `cargo build --release`:  python assets/build_icons.py
Outputs: assets/pressa.ico (Windows, multi-size), assets/icon-256.png (window icon),
assets/icon-1024.png (source for the macOS .icns).
"""

import shutil
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

ASSETS = Path(__file__).resolve().parent
ROOT = ASSETS.parent
ICO_SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]
PNG_SIZES = [256, 1024]


def main() -> None:
    cli = ROOT / "target" / "release" / ("pressa-cli.exe" if sys.platform == "win32" else "pressa-cli")
    if not cli.exists():
        sys.exit(f"{cli} not found; run `cargo build --release` first")

    svg = (ASSETS / "icon.svg").read_text(encoding="utf-8")
    with tempfile.TemporaryDirectory() as tmp:
        src, out = Path(tmp, "svg"), Path(tmp, "png")
        src.mkdir()
        for size in sorted(set(ICO_SIZES + PNG_SIZES)):
            sized = svg.replace('width="1024" height="1024"', f'width="{size}" height="{size}"', 1)
            (src / f"icon-{size}.svg").write_text(sized, encoding="utf-8")
        subprocess.run([str(cli), str(src), "-f", "png", "--lossless", "-e", "max", "-o", str(out)], check=True)

        for size in PNG_SIZES:
            shutil.copyfile(out / f"icon-{size}.png", ASSETS / f"icon-{size}.png")
        write_ico([(size, (out / f"icon-{size}.png").read_bytes()) for size in ICO_SIZES], ASSETS / "pressa.ico")
    print("icons written to", ASSETS)


def write_ico(images: list[tuple[int, bytes]], path: Path) -> None:
    """ICO with PNG-compressed entries (supported since Windows Vista)."""
    header = struct.pack("<HHH", 0, 1, len(images))
    offset = len(header) + 16 * len(images)
    entries, blobs = b"", b""
    for size, png in images:
        dim = 0 if size >= 256 else size  # 0 means 256
        entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(png), offset)
        blobs += png
        offset += len(png)
    path.write_bytes(header + entries + blobs)


if __name__ == "__main__":
    main()
