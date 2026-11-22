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
pub const SYS_FORK: u64 = 57;
pub const SYS_EXECVE: u64 = 59;
pub const SYS_GETPID: u64 = 39;
pub const SYS_EXIT: u64 = 60;
/// LumiOS 扩展（不属于 Linux 号段）
pub const SYS_GETDENTS64: u64 = 217;
pub const SYS_UNLINK: u64 = 87;
pub const SYS_GETCWD: u64 = 79;
pub const SYS_CHDIR: u64 = 80;
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

pub const O_RDONLY: u64 = 0;
pub const O_WRONLY: u64 = 1;
pub const O_CREAT: u64 = 0o100;
pub const O_TRUNC: u64 = 0o1000;

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

/// fork：父进程返回子进程 tid，子进程返回 0，失败返回负 errno。
pub fn fork() -> i64 {
    call(SYS_FORK, 0, 0, 0)
}

/// execve：路径要以 NUL 结尾。成功不返回。
pub fn execve(path: &str, argv: &[&str]) -> i64 {
    let mut pb = [0u8; 128];
    let n = path.len().min(127);
    pb[..n].copy_from_slice(&path.as_bytes()[..n]);
    // 参数最多 8 个，全部指向栈上的 NUL 结尾缓冲
    let mut store = [[0u8; 64]; 8];
    let mut ptrs = [0u64; 10];
    let cnt = argv.len().min(8);
    for i in 0..cnt {
        let a = argv[i].as_bytes();
        let l = a.len().min(63);
        store[i][..l].copy_from_slice(&a[..l]);
        ptrs[i] = store[i].as_ptr() as u64;
    }
    ptrs[cnt] = 0;
    call(SYS_EXECVE, pb.as_ptr() as u64, ptrs.as_ptr() as u64, 0)
}

/// 一次 readdir 拿到的目录项数量上限（够列一层目录）。
pub const DIR_MAX: usize = 64;

/// 目录项：名字 + 类型。
#[derive(Clone, Copy)]
pub struct DirEnt {
    pub name: [u8; 32],
    pub len: u8,
    pub is_dir: bool,
}

impl DirEnt {
    pub fn as_str(&self) -> &str {
        // SAFETY: 名字来自内核返回的字节串，这里只按 ASCII 打印，非法字节会被跳过。
        core::str::from_utf8(&self.name[..self.len as usize]).unwrap_or("?")
    }
}

/// 读一个目录，返回条目数。`path` 传 "." 表示当前目录。
///
/// 解析的是 Linux `struct linux_dirent64`：u64 ino / i64 off / u16 reclen /
/// u8 type / name...（name 以 NUL 结尾，reclen 按 8 字节对齐，所以**必须
/// 按 reclen 步进**，不能按名字长度推进——这是最容易写错的地方）。
pub fn readdir(path: &str, out: &mut [DirEnt; DIR_MAX]) -> i64 {
    let fd = open(path, 0);
    if fd < 0 {
        return -1;
    }
    let mut buf = [0u8; 2048];
    let n = call(SYS_GETDENTS64, fd as u64, buf.as_mut_ptr() as u64, buf.len() as u64);
    close(fd);
    if n <= 0 {
        return 0;
    }
    let mut off = 0usize;
    let mut cnt = 0usize;
    while off + 19 <= n as usize && cnt < DIR_MAX {
        let reclen = u16::from_le_bytes([buf[off + 16], buf[off + 17]]) as usize;
        if reclen < 20 || off + reclen > n as usize {
            break;
        }
        let dtype = buf[off + 18];
        let name_start = off + 19;
        let mut l = 0usize;
        while name_start + l < off + reclen && buf[name_start + l] != 0 {
            l += 1;
        }
        let mut e = DirEnt {
            name: [0; 32],
            len: 0,
            is_dir: dtype == 4,
        };
        let l = l.min(31);
        e.name[..l].copy_from_slice(&buf[name_start..name_start + l]);
        e.len = l as u8;
        out[cnt] = e;
        cnt += 1;
        off += reclen;
    }
    cnt as i64
}

pub fn unlink(path: &str) -> i64 {
    let mut pb = [0u8; 128];
    let n = path.len().min(127);
    pb[..n].copy_from_slice(&path.as_bytes()[..n]);
    call(SYS_UNLINK, pb.as_ptr() as u64, 0, 0)
}

