//! PS/2 键盘驱动（i8042 控制器）。
//!
//! ## 为什么是轮询，不是 IRQ1
//!
//! 本内核走 APIC，8259A 已被整体屏蔽。要让 IRQ1 真正变成中断，得在 IOAPIC
//! 里配一条重定向表项（GSI → 向量 → 目标 CPU）。那是可行的，但代价是：
//! 一条配错的中断线会让整个键盘"看起来没插"，现场极难区分"没按"和"中断丢了"。
//!
//! 轮询的代价是延迟：定时器 100 Hz，最坏 10 ms。人打字最快约 30 字/秒
//! （33 ms 一个键），i8042 的输出缓冲不会被写爆。等窗口服务稳定之后
//! 再换成 IOAPIC 中断，那时已经有可靠的现场可对比。
//!
//! ## 事件格式
//!
//! 一次按键压成 `u32`，方便直接过系统调用：
//!
//! ```text
//! bits  0..7   键码（ASCII 可打印字符，或下面 KEY_* 里的特殊键）
//! bits  8..15  1 = 按下，0 = 抬起
//! bits 16..23  修饰键位图：1=Shift 2=Ctrl 4=Alt
//! ```

use crate::arch::sync::IrqSpinlock;

const PS2_DATA: u16 = 0x60;
const PS2_STATUS: u16 = 0x64;
const PS2_CMD: u16 = 0x64;

/* ---- 特殊键码（>= 0x80 的都不可能是 ASCII） ---- */
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
pub const KEY_F1: u8 = 0x8E; // F1..F12 连续，F1 + n

/* ---- 修饰位 ---- */
pub const MOD_SHIFT: u8 = 1;
pub const MOD_CTRL: u8 = 2;
pub const MOD_ALT: u8 = 4;

const QUEUE: usize = 64;

struct State {
    head: usize,
    tail: usize,
    buf: [u32; QUEUE],
    /// 当前修饰键状态
    mods: u8,
    /// 扩展码前缀（0xE0）是否已经收到
    ext: bool,
    /// 上一次的扫描码是不是 0xE0 后的特殊键（用于处理 E0 XX 的抬起）
    ext_code: u8,
    /* ---- 鼠标（aux 口）---- */
    /// 3 字节包的第几个
    mouse_idx: u8,
    mouse_pkt: [u8; 3],
    mouse_dx: i32,
    mouse_dy: i32,
    mouse_btn: u8,
    /// 自上次读取以来是否发生过移动/按键
    mouse_dirty: bool,
}

static ST: IrqSpinlock<State> = IrqSpinlock::new(State {
    head: 0,
    tail: 0,
    buf: [0; QUEUE],
    mods: 0,
    ext: false,
    ext_code: 0,
    mouse_idx: 0,
    mouse_pkt: [0; 3],
    mouse_dx: 0,
    mouse_dy: 0,
    mouse_btn: 0,
    mouse_dirty: false,
});

#[inline]
unsafe fn inb(port: u16) -> u8 {
    let v: u8;
    core::arch::asm!("in al, dx", out("al") v, in("dx") port, options(nomem, nostack, preserves_flags));
    v
}

#[inline]
unsafe fn outb(port: u16, v: u8) {
    core::arch::asm!("out dx, al", in("dx") port, in("al") v, options(nomem, nostack, preserves_flags));
}

