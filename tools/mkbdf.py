#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""把 BDF 位图字体转成内核能直接用的 Rust 数组（8x16，ASCII 0x20..0x7F）。

为什么要转而不是内核里解析 BDF：内核是 no_std，连格式化都由自己实现，
不可能在启动路径上解析文本格式的字体文件。字体是**常量数据**，
在构建期一次性摊平成数组，运行时零成本。

用法：
    python tools/mkbdf.py res/fonts/spleen-8x16.bdf lumikernel/src/font8x16.rs
"""

import sys

CELL_W = 8
CELL_H = 16
FIRST = 0x20
LAST = 0x7F


def parse_bdf(path):
    """返回 ({编码: [[0/1]*8 x16]}, ascent)，按 BDF 的 BBX 偏移放进位图单元。"""
    glyphs = {}
    ascent = 12
    cur = None
    bbx = None
    rows = []
    in_bitmap = False
    with open(path, "r", encoding="utf-8", errors="replace") as f:
        for line in f:
            parts = line.split()
            if not parts:
                continue
            tag = parts[0]
            if tag == "STARTCHAR":
                cur = None
                rows = []
                in_bitmap = False
            elif tag == "ENCODING":
                cur = int(parts[1])
            elif tag == "FONT_ASCENT":
                ascent = int(parts[1])
            elif tag == "BBX":
                # BBX 宽 高 xoff yoff
                bbx = tuple(int(x) for x in parts[1:5])
            elif tag == "BITMAP":
                in_bitmap = True
                rows = []
            elif tag == "ENDCHAR":
                if cur is not None and bbx is not None:
                    glyphs[cur] = compose(bbx, rows, ascent)
                in_bitmap = False
            elif in_bitmap:
                rows.append(parts[0])
    return glyphs, ascent


def compose(bbx, rows, ascent):
    """把 BBX 描述的位图放进 8x16 的单元里（BDF 原点在基线、y 向上）。

    关键：BDF 的 y 是**相对基线向上**的，而我们的位图行号是从屏幕顶部向下。
    换算公式 screen_row = ascent - yoff - h + r。
    漏掉 ascent 会让整个字库下移 |yoff| 行——Spleen 的 yoff 是 -4，
    于是所有字都往下掉 4 行、底部被裁掉（踩过）。
    """
    w, h, xoff, yoff = bbx
    cell = [[0] * CELL_W for _ in range(CELL_H)]
    # 位图每行字节数（BDF 高位在左）
    nbytes = (w + 7) // 8
    for r, hexrow in enumerate(rows[:h]):
        try:
            val = int(hexrow, 16)
        except ValueError:
            continue
        bits = nbytes * 8
        for i in range(w):
            bit = (val >> (bits - 1 - i)) & 1
            if not bit:
                continue
            x = xoff + i
            y = ascent - yoff - h + r
            if 0 <= x < CELL_W and 0 <= y < CELL_H:
                cell[y][x] = 1
    return cell


def main():
    if len(sys.argv) != 3:
        print(__doc__)
        return 2
    src, dst = sys.argv[1], sys.argv[2]
    glyphs, _ascent = parse_bdf(src)

    out = []
    out.append("//! 8x16 位图字库（ASCII 0x20..0x7F），由 `tools/mkbdf.py` 从")
    out.append("//! `res/fonts/spleen-8x16.bdf` 生成——**不要手改本文件**。")
    out.append("//!")
    out.append("//! 字体：Spleen 8x16，Copyright (c) 2018-2026 Frederic Cambus，")
    out.append("//! BSD 2-Clause。许可证全文见 `res/fonts/LICENSE-spleen`。")
    out.append("//!")
    out.append("//! 每个字形 16 行，每行 1 字节，最高位是最左边的像素。")
    out.append("")
    out.append("/// 字库起始编码。")
    out.append("pub const FIRST_CHAR: u8 = 0x%02X;" % FIRST)
    out.append("")
    out.append("/// 字库结束编码（含）。")
    out.append("pub const LAST_CHAR: u8 = 0x%02X;" % LAST)
    out.append("")
    out.append("/// `FONT[(ch - FIRST_CHAR) as usize]` = 该字符的 16 行位图。")
    out.append("pub static FONT: [[u8; %d]; %d] = [" % (CELL_H, LAST - FIRST + 1))

    missing = []
    for code in range(FIRST, LAST + 1):
        cell = glyphs.get(code)
        if cell is None:
            missing.append(code)
            cell = [[0] * CELL_W for _ in range(CELL_H)]
        out.append("    [")
        for y in range(CELL_H):
            byte = 0
            for x in range(CELL_W):
                if cell[y][x]:
                    byte |= 1 << (7 - x)
            out.append("        0x%02X," % byte)
        out.append("    ],")
    out.append("];")
    out.append("")

    with open(dst, "w", encoding="utf-8") as f:
        f.write("\n".join(out))

    print("生成 %s：%d 个字形" % (dst, LAST - FIRST + 1))
    if missing:
        print("缺字（已填空白）：", ", ".join("0x%02X" % c for c in missing))
    return 0


if __name__ == "__main__":
    sys.exit(main())
