//! liblumi —— LumiOS 用户态基础库。
//!
//! 目标很具体：**四个图形应用所需要的一切，不多给一行**。
//!
//! - 系统调用封装（Linux 子集 + LumiOS 扩展）
//! - 帧缓冲画布（窗口服务直接拿到全屏；普通应用只拿到自己的共享缓冲）
//! - 8x16 字模（与内核共用同一份字库文件，保证两边字形一致）
//! - 键盘事件解码
//! - 一个极简 bump 分配器（够 `Vec`/`String` 用，不做 free）
//!
//! 这里刻意不引入任何第三方 crate：整个用户态只有我们自己写的代码，
//! 出了问题能一路读到底。

#![no_std]

extern crate alloc;

use core::alloc::{GlobalAlloc, Layout};

/* ============================ 系统调用 ============================ */

pub const SYS_READ: u64 = 0;
pub const SYS_WRITE: u64 = 1;
pub const SYS_OPEN: u64 = 2;
pub const SYS_CLOSE: u64 = 3;
pub const SYS_BRK: u64 = 12;
pub const SYS_GETPID: u64 = 39;
pub const SYS_EXIT: u64 = 60;
/// LumiOS 扩展（不属于 Linux 号段）
pub const SYS_LUMI: u64 = 0x400;

#[inline(always)]
pub unsafe fn syscall4(n: u64, a: u64, b: u64, c: u64) -> i64 {
    let ret: i64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") n as i64 => ret,
        in("rdi") a,
        in("rsi") b,
        in("rdx") c,
        lateout("rcx") _,
        lateout("r11") _,
        options(nostack)
    );
    ret
}

#[inline(always)]
fn call(n: u64, a: u64, b: u64, c: u64) -> i64 {
    // SAFETY: 系统调用号与参数由本库内部给出，调用方不传裸指针给内核以外的地方。
    unsafe { syscall4(n, a, b, c) }
}

pub fn write(fd: i64, buf: &[u8]) -> i64 {
    call(SYS_WRITE, fd as u64, buf.as_ptr() as u64, buf.len() as u64)
}

pub fn read(fd: i64, buf: &mut [u8]) -> i64 {
    call(SYS_READ, fd as u64, buf.as_mut_ptr() as u64, buf.len() as u64)
}

pub fn open(path: &str, flags: u64) -> i64 {
    // 内核的 open 期望以 NUL 结尾的路径
    let mut buf = [0u8; 128];
    let n = path.len().min(127);
    buf[..n].copy_from_slice(&path.as_bytes()[..n]);
    call(SYS_OPEN, buf.as_ptr() as u64, flags, 0)
}

pub fn close(fd: i64) -> i64 {
    call(SYS_CLOSE, fd as u64, 0, 0)
}

pub fn getpid() -> i64 {
    call(SYS_GETPID, 0, 0, 0)
}

pub fn exit(code: i64) -> ! {
    call(SYS_EXIT, code as u64, 0, 0);
    // exit 不该返回；真返回了就死循环，别让 CPU 跑去执行垃圾
    loop {
        core::hint::spin_loop();
    }
}

/// 打印到内核控制台（调试用；GUI 应用应该画到画布上）。
pub fn println(s: &str) {
    write(1, s.as_bytes());
    write(1, b"\n");
}

/* ======================== LumiOS 图形扩展 ======================== */

const OP_FB_INFO: u64 = 1;
const OP_FB_MAP: u64 = 2;
const OP_INPUT_READ: u64 = 3;
const OP_SHM_ALLOC: u64 = 4;
const OP_SHM_MAP: u64 = 5;
const OP_SHM_FREE: u64 = 6;
const OP_MOUSE_READ: u64 = 7;

#[derive(Clone, Copy)]
pub struct FbInfo {
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub bpp: u32,
}

pub fn fb_info() -> Option<FbInfo> {
    let mut v = [0u32; 4];
    let r = lumi(OP_FB_INFO, v.as_mut_ptr() as u64, 0);
    if r < 0 {
        return None;
    }
    Some(FbInfo {
        width: v[0],
        height: v[1],
        pitch: v[2],
        bpp: v[3],
    })
}