/// 初始化 i8042：开键盘端口、清输出缓冲、使能扫描。
pub fn init() {
    // SAFETY: 0x60/0x64 是标准 i8042 端口。
    unsafe {
        // 清空可能残留的输出
        for _ in 0..16 {
            if inb(PS2_STATUS) & 1 == 0 {
                break;
            }
            let _ = inb(PS2_DATA);
        }
        // 使能键盘（0xAE = enable keyboard interface）
        while inb(PS2_STATUS) & 2 != 0 {
            core::hint::spin_loop();
        }
        outb(PS2_CMD, 0xAE);
        // 使能扫描（0xF4 = enable scanning）
        while inb(PS2_STATUS) & 2 != 0 {
            core::hint::spin_loop();
        }
        outb(PS2_DATA, 0xF4);

        // ---- 鼠标：打开辅助端口并让它开始上报 ----
        // 0xA8 = enable auxiliary device
        while inb(PS2_STATUS) & 2 != 0 {
            core::hint::spin_loop();
        }
        outb(PS2_CMD, 0xA8);
        // 读配置字节，清掉"禁用 aux 时钟"那一位
        while inb(PS2_STATUS) & 2 != 0 {
            core::hint::spin_loop();
        }
        outb(PS2_CMD, 0x20);
        while inb(PS2_STATUS) & 1 == 0 {
            core::hint::spin_loop();
        }
        let mut cfg = inb(PS2_DATA);
        cfg &= !0x20; // bit5=1 表示 aux 时钟被禁用
        cfg |= 0x02;  // 打开 aux 中断位（我们走轮询，置上无害）
        while inb(PS2_STATUS) & 2 != 0 {
            core::hint::spin_loop();
        }
        outb(PS2_CMD, 0x60);
        while inb(PS2_STATUS) & 2 != 0 {
            core::hint::spin_loop();
        }
        outb(PS2_DATA, cfg);
        // 往 aux 口写 0xF4 = enable data reporting
        while inb(PS2_STATUS) & 2 != 0 {
            core::hint::spin_loop();
        }
        outb(PS2_CMD, 0xD4);
        while inb(PS2_STATUS) & 2 != 0 {
            core::hint::spin_loop();
        }
        outb(PS2_DATA, 0xF4);
    }
    kserialln!("[ps2] 键盘 + 鼠标（aux）已使能（轮询模式）");
}

fn push(st: &mut State, ev: u32) {
    let next = (st.head + 1) % QUEUE;
    if next == st.tail {
        // 队列满：丢最老的，宁可丢事件也不要卡住中断/tick 路径
        st.tail = (st.tail + 1) % QUEUE;
    }
    st.buf[st.head] = ev;
    st.head = next;
}

/// 取一个事件；没有返回 `None`。
pub fn poll_event() -> Option<u32> {
    let mut st = ST.lock();
    if st.head == st.tail {
        return None;
    }
    let ev = st.buf[st.tail];
    st.tail = (st.tail + 1) % QUEUE;
    Some(ev)
}

/// 扫一次控制器。由定时器 tick 调用（每秒 100 次）。
pub fn poll() {
    // 每次最多取 16 个字节，防止某次异常把 tick 拖住
    for _ in 0..16 {
        // SAFETY: 状态端口只读
        let status = unsafe { inb(PS2_STATUS) };
        if status & 1 == 0 {
            break;
        }
        let byte = unsafe { inb(PS2_DATA) };
        // bit5 = 数据来自辅助端口（鼠标）
        if status & 0x20 != 0 {
            handle_mouse_byte(byte);
            continue;
        }
        handle_scancode(byte);
    }
}

/// 鼠标 3 字节包：`flags, dx, dy`。flags 的 bit3 恒为 1，用来做同步。
fn handle_mouse_byte(b: u8) {
    let mut st = ST.lock();
    if st.mouse_idx == 0 {
        // 不同步的包直接丢掉，等下一个 bit3=1 的头字节
        if b & 0x08 == 0 {
            return;
        }
        st.mouse_pkt[0] = b;
        st.mouse_idx = 1;
        return;
    }
    let idx = st.mouse_idx as usize;
    st.mouse_pkt[idx] = b;
    st.mouse_idx += 1;
    if st.mouse_idx < 3 {
        return;
    }
    st.mouse_idx = 0;

    let f = st.mouse_pkt[0];
    // 溢出位：丢掉这一包，别把位移算爆
    if f & 0xC0 != 0 {
        return;
    }
    let mut dx = st.mouse_pkt[1] as i32;
    let mut dy = st.mouse_pkt[2] as i32;
    if f & 0x10 != 0 {
        dx -= 256; // x 为负
    }
    if f & 0x20 != 0 {
        dy -= 256; // y 为负
    }
    // PS/2 的 y 轴向上为正，屏幕坐标向下为正
    st.mouse_dx += dx;
    st.mouse_dy -= dy;
    st.mouse_btn = f & 0x07;
    st.mouse_dirty = true;
}

/// 取一次鼠标增量与按键状态；自上次读取以来没有变化返回 `None`。
///
/// 返回 `(dx, dy, buttons)`：`buttons` bit0=左 bit1=右 bit2=中。
pub fn poll_mouse() -> Option<(i32, i32, u8)> {
    let mut st = ST.lock();
    if !st.mouse_dirty {
        return None;
    }
    let r = (st.mouse_dx, st.mouse_dy, st.mouse_btn);
    st.mouse_dx = 0;
    st.mouse_dy = 0;
    st.mouse_dirty = false;
    Some(r)
}

