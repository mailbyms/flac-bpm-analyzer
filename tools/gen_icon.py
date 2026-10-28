"""生成应用图标（零依赖：仅用标准库 zlib/struct）。

几何与主界面左上角的 SVG 完全一致（24×24 viewBox）：

    <rect x="1" y="1" width="22" height="22" rx="7" fill="#E07856"/>
    <path d="M5.5 12h1.6 M9.4 8.6v6.8 M12.8 6v12 M16.2 9.6v4.8 M19.6 12h-1.6"
          stroke="#FFFFFF" stroke-width="1.8" stroke-linecap="round"/>

输出：
  assets/icon.ico        多尺寸（16/24/32/48/64/128 用 32bit BMP，256 用 PNG），给 exe 用
  assets/icon_256.png    预览用
  assets/icon_256.rgba   256×256 原始 RGBA，给窗口图标 include_bytes! 用（免解码依赖）

用法：python tools/gen_icon.py
"""

import math
import os
import struct
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
ASSETS = os.path.normpath(os.path.join(HERE, "..", "assets"))

BG = (0xE0, 0x78, 0x56)  # #E07856 主色橙
FG = (0xFF, 0xFF, 0xFF)  # 波形白

# 圆角方块：x, y, w, h, r（24 网格）
RECT = (1.0, 1.0, 22.0, 22.0, 7.0)

# 波形：三段竖线 + 两端水平短线
STROKES = [
    ((5.5, 12.0), (7.1, 12.0)),
    ((9.4, 8.6), (9.4, 15.4)),
    ((12.8, 6.0), (12.8, 18.0)),
    ((16.2, 9.6), (16.2, 14.4)),
    ((19.6, 12.0), (18.0, 12.0)),
]
STROKE_W = 1.8


def in_round_rect(u, v, x, y, w, h, r):
    if u < x or u > x + w or v < y or v > y + h:
        return False
    cx = min(max(u, x + r), x + w - r)
    cy = min(max(v, y + r), y + h - r)
    return (u - cx) ** 2 + (v - cy) ** 2 <= r * r


def dist_seg(u, v, a, b):
    ax, ay = a
    bx, by = b
    dx, dy = bx - ax, by - ay
    l2 = dx * dx + dy * dy
    if l2 == 0.0:
        return math.hypot(u - ax, v - ay)
    t = ((u - ax) * dx + (v - ay) * dy) / l2
    t = 0.0 if t < 0.0 else (1.0 if t > 1.0 else t)
    return math.hypot(u - (ax + t * dx), v - (ay + t * dy))


def render(size, ss):
    """超采样渲染：返回 size×size 的 RGBA 字节（逐样本累积以得到平滑边缘）。"""
    inv = 24.0 / size
    half = STROKE_W / 2.0
    total = float(ss * ss)
    out = bytearray()
    for py in range(size):
        for px in range(size):
            ar = ag = ab = 0.0
            covered = 0
            for sy in range(ss):
                for sx in range(ss):
                    u = (px + (sx + 0.5) / ss) * inv
                    v = (py + (sy + 0.5) / ss) * inv
                    if not in_round_rect(u, v, *RECT):
                        continue
                    white = any(dist_seg(u, v, a, b) <= half for a, b in STROKES)
                    r, g, b = FG if white else BG
                    ar += r
                    ag += g
                    ab += b
                    covered += 1
            if covered == 0:
                out += b"\x00\x00\x00\x00"
            else:
                out += bytes(
                    (
                        round(ar / covered),
                        round(ag / covered),
                        round(ab / covered),
                        round(covered / total * 255.0),
                    )
                )
    return bytes(out)


def png_bytes(rgba, w, h):
    raw = bytearray()
    stride = w * 4
    for y in range(h):
        raw.append(0)  # filter type: none
        raw += rgba[y * stride : (y + 1) * stride]

    def chunk(tag, data):
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    ihdr = struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", ihdr)
        + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + chunk(b"IEND", b"")
    )


def bmp_entry(rgba, w, h):
    """ICO 内的 32bit BMP：BITMAPINFOHEADER + bottom-up BGRA + AND 掩码。"""
    hdr = struct.pack("<IiiHHIIiiII", 40, w, h * 2, 1, 32, 0, w * h * 4, 0, 0, 0, 0)
    px = bytearray()
    stride = w * 4
    for y in range(h - 1, -1, -1):
        row = rgba[y * stride : (y + 1) * stride]
        for x in range(w):
            r, g, b, a = row[x * 4 : x * 4 + 4]
            px += bytes((b, g, r, a))
    mask_stride = ((w + 31) // 32) * 4
    mask = b"\x00" * (mask_stride * h)
    return hdr + bytes(px) + mask


def ico_bytes(entries):
    count = len(entries)
    head = struct.pack("<HHH", 0, 1, count)
    offset = 6 + 16 * count
    dirs = b""
    blobs = b""
    for size, data in entries:
        dim = 0 if size >= 256 else size
        dirs += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
        blobs += data
    return head + dirs + blobs


def main():
    os.makedirs(ASSETS, exist_ok=True)

    # 大尺寸少采样、小尺寸多采样（像素少，多采样成本低且边缘收益明显）
    sizes = [(16, 8), (24, 8), (32, 6), (48, 5), (64, 4), (128, 3), (256, 3)]
    rendered = {}
    for size, ss in sizes:
        rendered[size] = render(size, ss)
        print(f"  渲染 {size}×{size}（{ss}×{ss} 超采样）")

    # 256 用 PNG 条目，其余用 BMP（兼容性最好）
    entries = []
    for size, _ in sizes:
        data = rendered[size]
        if size >= 256:
            entries.append((size, png_bytes(data, size, size)))
        else:
            entries.append((size, bmp_entry(data, size, size)))

    ico_path = os.path.join(ASSETS, "icon.ico")
    with open(ico_path, "wb") as f:
        f.write(ico_bytes(entries))
    print(f"  写出 {ico_path}（{os.path.getsize(ico_path)} 字节）")

    png_path = os.path.join(ASSETS, "icon_256.png")
    with open(png_path, "wb") as f:
        f.write(png_bytes(rendered[256], 256, 256))
    print(f"  写出 {png_path}（{os.path.getsize(png_path)} 字节）")

    rgba_path = os.path.join(ASSETS, "icon_256.rgba")
    with open(rgba_path, "wb") as f:
        f.write(rendered[256])
    print(f"  写出 {rgba_path}（{os.path.getsize(rgba_path)} 字节）")


if __name__ == "__main__":
    main()
