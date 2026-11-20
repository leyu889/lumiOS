//! LumiOS 扩展系统调用：图形、输入、共享内存。
//!
//! ## 为什么单独开一个 syscall 号，而不是挤进 Linux 号段
//!
//! Linux 的系统调用号是**别人定的**，挤一个进去迟早和真实 ABI 撞车
//! （musl 里某个新号一出现，我们的语义就变了）。LumiOS 自己的东西
//! 用一个明确不属于 Linux 的号（`0x400`），子操作放在第一个参数里。
//!
//! 应用侧不需要记这些数字——`liblumi` 会把它们包成 `fb_open()` /
//! `input_read()` / `shm_alloc()` 这样的函数。
//!
//! ## 地址布局
//!
//! ```text
//! 0x0000_0000_0040_0000  ELF 装载
//! ...                    brk
//! 0x0000_0000_7000_0000  帧缓冲（固定，3 MiB）
//! 0x0000_0000_7100_0000  共享内存区（每个 region 8 MiB 步进）
//! 0x0000_0001_0000_0000  mmap 向下生长的起点
//! ```
//!
//! 这三段之间留了足够空隙，不会和匿名 mmap 撞上（mmap 从 4 GiB 往下长）。

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch::sync::IrqSpinlock;
use crate::mm::{frames, paging};

/// LumiOS 扩展系统调用号（不属于 Linux 号段）。
pub const SYS_LUMI: u64 = 0x400;

const OP_FB_INFO: u64 = 1;
const OP_FB_MAP: u64 = 2;
const OP_INPUT_READ: u64 = 3;
const OP_SHM_ALLOC: u64 = 4;
const OP_SHM_MAP: u64 = 5;
const OP_SHM_FREE: u64 = 6;
const OP_MOUSE_READ: u64 = 7;

/// 帧缓冲在用户态看到的固定地址。
pub const FB_VA_USER: u64 = 0x7000_0000;
// 放在用户栈（0x8000_0000 往下 8 页）**下面**：
// 之前写 0x8000_0000 正好压在栈顶上，一映射就把栈覆盖了。
const SHM_BASE: u64 = 0x7100_0000;
const SHM_STRIDE: u64 = 0x0080_0000; // 8 MiB/region
const SHM_MAX: usize = 16;

struct Region {
    used: bool,
    pages: usize,
    /// 每页的物理地址（共享内存不要求物理连续：逐页映射即可，
    /// 只有 virtio 那种"设备要连续物理内存"的场合才需要连续分配）。
    phys: Vec<u64>,
}

static REGIONS: IrqSpinlock<[Region; SHM_MAX]> = IrqSpinlock::new(
    [const {
        Region {
            used: false,
            pages: 0,
            phys: Vec::new(),
        }
    }; SHM_MAX],
);

static FB_MAPPED: AtomicBool = AtomicBool::new(false);

fn err(e: i64) -> i64 {
    -e
}

/// 把字节块写到用户地址（用 uaccess，避免用户传坏指针把内核带崩）。
fn write_user(ptr: u64, data: &[u8]) -> Result<(), i64> {
    crate::linux::uaccess::copy_out(ptr, data).map_err(|_| klinux::errno::EFAULT)
}

pub fn dispatch(op: u64, a1: u64, a2: u64, _a3: u64) -> i64 {
    match op {
        OP_FB_INFO => fb_info(a1),
        OP_FB_MAP => fb_map(),
        OP_INPUT_READ => input_read(),
        OP_SHM_ALLOC => shm_alloc(a1 as usize),
        OP_SHM_MAP => shm_map(a1 as usize),
        OP_SHM_FREE => shm_free(a1 as usize),
        OP_MOUSE_READ => mouse_read(),
        _ => err(klinux::errno::EINVAL),
    }
}

/// 取帧缓冲参数：往用户结构里写 `{width, height, pitch, bpp}`（4 个 u32）。
fn fb_info(ptr: u64) -> i64 {
    if !crate::video::ready() {
        return err(klinux::errno::ENODEV);
    }
    let mut buf = [0u8; 16];
    buf[0..4].copy_from_slice(&crate::video::width().to_le_bytes());
    buf[4..8].copy_from_slice(&crate::video::height().to_le_bytes());
    buf[8..12].copy_from_slice(&crate::video::pitch().to_le_bytes());
    buf[12..16].copy_from_slice(&32u32.to_le_bytes());
    match write_user(ptr, &buf) {
        Ok(()) => 0,
        Err(e) => e,
    }
}

