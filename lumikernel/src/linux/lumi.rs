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
/// 在 busybox 里执行一条命令（输出重定向到 /data/.out 由 sh 自己负责）
const OP_RUN: u64 = 8;

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

/// 输入事件的所有者（tid）。**第一个映射帧缓冲的进程**就是窗口服务，
/// 键盘事件只发给它。
///
/// 为什么必须这么做：输入队列是全局的，`input_read` 谁调谁拿。
/// 客户端进程也去轮询的话，按键会被它吃掉，窗口服务收不到——
/// 表现就是"按键完全没反应"，而内核日志里事件明明被取走了。
static INPUT_OWNER: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn err(e: i64) -> i64 {
    -e
}

/// 把字节块写到用户地址（用 uaccess，避免用户传坏指针把内核带崩）。
/// 从用户地址读一段数据（读用户内存，逐页翻译）。
fn read_user(ptr: u64, out: &mut [u8]) -> Result<(), i64> {
    let mut off = 0usize;
    while off < out.len() {
        let va = ptr + off as u64;
        let page = va & !0xFFF;
        let phys = match crate::paging::translate_in(crate::task::current_pml4(), page) {
            Some(p) => p,
            None => return Err(err(klinux::errno::EFAULT)),
        };
        let in_page = (va - page) as usize;
        let n = core::cmp::min(4096 - in_page, out.len() - off);
        // SAFETY: 物理页经直接映射区可访问，范围在单页内
        unsafe {
            core::ptr::copy_nonoverlapping(
                (crate::paging::phys_to_virt(phys) as *const u8).add(in_page),
                out.as_mut_ptr().add(off),
                n,
            );
        }
        off += n;
    }
    Ok(())
}

/// 用 busybox 的 sh 执行一条命令。
///
/// 为什么不 fork：窗口服务进程里 fork 会让它崩（COW 与共享映射的冲突，已实测）。
/// 这里直接由内核 spawn 一个子进程，父进程完全不受影响。
fn run_cmd(ptr: u64, len: u64) -> i64 {
    if len == 0 || len > 900 {
        return err(klinux::errno::EINVAL);
    }
    let mut buf = [0u8; 1024];
    let n = len as usize;
    if read_user(ptr, &mut buf[..n]).is_err() {
        return err(klinux::errno::EFAULT);
    }
    let argv: alloc::vec::Vec<alloc::vec::Vec<u8>> = alloc::vec![
        b"busybox".to_vec(),
        b"sh".to_vec(),
        b"-c".to_vec(),
        buf[..n].to_vec(),
    ];
    let envp: alloc::vec::Vec<alloc::vec::Vec<u8>> = alloc::vec![b"PATH=/bin".to_vec()];
    // **静音控制台**：子进程的 stdout 会重定向到 /data/.out，
    // 但如果它往 stderr 或直接往控制台写，就会盖住整个桌面（实际发生过）。
    crate::console::set_mute(true);
    let r = match crate::linux::proc::spawn_program(b"/bin/busybox", &argv, &envp) {
        Ok(_) => 0,
        Err(e) => e,
    };
    crate::console::set_mute(false);
    r
}

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
        OP_RUN => run_cmd(a1, a2),
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
    // 谁映射帧缓冲，谁就是窗口服务 —— 由它独占输入
    INPUT_OWNER.store(crate::task::current_tid(), Ordering::Release);
    FB_VA_USER as i64
}

/// 取一个输入事件；没有事件返回 0（键码 0 永远不会被产生，可以安全当"空"用）。
fn input_read() -> i64 {
    // 只把事件发给窗口服务；其他进程一律拿不到（返回"没有事件"）。
    // 这样客户端即使误调也抢不走键盘。
    let owner = INPUT_OWNER.load(Ordering::Acquire);
    if owner != 0 && owner != crate::task::current_tid() {
        return 0;
    }
    match crate::dev::ps2::poll_event() {
        Some(ev) => {
            // 诊断：确认"队里真的有事件"以及"被哪个进程取走了"
            kserialln!("[input] pop ev=0x{:X}", ev);
            ev as i64
        }
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