pub fn getcwd(buf: &mut [u8]) -> i64 {
    call(SYS_GETCWD, buf.as_mut_ptr() as u64, buf.len() as u64, 0)
}

pub fn chdir(path: &str) -> i64 {
    let mut pb = [0u8; 128];
    let n = path.len().min(127);
    pb[..n].copy_from_slice(&path.as_bytes()[..n]);
    call(SYS_CHDIR, pb.as_ptr() as u64, 0, 0)
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

    /// 填充矩形。**所有参数先钳到画布内**——负坐标或超出边界的矩形
    /// 以前会退化成"逐点调用 put"，而 put 每次都要判一次边界，
    /// 一旦调用方算了负的宽高就会白跑几十万次循环（甚至写飞）。
    /// 统一在入口钳一次，比在每个调用点写检查可靠。
    pub fn fill(&self, x: i32, y: i32, w: i32, h: i32, color: u32) {
        if w <= 0 || h <= 0 {
            return;
        }
        let x0 = x.max(0).min(self.width as i32);
        let y0 = y.max(0).min(self.height as i32);
        let x1 = (x + w).max(0).min(self.width as i32);
        let y1 = (y + h).max(0).min(self.height as i32);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        // **按行整块写**，不再逐像素调用 put。
        // put 每点都要判一次边界、做一次 volatile 写；整屏 78 万点就是 78 万次判断。
        // 这里每行只算一次指针、一次长度，然后交给 slice::fill（会被优化成宽写）。
        let n = (x1 - x0) as usize;
        for yy in y0..y1 {
            let off = (yy as u32 * self.pitch / 4 + x0 as u32) as isize;
            // SAFETY: [x0,x1) 与 [y0,y1) 已钳在画布内，整行连续可写。
            unsafe {
                let p = self.ptr.offset(off) as *mut u32;
                core::slice::from_raw_parts_mut(p, n).fill(color);
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
            for dx in 0..font::CELL_W {
                if bits & (1u16 << (font::CELL_W - 1 - dx)) != 0 {
                    self.put(x + dx as i32, y + dy as i32, color);
                }
            }
        }
    }

    pub fn text(&self, x: i32, y: i32, s: &str, color: u32) {
        let mut cx = x;
        for b in s.bytes() {
            self.glyph(cx, y, b, color);
            cx += font::CELL_W as i32;
        }
    }

    /// 带底色的文字（先抹掉整行高度，避免叠加残影）。
    pub fn text_bg(&self, x: i32, y: i32, s: &str, fg: u32, bg: u32) {
        // 宽度按"画布剩余宽度"截断：长字符串不该把背景矩形推到界外
        let nb = s.len() as i32;
        let max_chars = ((self.width as i32 - x) / font::CELL_W as i32).max(0);
        let w = nb.min(max_chars).max(0) * font::CELL_W as i32;
        if w > 0 {
            self.fill(x, y, w, font::CELL_H as i32, bg);
        }
        self.text(x, y, s, fg);
    }

    /// 把另一块画布的内容贴到 (dx, dy)。窗口服务合成时用。
    /// 整块贴图。按行 `copy_nonoverlapping`，比逐像素快一个数量级。
    pub fn blit(&self, src: &Canvas, dx: i32, dy: i32) {
        // 目标矩形先钳一次，避免每行重复判断
        let x0 = dx.max(0);
        let y0 = dy.max(0);
        let x1 = (dx + src.width as i32).min(self.width as i32);
        let y1 = (dy + src.height as i32).min(self.height as i32);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let n = (x1 - x0) as usize;
        for yy in y0..y1 {
            let sx = (x0 - dx) as usize;
            let sy = (yy - dy) as usize;
            let soff = (sy as u32 * src.pitch / 4) as isize + sx as isize;
            let doff = (yy as u32 * self.pitch / 4) as isize + x0 as isize;
            // SAFETY: 源偏移在源画布内（sx/sy 由钳制推出），目标同理。
            unsafe {
                let sp = src.ptr.offset(soff);
                let dp = self.ptr.offset(doff) as *mut u32;
                core::ptr::copy_nonoverlapping(sp, dp, n);
            }
        }
    }
}

/// 与内核共用同一份字库文件——两边字形必须一致，否则控制台和 GUI 的
/// 字看起来是两种风格。
#[path = "../../lumikernel/src/font16.rs"]
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