/// 把帧缓冲映射进当前进程，返回用户态地址。
///
/// 窗口服务是整个系统里唯一被允许直接写屏幕的进程——别的进程只能往
/// 自己的共享缓冲里画，再由窗口服务合成。这是最小可信计算基的边界。
fn fb_map() -> i64 {
    if !crate::video::ready() {
        return err(klinux::errno::ENODEV);
    }
    let pages = ((crate::video::pitch() as u64) * (crate::video::height() as u64) + 0xFFF) / 0x1000;
    let phys_base = crate::video::fb_phys();
    if phys_base == 0 {
        return err(klinux::errno::ENODEV);
    }
    for i in 0..pages {
        let va = FB_VA_USER + i * 0x1000;
        if paging::map_in_current(va, phys_base + i * 0x1000, kvm::Flags::USER_RW).is_err() {
            return err(klinux::errno::ENOMEM);
        }
    }
    FB_MAPPED.store(true, Ordering::Release);
    FB_VA_USER as i64
}

/// 取一个输入事件；没有事件返回 0（键码 0 永远不会被产生，可以安全当"空"用）。
fn input_read() -> i64 {
    match crate::dev::ps2::poll_event() {
        Some(ev) => ev as i64,
        None => 0,
    }
}

/// 取一次鼠标增量；没有变化返回 0。
///
/// 打包成 `dx<<0 | dy<<8 | buttons<<16`（各 8 位，有符号按补码截断）。
/// 一次轮询的位移不会超过 ±127，所以 8 位够用；真超了说明鼠标在飞，
/// 丢掉那一段比让光标跳到屏幕外更好。
fn mouse_read() -> i64 {
    match crate::dev::ps2::poll_mouse() {
        Some((dx, dy, btn)) => {
            let x = dx.clamp(-127, 127) as i8 as u8 as u32;
            let y = dy.clamp(-127, 127) as i8 as u8 as u32;
            (x | (y << 8) | ((btn as u32) << 16)) as i64
        }
        None => 0,
    }
}

/// 分配共享内存：`npages` 页，返回 region id（>=1）。
fn shm_alloc(npages: usize) -> i64 {
    if npages == 0 || npages > 4096 {
        return err(klinux::errno::EINVAL);
    }
    let mut g = REGIONS.lock();
    let idx = match (0..SHM_MAX).find(|i| !g[*i].used) {
        Some(i) => i,
        None => return err(klinux::errno::ENOMEM),
    };
    let mut phys = Vec::with_capacity(npages);
    for _ in 0..npages {
        match frames::alloc_frame() {
            Some(p) => {
                // 清零：客户端的窗口缓冲不该看到上一个持有者的像素
                // SAFETY: 刚分配的独占页，位于直接映射区。
                unsafe {
                    core::ptr::write_bytes(paging::phys_to_virt(p) as *mut u8, 0, 4096);
                }
                phys.push(p);
            }
            None => {
                // 分配失败：把已经拿到的还回去，别泄漏
                for p in phys.iter() {
                    frames::free_frame(*p);
                }
                return err(klinux::errno::ENOMEM);
            }
        }
    }
    g[idx] = Region {
        used: true,
        pages: npages,
        phys,
    };
    (idx + 1) as i64
}

/// 把共享内存映射进当前进程，返回用户态地址。
fn shm_map(id: usize) -> i64 {
    if id == 0 || id > SHM_MAX {
        return err(klinux::errno::EINVAL);
    }
    let g = REGIONS.lock();
    let r = &g[id - 1];
    if !r.used {
        return err(klinux::errno::EINVAL);
    }
    let base = SHM_BASE + (id as u64 - 1) * SHM_STRIDE;
    for (i, p) in r.phys.iter().enumerate() {
        let va = base + (i as u64) * 0x1000;
        if paging::map_in_current(va, *p, kvm::Flags::USER_RW).is_err() {
            return err(klinux::errno::ENOMEM);
        }
    }
    base as i64
}

/// 释放共享内存。映射到各进程的页表项暂不回收（进程退出时随页表一起消失），
/// 物理页还给分配器。
fn shm_free(id: usize) -> i64 {
    if id == 0 || id > SHM_MAX {
        return err(klinux::errno::EINVAL);
    }
    let mut g = REGIONS.lock();
    let r = &mut g[id - 1];
    if !r.used {
        return err(klinux::errno::EINVAL);
    }
    for p in r.phys.iter() {
        frames::free_frame(*p);
    }
    r.phys = Vec::new();
    r.pages = 0;
    r.used = false;
    0
}