/// 把全屏帧缓冲映射进来，返回画布。**只有窗口服务该调用它**。
pub fn fb_map(tag: u32) -> Option<u64> {
    let r = lumi(OP_FB_MAP, tag as u64, 0);
    if r < 0 {
        None
    } else {
        Some(r as u64)
    }
}

pub fn shm_alloc(pages: usize) -> i64 {
    lumi(OP_SHM_ALLOC, pages as u64, 0)
}

pub fn shm_map(id: i64) -> Option<u64> {
    let r = lumi(OP_SHM_MAP, id as u64, 0);
    if r < 0 {
        None
    } else {
        Some(r as u64)
    }
}

pub fn shm_free(id: i64) -> i64 {
    lumi(OP_SHM_FREE, id as u64, 0)
}

/// 取一次鼠标增量 `(dx, dy, buttons)`；没有变化返回 `None`。
pub fn mouse_read() -> Option<(i32, i32, u8)> {
    let r = lumi(OP_MOUSE_READ, 0, 0);
    if r == 0 {
        return None;
    }
    let v = r as u32;
    let dx = (v & 0xFF) as u8 as i8 as i32;
    let dy = ((v >> 8) & 0xFF) as u8 as i8 as i32;
    let btn = ((v >> 16) & 0xFF) as u8;
    Some((dx, dy, btn))
}

/// 取一个键盘事件；没有事件返回 `None`。
pub fn input_read() -> Option<KeyEvent> {
    let r = lumi(OP_INPUT_READ, 0, 0);
    if r <= 0 {
        None
    } else {
        Some(KeyEvent::decode(r as u32))
    }
}

/// LumiOS 扩展统一入口：`n = SYS_LUMI`，**第一个参数是子操作号**。
/// 之前把子操作塞进系统调用号的高位是错的——内核只看 rax，看到的是一个
/// 天文数字，直接 EINVAL。
#[inline(always)]
fn lumi(op: u64, a: u64, b: u64) -> i64 {
    call(SYS_LUMI, op, a, b)
}

/* ============================ 键盘事件 ============================ */

pub const MOD_SHIFT: u8 = 1;
pub const MOD_CTRL: u8 = 2;
pub const MOD_ALT: u8 = 4;

pub const KEY_ESC: u8 = 0x80;
pub const KEY_ENTER: u8 = 0x81;
pub const KEY_BACKSPACE: u8 = 0x82;
pub const KEY_TAB: u8 = 0x83;
pub const KEY_UP: u8 = 0x84;
pub const KEY_DOWN: u8 = 0x85;
pub const KEY_LEFT: u8 = 0x86;
pub const KEY_RIGHT: u8 = 0x87;
pub const KEY_HOME: u8 = 0x88;
pub const KEY_END: u8 = 0x89;
pub const KEY_PGUP: u8 = 0x8A;
pub const KEY_PGDN: u8 = 0x8B;
pub const KEY_DELETE: u8 = 0x8C;
pub const KEY_INSERT: u8 = 0x8D;
pub const KEY_F1: u8 = 0x8E;

#[derive(Clone, Copy)]
pub struct KeyEvent {
    pub code: u8,
    pub pressed: bool,
    pub mods: u8,
}

impl KeyEvent {
    pub fn decode(raw: u32) -> Self {
        KeyEvent {
            code: (raw & 0xFF) as u8,
            pressed: (raw >> 8) & 0xFF != 0,
            mods: ((raw >> 16) & 0xFF) as u8,
        }
    }
    pub fn is_alt(&self) -> bool {
        self.mods & MOD_ALT != 0
    }
    pub fn is_ctrl(&self) -> bool {
        self.mods & MOD_CTRL != 0
    }
    pub fn is_shift(&self) -> bool {
        self.mods & MOD_SHIFT != 0
    }
}

/* ============================== 画布 ============================== */

/// 一块可写像素区域（全屏帧缓冲，或某个窗口的共享缓冲）。
#[derive(Clone, Copy)]
pub struct Canvas {
    pub ptr: *mut u32,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
}

