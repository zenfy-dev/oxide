#!/usr/bin/env python3
"""Regenerates assets/icon-*.png and assets/oxide.ico.

Mirrors `icon_rgba` in crates/oxide-gui/src/theme.rs (the window icon) so
packaged icons match the running app. Standard library only.
"""
import math, struct, zlib, pathlib

def icon_rgba(size):
    s = float(size)
    radius, inset = s * 0.22, s * 0.06
    top, bottom = (234.0, 108.0, 52.0), (176.0, 58.0, 22.0)
    out = bytearray()
    for y in range(size):
        for x in range(size):
            fx, fy = x + 0.5, y + 0.5
            dx = abs(fx - s / 2) - (s / 2 - inset - radius)
            dy = abs(fy - s / 2) - (s / 2 - inset - radius)
            outside = math.hypot(max(dx, 0), max(dy, 0)) + min(max(dx, dy), 0) - radius
            cover = min(max(0.5 - outside, 0.0), 1.0)
            t = fy / s
            c = [top[i] + (bottom[i] - top[i]) * t for i in range(3)]
            r = math.hypot(fx - s / 2, fy - s / 2)
            ring = min(max(s * 0.075 - abs(r - s * 0.24), 0.0), 1.0)
            c = [ch + (255 - ch) * ring for ch in c]
            out += bytes([int(c[0]), int(c[1]), int(c[2]), int(cover * 255)])
    return bytes(out)

def png(size):
    raw = icon_rgba(size)
    rows = b"".join(b"\x00" + raw[y * size * 4:(y + 1) * size * 4] for y in range(size))
    chunk = lambda t, d: struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(rows, 9)) + chunk(b"IEND", b""))

root = pathlib.Path(__file__).resolve().parent.parent / "assets"
root.mkdir(exist_ok=True)
pngs = {}
for size in (16, 32, 48, 128, 256, 512, 1024):
    pngs[size] = png(size)
    (root / f"icon-{size}.png").write_bytes(pngs[size])
# ICO with embedded PNG images (supported since Windows Vista).
sizes = (16, 32, 48, 256)
header = struct.pack("<HHH", 0, 1, len(sizes))
offset, entries, blobs = 6 + 16 * len(sizes), b"", b""
for size in sizes:
    data = pngs[size]
    entries += struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(data), offset + len(blobs))
    blobs += data
(root / "oxide.ico").write_bytes(header + entries + blobs)
print("wrote", sorted(p.name for p in root.iterdir()))