fn handle_scancode(sc: u8) {
    let mut st = ST.lock();

    if sc == 0xE0 {
        st.ext = true;
        return;
    }
    if sc == 0xE1 {
        // Pause 键的多字节序列，直接忽略
        st.ext = false;
        return;
    }

    let released = sc & 0x80 != 0;
    let code = sc & 0x7F;

    // 修饰键（不区分扩展）
    match code {
        0x2A | 0x36 => {
            if released {
                st.mods &= !MOD_SHIFT;
            } else {
                st.mods |= MOD_SHIFT;
            }
            if st.ext {
                st.ext = false;
            }
            return;
        }
        0x1D => {
            // 扩展码 0xE0 0x1D 是右 Ctrl，语义相同
            if released {
                st.mods &= !MOD_CTRL;
            } else {
                st.mods |= MOD_CTRL;
            }
            st.ext = false;
            return;
        }
        0x38 => {
            if released {
                st.mods &= !MOD_ALT;
            } else {
                st.mods |= MOD_ALT;
            }
            st.ext = false;
            return;
        }
        _ => {}
    }

    let ext = st.ext;
    st.ext = false;

    let key = if ext {
        match code {
            0x48 => KEY_UP,
            0x50 => KEY_DOWN,
            0x4B => KEY_LEFT,
            0x4D => KEY_RIGHT,
            0x47 => KEY_HOME,
            0x4F => KEY_END,
            0x49 => KEY_PGUP,
            0x51 => KEY_PGDN,
            0x53 => KEY_DELETE,
            0x52 => KEY_INSERT,
            0x1C => KEY_ENTER,
            0x35 => b'/',
            _ => return,
        }
    } else {
        match code {
            0x01 => KEY_ESC,
            0x0E => KEY_BACKSPACE,
            0x0F => KEY_TAB,
            0x1C => KEY_ENTER,
            0x3B => KEY_F1,
            0x3C => KEY_F1 + 1,
            0x3D => KEY_F1 + 2,
            0x3E => KEY_F1 + 3,
            0x3F => KEY_F1 + 4,
            0x40 => KEY_F1 + 5,
            0x41 => KEY_F1 + 6,
            0x42 => KEY_F1 + 7,
            0x43 => KEY_F1 + 8,
            0x44 => KEY_F1 + 9,
            _ => match ascii_of(code, st.mods & MOD_SHIFT != 0) {
                Some(c) => c,
                None => return,
            },
        }
    };

    let ev = (key as u32) | ((if released { 0 } else { 1 }) << 8) | ((st.mods as u32) << 16);
    push(&mut st, ev);
}

/// 扫描码（set 1，US 布局）→ ASCII。
fn ascii_of(code: u8, shift: bool) -> Option<u8> {
    let v = match code {
        0x02 => if shift { b'!' } else { b'1' },
        0x03 => if shift { b'@' } else { b'2' },
        0x04 => if shift { b'#' } else { b'3' },
        0x05 => if shift { b'$' } else { b'4' },
        0x06 => if shift { b'%' } else { b'5' },
        0x07 => if shift { b'^' } else { b'6' },
        0x08 => if shift { b'&' } else { b'7' },
        0x09 => if shift { b'*' } else { b'8' },
        0x0A => if shift { b'(' } else { b'9' },
        0x0B => if shift { b')' } else { b'0' },
        0x0C => if shift { b'_' } else { b'-' },
        0x0D => if shift { b'+' } else { b'=' },
        0x10..=0x19 => {
            let base = b"qwertyuiop"[(code - 0x10) as usize];
            if shift { base - 32 } else { base }
        }
        0x1A => if shift { b'{' } else { b'[' },
        0x1B => if shift { b'}' } else { b']' },
        0x1E..=0x26 => {
            let base = b"asdfghjkl"[(code - 0x1E) as usize];
            if shift { base - 32 } else { base }
        }
        0x27 => if shift { b':' } else { b';' },
        0x28 => if shift { b'"' } else { b'\'' },
        0x29 => if shift { b'~' } else { b'`' },
        0x2B => if shift { b'|' } else { b'\\' },
        0x2C..=0x32 => {
            let base = b"zxcvbnm"[(code - 0x2C) as usize];
            if shift { base - 32 } else { base }
        }
        0x33 => if shift { b'<' } else { b',' },
        0x34 => if shift { b'>' } else { b'.' },
        0x35 => if shift { b'?' } else { b'/' },
        0x39 => b' ',
        _ => return None,
    };
    Some(v)
}
