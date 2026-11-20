//! 用户态图形/输入自检程序（LumiOS 0.1 的地基验证）。
//!
//! 做三件事，任何一步失败都能从串口看出来：
//!   1. 把全屏帧缓冲映射进自己的地址空间，画一块可识别的图案 + 文字
//!   2. 进入最多 6 秒的事件循环，把收到的按键回显到屏幕上并计数
//!   3. 打印统计并退出——退出码 = 收到的按键数（>0 表示键盘链路真的通了）
//!
//! 它同时是"窗口服务之前的最小证明"：如果这个程序能画出东西、能收到键，
//! 那么窗口服务缺的只是合成与窗口管理逻辑，而不是内核能力。

#![no_std]
#![no_main]

use liblumi::*;

const BG: u32 = 0x0010_141C;
const FG: u32 = 0x00E6_E8EC;
const ACCENT: u32 = 0x007A_A2F7;
const OK: u32 = 0x003F_C46A;
const DIM: u32 = 0x0080_8A99;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let canvas = match Canvas::screen() {
        Some(c) => c,
        None => {
            println("[lumidemo] 没有帧缓冲（fb_map 失败）");
            exit(120);
        }
    };

    println("[lumidemo] 帧缓冲已映射，开始绘制");

    // 顶部标题栏
    canvas.fill(0, 0, canvas.width as i32, 56, 0x0018_1F2B);
    canvas.text(16, 20, "LumiOS userland / fb + font + input", FG);
    chart(&canvas);
    canvas.text(24, 92, "keys:", DIM);

    let mut count = 0u32;
    let mut last = 0u8;
    let mut buf = [0u8; 48];

    // 有限时长：4000 次轮询 × 2000 次空转 ≈ 1 秒量级。
    // **不能写成"收到键就重置计数"**，否则有人按住键时这个自检永远不结束。
    for _ in 0..4000u32 {
        while let Some(ev) = input_read() {
            if !ev.pressed {
                continue;
            }
            count += 1;
            last = ev.code;
            let s = fmt_key(&mut buf, count, ev.code);
            canvas.text_bg(24, 116, s, OK, BG);
        }
        for _ in 0..2000 {
            core::hint::spin_loop();
        }
    }
    let _ = last;

    println("[lumidemo] 结束");
    exit(count as i64)
}

/// 画一块渐变 + 三个色块，用来确认像素格式（BGR vs RGB）没搞反。
fn chart(c: &Canvas) {
    // 横向渐变条：红分量递增
    for x in 0..(c.width.min(512)) {
        let r = (x * 255 / 511) as u32;
        let color = (r << 16) | 0x0040_80;
        c.fill(24 + x as i32, 60, 1, 18, color);
    }
    // 三个纯色块：顺序为 红 绿 蓝。屏幕上顺序不对说明通道顺序错了。
    c.fill(552, 60, 48, 18, 0x00FF_0000);
    c.fill(608, 60, 48, 18, 0x0000_FF00);
    c.fill(664, 60, 48, 18, 0x0000_00FF);
    c.text(24, 152, "R  G  B  order check", DIM);
    let _ = ACCENT;
}

/// 极简数字格式化（不引 core::fmt，省体积也省栈）。
fn fmt_key(buf: &mut [u8; 48], n: u32, code: u8) -> &str {
    let prefix = b"count=";
    buf[..prefix.len()].copy_from_slice(prefix);
    let mut i = prefix.len();
    let mut v = n;
    let mut tmp = [0u8; 10];
    let mut t = 0;
    if v == 0 {
        buf[i] = b'0';
        i += 1;
    } else {
        while v > 0 {
            tmp[t] = b'0' + (v % 10) as u8;
            v /= 10;
            t += 1;
        }
        while t > 0 {
            t -= 1;
            buf[i] = tmp[t];
            i += 1;
        }
    }
    buf[i] = b' ';
    i += 1;
    buf[i] = b'k';
    buf[i + 1] = b'e';
    buf[i + 2] = b'y';
    buf[i + 3] = b'=';
    i += 4;
    if code >= 0x20 && code < 0x7F {
        buf[i] = code;
        i += 1;
    } else {
        buf[i] = b'0';
        buf[i + 1] = b'x';
        i += 2;
        const HEX: &[u8; 16] = b"0123456789abcdef";
        buf[i] = HEX[(code >> 4) as usize];
        buf[i + 1] = HEX[(code & 0xF) as usize];
        i += 2;
    }
    // SAFETY: 内容全是 ASCII，且 i <= buf.len()。
    unsafe { core::str::from_utf8_unchecked(&buf[..i]) }
}
