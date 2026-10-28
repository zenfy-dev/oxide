#!/usr/bin/env python3
"""Regenerates every icon from assets/icon-source.png (1024x1024, transparent).

Outputs:
  assets/icon-{16..1024}.png   bundle, AppImage and docs icons
  assets/oxide.ico             Windows executable and installer icon
  assets/icon-256.rgba         raw RGBA embedded in the app (window icon, empty screen)

Requires Pillow:  python3 -m pip install pillow
"""
import pathlib

from PIL import Image

assets = pathlib.Path(__file__).resolve().parent.parent / "assets"
source = Image.open(assets / "icon-source.png").convert("RGBA")
assert source.size == (1024, 1024), "icon-source.png must be 1024x1024"

for size in (16, 32, 48, 128, 256, 512, 1024):
    source.resize((size, size), Image.LANCZOS).save(assets / f"icon-{size}.png", optimize=True)

source.save(assets / "oxide.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
(assets / "icon-256.rgba").write_bytes(source.resize((256, 256), Image.LANCZOS).tobytes())
print("wrote", sorted(p.name for p in assets.iterdir()))
