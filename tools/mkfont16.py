# -*- coding: utf-8 -*-
"""用思源黑体（Bold）在构建期光栅化出 12x16 点阵字库。

## 关键：统一度量

以前的写法是"每个字符裁到笔迹 -> 等比缩放进格子"，结果是
`A` 和 `o` 各按自己的大小填满高度，看起来字高不齐、基线漂移，
整体非常丑（一眼就能看出是拼起来的）。

现在改成**全局统一**：
1. 所有字符用同一个字号渲染到同一条基线；
2. 用字体自身的 ascent/descent 算一个**唯一缩放比** s；
3. 基线固定在格子里的同一行，所有字符按同一个 s 采样。

这样 cap height / x-height / 基线都是字体原生的比例，字就"齐"了。
"""
from PIL import Image, ImageDraw, ImageFont
import io

FONT = r'E:\workbuddy\Deepseek_OS\kernel\res\fonts\SourceHanSansSC-Bold.otf'
OUT = r'E:\workbuddy\Deepseek_OS\kernel\lumikernel\src\font16.rs'

CELL_W, CELL_H = 12, 16
SIZE = 30              # 源字号（大一点再降采样，边缘更干净）
THRESH = 120           # 采样阈值
FIRST, LAST = 0x20, 0x7F

SRC_W, SRC_H = 64, 64
BASE_SRC = 44          # 源画布里的基线行

f = ImageFont.truetype(FONT, SIZE)
ascent, descent = f.getmetrics()

# 唯一缩放：按**字冠高度**（大写 H 的笔迹高）定，而不是行高。
# 行高含大量上留白，按它缩放会让字整体偏小。
_probe = Image.new('L', (SRC_W, SRC_H), 0)
ImageDraw.Draw(_probe).text((SRC_W // 2, BASE_SRC), 'H', font=f, fill=255, anchor="ms")
_bb = _probe.getbbox()
CAP = (_bb[3] - _bb[1]) if _bb else 20
S = 10.0 / float(CAP)          # 字冠占 10px（格子高 16，留出下伸与行距）
GRID_BASE = 13                  # 基线固定在第 13 行


def glyph_bits(ch):
    img = Image.new('L', (SRC_W, SRC_H), 0)
    ImageDraw.Draw(img).text((SRC_W // 2, BASE_SRC), ch, font=f, fill=255, anchor="ms")
    bb = img.getbbox()
    if bb is None:
        return [0] * CELL_H

    # 水平方向按笔迹中心对齐到格子中心（不影响垂直度量）
    cx_src = (bb[0] + bb[2]) / 2.0
    cx_cell = (CELL_W - 1) / 2.0

    rows = []
    for y in range(CELL_H):
        sy = int(round(BASE_SRC + (y - GRID_BASE) / S))
        bits = 0
        for x in range(CELL_W):
            sx = int(round(cx_src + (x - cx_cell) / S))
            ok = (0 <= sx < SRC_W) and (0 <= sy < SRC_H) and img.getpixel((sx, sy)) >= THRESH
            if ok:
                bits |= 1 << (CELL_W - 1 - x)
        rows.append(bits)
    return rows


out = []
out.append('//! %dx%d 点阵字库（ASCII 0x20..0x7F），由 `tools/mkfont16.py` 从' % (CELL_W, CELL_H))
out.append('//! `res/fonts/SourceHanSansSC-Bold.otf`（思源黑体 Bold）构建期光栅化生成。')
out.append('//!')
out.append('//! **所有字形共用同一个缩放比与同一条基线**（由字体 ascent/descent 推出），')
out.append('//! 不是逐字"裁切填满"——后者会让每个字高度不一、基线漂移，看起来是拼的。')
out.append('//!')
out.append('//! 字体：Source Han Sans SC Bold，SIL Open Font License 1.1。')
out.append('//! 许可证见 `res/fonts/LICENSE-SourceHanSans`。')
out.append('')
out.append('/// 字模单元宽（像素）。')
out.append('pub const CELL_W: u32 = %d;' % CELL_W)
out.append('')
out.append('/// 字模单元高（像素）。')
out.append('pub const CELL_H: u32 = %d;' % CELL_H)
out.append('')
out.append('pub const FIRST_CHAR: u8 = 0x%02X;' % FIRST)
out.append('pub const LAST_CHAR: u8 = 0x%02X;' % LAST)
out.append('')
out.append('pub static FONT: [[u16; %d]; %d] = [' % (CELL_H, LAST - FIRST + 1))
for code in range(FIRST, LAST + 1):
    rows = glyph_bits(chr(code))
    out.append('    [')
    line = '        '
    for i, b in enumerate(rows):
        line += '0x%04X, ' % b
        if (i + 1) % 8 == 0:
            out.append(line.rstrip())
            line = '        '
    if line.strip():
        out.append(line.rstrip())
    out.append('    ],')
out.append('];')

io.open(OUT, 'w', encoding='utf-8').write('\n'.join(out) + '\n')
print('生成：%d 字形 %dx%d，缩放 s=%.3f，基线行=%.1f'
      % (LAST - FIRST + 1, CELL_W, CELL_H, S, GRID_BASE))

for ch in 'Agxo':
    rows = glyph_bits(ch)
    print('--- %r ---' % ch)
    for b in rows:
        print(''.join('#' if b & (1 << (CELL_W - 1 - i)) else '.' for i in range(CELL_W)))
