//! 帧缓冲驱动与图形控制台。
//!
//! 启动链（`boot/stage2.asm`）在实模式里通过 VBE 设好 1024x768x32 的线性
//! 帧缓冲，并把参数写到物理地址 `0x9380` 的交接块：
//!
//! ```text
//! 0x9380  u32  magic 'LVID'（0 = 没设成功，内核退回 VGA 文本模式）
//! 0x9384  u32  width
//! 0x9388  u32  height
//! 0x938C  u32  pitch（字节/行，可能大于 width*4）
//! 0x9390  u32  bpp
//! 0x9394  u32  物理基址
//! 0x9398  u32  VBE 模式号（仅诊断用）
//! ```
//!
//! ## 为什么要单独映射，不能直接映射区凑合
//!
//! 直接映射区只覆盖物理内存前 1 GiB（`DIRECT_MAP_BASE + phys`）。显卡的
//! 帧缓冲通常在 `0xE0000000` 附近——**在 1 GiB 之外**，直接映射过去没有页表
//! 支撑，第一次写就是 #PF。所以这里单独在 `FB_VA` 处建一段页表。
//!
//! ## 为什么先映射 4 KiB 页
//!
//! 帧缓冲 3 MiB = 768 个页，启动期映射一遍约几十微秒，可以接受。
//! 真正的收益是**不引入新的失败模式**：2 MiB 大页要求物理地址 2 MiB 对齐，
//! 而 VBE 给的基址只保证页对齐。等以后做 DRM 时再优化。

use crate::mm::paging;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// 帧缓冲映射到内核的虚拟基址（绕开直接映射区，独立窗口）。
pub const FB_VA: u64 = 0xFFFF_C000_0000_0000;

const HANDOFF_PHYS: u64 = 0x9380;
const MAGIC: u32 = 0x4449_564C; // 'LVID'

/// 字模单元宽高（与 `font8x16` 一致）。
pub const CELL_W: u32 = crate::font16::CELL_W; // 16
pub const CELL_H: u32 = crate::font16::CELL_H; // 16

// 调色板：深色底 + 浅色字，接近现代终端的观感。
pub const BG: u32 = 0x0010_141C;
pub const FG: u32 = 0x00E6_E8EC;
pub const FG_DIM: u32 = 0x0080_8A99;
pub const ACCENT: u32 = 0x007A_A2F7;

static READY: AtomicBool = AtomicBool::new(false);
static W: AtomicU32 = AtomicU32::new(0);
static H: AtomicU32 = AtomicU32::new(0);
static PITCH: AtomicU32 = AtomicU32::new(0);
/// 帧缓冲物理基址（用户态映射时要用）。
static FB_PHYS: AtomicU64 = AtomicU64::new(0);

/// 文本光标（行为单位，不是像素）。
static ROW: AtomicU32 = AtomicU32::new(0);
static COL: AtomicU32 = AtomicU32::new(0);

pub fn ready() -> bool {
    READY.load(Ordering::Acquire)
}

pub fn width() -> u32 {
    W.load(Ordering::Relaxed)
}

pub fn height() -> u32 {
    H.load(Ordering::Relaxed)
}

pub fn pitch() -> u32 {
    PITCH.load(Ordering::Relaxed)
}

/// 帧缓冲的物理基址（0 = 没有图形）。
pub fn fb_phys() -> u64 {
    FB_PHYS.load(Ordering::Relaxed)
}

/// 把帧缓冲映射进内核地址空间，并清屏。
///
/// 返回 `false` 表示这次启动没有可用的线性帧缓冲（stage2 没设成 VBE），
/// 调用方应当退回 VGA 文本控制台——**这一点很重要**：图形不该是启动的
/// 硬依赖，没有图形时内核仍然要能跑完全部自检。
pub fn init() -> bool {
    // SAFETY: 0x9380 在低 1 MiB 内，直接映射必然存在；交接块由 stage2 写入。
    let base = paging::phys_to_virt(HANDOFF_PHYS) as *const u32;
    let (magic, w, h, p, bpp, addr) = unsafe {
        (
            core::ptr::read_volatile(base),
            core::ptr::read_volatile(base.add(1)),
            core::ptr::read_volatile(base.add(2)),
            core::ptr::read_volatile(base.add(3)),
            core::ptr::read_volatile(base.add(4)),
            core::ptr::read_volatile(base.add(5)),
        )
    };
    if magic != MAGIC || bpp != 32 || w == 0 || h == 0 || p < w * 4 {
        return false;
    }

    // 逐页映射。失败（比如页表项分配不出来）就当没有图形，不 panic。
    let bytes = (p as u64) * (h as u64);
    let pages = (bytes + 0xFFF) / 0x1000;
    for i in 0..pages {
        let va = FB_VA + i * 0x1000;
        let pa = (addr as u64) + i * 0x1000;
        if paging::map_in_current(va, pa, kvm::Flags::KERNEL_RW).is_err() {
            return false;
        }
    }

    W.store(w, Ordering::Relaxed);
    H.store(h, Ordering::Relaxed);
    PITCH.store(p, Ordering::Relaxed);
    FB_PHYS.store(addr as u64, Ordering::Relaxed);
    READY.store(true, Ordering::Release);

    clear(BG);
    ROW.store(0, Ordering::Relaxed);
    COL.store(0, Ordering::Relaxed);
    true
}

