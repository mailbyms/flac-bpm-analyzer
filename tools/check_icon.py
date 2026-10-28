"""检查 PE 文件里的图标资源（严格验证 exe 是否真的嵌入了图标，不依赖任何第三方库）。

用法：python tools/check_icon.py target/release/flac-bpm-analyzer.exe
输出：资源类型清单 + RT_GROUP_ICON 里的各尺寸图标条目
"""

import struct
import sys

RT_ICON = 3
RT_GROUP_ICON = 14


def parse(path):
    data = open(path, "rb").read()
    if data[:2] != b"MZ":
        raise SystemExit("不是 PE 文件")
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    if data[pe : pe + 4] != b"PE\0\0":
        raise SystemExit("PE 签名不对")
    coff = pe + 4
    nsec = struct.unpack_from("<H", data, coff + 2)[0]
    opt_size = struct.unpack_from("<H", data, coff + 16)[0]
    opt = coff + 20
    magic = struct.unpack_from("<H", data, opt)[0]
    dd = opt + (0x60 if magic == 0x10B else 0x70)
    rsrc_rva, rsrc_size = struct.unpack_from("<II", data, dd + 2 * 8)

    sec = opt + opt_size
    sections = []
    for i in range(nsec):
        off = sec + i * 40
        vsize, vaddr, rawsize, rawptr = struct.unpack_from("<IIII", data, off + 8)
        sections.append((vaddr, vsize, rawptr, rawsize))

    def rva2off(rva):
        for vaddr, vsize, rawptr, rawsize in sections:
            if vaddr <= rva < vaddr + max(vsize, rawsize):
                return rawptr + (rva - vaddr)
        raise ValueError(f"RVA 0x{rva:x} 不在任何节内")

    def entries(off):
        named, ids = struct.unpack_from("<HH", data, off + 12)
        out = []
        for i in range(named + ids):
            e = off + 16 + i * 8
            name, ptr = struct.unpack_from("<II", data, e)
            out.append((name, ptr))
        return out

    base = rva2off(rsrc_rva)
    types = {}
    for type_id, ptr in entries(base):
        if ptr & 0x80000000:
            names = entries(base + (ptr & 0x7FFFFFFF))
            types[type_id] = len(names)
    return data, base, types, entries, rva2off


def main():
    path = sys.argv[1] if len(sys.argv) > 1 else "target/release/flac-bpm-analyzer.exe"
    data, base, types, entries, rva2off = parse(path)
    print(f"文件：{path}（{len(data):,} 字节）")
    kind = {RT_ICON: "RT_ICON", RT_GROUP_ICON: "RT_GROUP_ICON"}
    if not types:
        print("资源表为空 → 没有嵌入任何资源")
        return 1
    print("资源类型：" + ", ".join(f"{kind.get(t, t)}×{n}" for t, n in sorted(types.items())))

    if RT_GROUP_ICON not in types:
        print("结论：缺少 RT_GROUP_ICON → exe 不会显示自定义图标")
        return 1

    def resolve(ptr):
        """沿着子目录一路走到叶子（IMAGE_RESOURCE_DATA_ENTRY）。"""
        depth = 0
        while ptr & 0x80000000:
            kids = entries(base + (ptr & 0x7FFFFFFF))
            if not kids or depth > 3:
                return None
            ptr = kids[0][1]
            depth += 1
        return base + ptr

    # 读 RT_GROUP_ICON 的第一个条目，解析 GRPICONDIR
    leaf = None
    for type_id, ptr in entries(base):
        if type_id == RT_GROUP_ICON:
            leaf = resolve(ptr)
            break
    if leaf is None:
        print("结论：无法定位 RT_GROUP_ICON 数据")
        return 1
    entry_rva, entry_size, _codepage = struct.unpack_from("<III", data, leaf)
    g = rva2off(entry_rva)

    _reserved, gtype, count = struct.unpack_from("<HHH", data, g)
    sizes = []
    for i in range(count):
        e = g + 6 + i * 14
        w, h, _colors, _res, _planes, _bits, nbytes, _iid = struct.unpack_from(
            "<BBBBHHIH", data, e
        )
        sizes.append((w or 256, h or 256, nbytes))
    print(f"RT_GROUP_ICON：{count} 个条目")
    for w, h, nbytes in sizes:
        print(f"  {w:>3}×{h:<3}  {nbytes:>7,} 字节")
    print("结论：exe 已嵌入自定义图标")
    return 0


if __name__ == "__main__":
    sys.exit(main())
