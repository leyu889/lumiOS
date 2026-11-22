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

FONT = r'E:\workbuddy\Deepseek_OS\kernel\res\fonts\SourceHanSansSC-Regular.otf'
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

    # **左对齐**：笔迹左边贴着格子的第 0 列。
    # 等宽格子那套"居中"在比例排版下会互相重叠，必须改成左对齐 + 按字宽推进。
    cx_src = float(bb[0])

    # **区域平均**（box filter）：把目标像素覆盖的源区域取平均，
    # 才有真正的中间灰度。用最近邻的话，30px 直接点到 12px 只会出黑白。
    from PIL import ImageStat
    rows = []
    for y in range(CELL_H):
        sy_c = BASE_SRC + (y - GRID_BASE) / S          # 目标行中心对应的源行
        sy0 = sy_c - 0.5 / S
        sy1 = sy_c + 0.5 / S
        bits = 0
        for x in range(CELL_W):
            sx_c = cx_src + x / S
            sx0 = sx_c - 0.5 / S
            sx1 = sx_c + 0.5 / S
            box = (
                max(0, int(sx0)), max(0, int(sy0)),
                min(SRC_W, int(sx1) + 1), min(SRC_H, int(sy1) + 1),
            )
            if box[2] <= box[0] or box[3] <= box[1]:
                lvl = 0
            else:
                v = ImageStat.Stat(img.crop(box)).mean[0]
                lvl = 3 if v >= 120 else (2 if v >= 70 else (1 if v >= 25 else 0))
            bits |= lvl << (2 * (CELL_W - 1 - x))
        rows.append(bits)
    return rows


out = []
out.append('//! %dx%d 点阵字库（ASCII 0x20..0x7F），由 `tools/mkfont16.py` 从' % (CELL_W, CELL_H))
out.append('//! `res/fonts/SourceHanSansSC-Regular.otf`（思源黑体 Bold）构建期光栅化生成。')
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
adv = []
for code in range(FIRST, LAST + 1):
    ch = chr(code)
    if ch == ' ':
        adv.append(max(3, int(round(f.getlength(ch) * S))))
    else:
        _img = Image.new('L', (SRC_W, SRC_H), 0)
        ImageDraw.Draw(_img).text((SRC_W // 2, BASE_SRC), ch, font=f, fill=255, anchor="ms")
        _bb = _img.getbbox()
        w = (_bb[2] - _bb[0]) if _bb else 4
        adv.append(max(3, min(CELL_W, int(round(w * S)) + 1)))

out.append('/// 每个字形的**推进宽度**（像素）。比例排版靠它累加，')
out.append('/// 等宽格子会让 `i` 和 `m` 占同样宽度，字距看起来就是散的。')
out.append('pub static ADV: [u8; %d] = [' % (LAST - FIRST + 1))
line = '    '
for i, a in enumerate(adv):
    line += '%d, ' % a
    if (i + 1) % 16 == 0:
        out.append(line.rstrip())
        line = '    '
if line.strip():
    out.append(line.rstrip())
out.append('];')
out.append('')
out.append('pub static FONT: [[u32; %d]; %d] = [' % (CELL_H, LAST - FIRST + 1))
for code in range(FIRST, LAST + 1):
    rows = glyph_bits(chr(code))
    out.append('    [')
    line = '        '
    for i, b in enumerate(rows):
        line += '0x%06X, ' % b
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

for ch in 'Ag':
    rows = glyph_bits(ch)
    print('--- %r ---' % ch)
    for b in rows:
        print(''.join('.oO#'[(b >> (2 * (CELL_W - 1 - i))) & 3] for i in range(CELL_W)))