/// 屏宽能放下多少个字符格。
pub fn cols() -> u32 {
    width() / CELL_W
}

pub fn rows() -> u32 {
    height() / CELL_H
}

/* ---------------- 基本绘制 ---------------- */

#[inline]
fn pixel(x: u32, y: u32) -> *mut u32 {
    (FB_VA + (y * pitch() + x * 4) as u64) as *mut u32
}

#[inline]
pub fn put_pixel(x: u32, y: u32, color: u32) {
    if x >= width() || y >= height() {
        return;
    }
    // SAFETY: 上面已做边界检查；该地址落在已映射的帧缓冲窗口内。
    unsafe {
        core::ptr::write_volatile(pixel(x, y), color);
    }
}

pub fn fill_rect(x: u32, y: u32, w: u32, h: u32, color: u32) {
    let (fw, fh) = (width(), height());
    if x >= fw || y >= fh || w == 0 || h == 0 {
        return;
    }
    let x2 = (x + w).min(fw);
    let y2 = (y + h).min(fh);
    for yy in y..y2 {
        let row = pixel(x, yy);
        // SAFETY: 行内像素连续，且 [x, x2) 在屏内。
        unsafe {
            for i in 0..(x2 - x) as u64 {
                core::ptr::write_volatile(row.add(i as usize), color);
            }
        }
    }
}

pub fn clear(color: u32) {
    fill_rect(0, 0, width(), height(), color);
}

/// 画一个字模（8x16）。`fg` 只填有点的像素，背景保持不动——
/// 这样上层可以先画底色再叠字，也方便做"选中"效果。
pub fn draw_glyph(x: u32, y: u32, ch: u8, fg: u32) {
    let idx = ch.wrapping_sub(crate::font16::FIRST_CHAR) as usize;
    if idx >= crate::font16::FONT.len() {
        return;
    }
    let glyph = &crate::font16::FONT[idx];
    for (dy, bits) in glyph.iter().enumerate() {
        if *bits == 0 {
            continue;
        }
        let py = y + dy as u32;
        // 字模现在是 16 位宽的一行，最高位是最左像素
        for dx in 0..CELL_W {
            if bits & (1u16 << (CELL_W - 1 - dx)) != 0 {
                put_pixel(x + dx, py, fg);
            }
        }
    }
}

/// 在像素坐标处画一行文字（不做换行）。
pub fn draw_text(x: u32, y: u32, s: &str, fg: u32) {
    let mut cx = x;
    for b in s.bytes() {
        draw_glyph(cx, y, b, fg);
        cx += CELL_W;
    }
}

/* ---------------- 图形控制台（内核日志输出） ---------------- */

fn scroll_up() {
    let p = pitch();
    let h = height();
    let shift = CELL_H * p;
    let total = h * p;
    // SAFETY: 整块帧缓冲窗口都已映射；源与目标都在窗口内且重叠，
    // 这里按字节从前向后拷（目标地址更低），与前向 memcpy 语义一致。
    unsafe {
        let dst = FB_VA as *mut u8;
        let src = (FB_VA + shift as u64) as *const u8;
        core::ptr::copy(src, dst, (total - shift) as usize);
    }
    fill_rect(0, h - CELL_H, width(), CELL_H, BG);
}

fn newline() {
    COL.store(0, Ordering::Relaxed);
    let r = ROW.fetch_add(1, Ordering::Relaxed) + 1;
    if r >= rows() {
        scroll_up();
        ROW.store(rows() - 1, Ordering::Relaxed);
    }
}

/// 往图形控制台写一个字节。
pub fn console_put(b: u8) {
    if !ready() {
        return;
    }
    match b {
        b'\n' => newline(),
        b'\r' => COL.store(0, Ordering::Relaxed),
        _ => {
            let r = ROW.load(Ordering::Relaxed);
            let c = COL.load(Ordering::Relaxed);
            let x = c * CELL_W;
            let y = r * CELL_H;
            // 先把这一格涂底，再画字：否则滚动/覆盖时会残留旧像素
            fill_rect(x, y, CELL_W, CELL_H, BG);
            draw_glyph(x, y, b, FG);
            let c = c + 1;
            if c >= cols() {
                newline();
            } else {
                COL.store(c, Ordering::Relaxed);
            }
        }
    }
}

pub fn console_clear() {
    if !ready() {
        return;
    }
    clear(BG);
    ROW.store(0, Ordering::Relaxed);
    COL.store(0, Ordering::Relaxed);
}