impl Canvas {
    /// 从帧缓冲物理窗口构造（窗口服务用）。
    pub fn screen() -> Option<Canvas> {
        let info = fb_info()?;
        let va = fb_map(0)?;
        Some(Canvas {
            ptr: va as *mut u32,
            width: info.width,
            height: info.height,
            pitch: info.pitch,
        })
    }

    /// 从共享内存构造（普通应用用）。
    pub fn from_shm(va: u64, width: u32, height: u32) -> Canvas {
        Canvas {
            ptr: va as *mut u32,
            width,
            height,
            pitch: width * 4,
        }
    }

    #[inline]
    pub fn put(&self, x: i32, y: i32, color: u32) {
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return;
        }
        let off = (y as u32 * self.pitch / 4 + x as u32) as isize;
        // SAFETY: 上面已做边界检查，偏移必落在本画布的音高范围内。
        unsafe {
            core::ptr::write_volatile(self.ptr.offset(off), color);
        }
    }

    pub fn fill(&self, x: i32, y: i32, w: i32, h: i32, color: u32) {
        if w <= 0 || h <= 0 {
            return;
        }
        for yy in y..(y + h) {
            for xx in x..(x + w) {
                self.put(xx, yy, color);
            }
        }
    }

    pub fn clear(&self, color: u32) {
        self.fill(0, 0, self.width as i32, self.height as i32, color);
    }

    pub fn glyph(&self, x: i32, y: i32, ch: u8, color: u32) {
        let idx = ch.wrapping_sub(font::FIRST_CHAR) as usize;
        if idx >= font::FONT.len() {
            return;
        }
        let g = &font::FONT[idx];
        for (dy, bits) in g.iter().enumerate() {
            if *bits == 0 {
                continue;
            }
            for dx in 0..8u32 {
                if bits & (0x80 >> dx) != 0 {
                    self.put(x + dx as i32, y + dy as i32, color);
                }
            }
        }
    }

    pub fn text(&self, x: i32, y: i32, s: &str, color: u32) {
        let mut cx = x;
        for b in s.bytes() {
            self.glyph(cx, y, b, color);
            cx += 8;
        }
    }

    /// 带底色的文字（先抹掉整行高度，避免叠加残影）。
    pub fn text_bg(&self, x: i32, y: i32, s: &str, fg: u32, bg: u32) {
        self.fill(x, y, (s.len() as i32) * 8, 16, bg);
        self.text(x, y, s, fg);
    }

    /// 把另一块画布的内容贴到 (dx, dy)。窗口服务合成时用。
    pub fn blit(&self, src: &Canvas, dx: i32, dy: i32) {
        for y in 0..src.height as i32 {
            for x in 0..src.width as i32 {
                let off = (y as u32 * src.pitch / 4 + x as u32) as isize;
                // SAFETY: 源画布内部偏移在界内。
                let c = unsafe { core::ptr::read_volatile(src.ptr.offset(off)) };
                self.put(dx + x, dy + y, c);
            }
        }
    }
}

/// 与内核共用同一份字库文件——两边字形必须一致，否则控制台和 GUI 的
/// 字看起来是两种风格。
#[path = "../../lumikernel/src/font8x16.rs"]
pub mod font;

/* ============================= 分配器 ============================= */

/// 极简 bump 分配器：只分配不释放。
///
/// 为什么够用：四个应用都是"启动时分配、之后长时间运行"的形态，
/// 没有频繁的短生命周期对象。真需要回收时再换，不要一上来就写通用分配器。
struct Bump;

const ARENA_SIZE: usize = 512 * 1024;
static mut ARENA: [u8; ARENA_SIZE] = [0; ARENA_SIZE];
static mut ARENA_OFF: usize = 0;

unsafe impl GlobalAlloc for Bump {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let align = layout.align().max(16);
        let start = (ARENA_OFF + align - 1) & !(align - 1);
        if start + layout.size() > ARENA_SIZE {
            return core::ptr::null_mut();
        }
        let p = ARENA.as_mut_ptr().add(start);
        ARENA_OFF = start + layout.size();
        p
    }
    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}

#[global_allocator]
static ALLOC: Bump = Bump;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    write(2, b"[lumi] panic\n");
    exit(101)
}
