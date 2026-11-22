//! lumibrowser —— LumiOS 浏览器（独立进程）。
//!
//! ## 现在这一版是什么
//!
//! 一个**自带渲染链路的独立进程**：自己映射帧缓冲、自己解析 HTML、
//! 自己把字画到屏幕上。它验证的是「浏览器可以作为独立应用存在」，
//! 而不是「它已经嵌进桌面窗口」——后者要等窗口服务的客户端协议
//! （共享内存 + 事件队列）接好，那时它会改成画进自己的窗口缓冲。
//!
//! ## 为什么不是 Servo
//!
//! Servo 需要 std 线程、futex、文件映射与 GL 表面，这套内核目前一样都没有。
//! 这里是**渲染链路的验证台**：词法 → 块级布局 → 折行 → 字模。
//! 换成真引擎时，被替换的就是这三段。

#![no_std]
#![no_main]

use liblumi::*;

const BG: u32 = 0x00FA_FAF7;
const FG: u32 = 0x0019_1C21;
const DIM: u32 = 0x0055_5F6B;
const ACCENT: u32 = 0x0025_63EB;
const RULE: u32 = 0x00D5_D9DE;
const HEAD_BG: u32 = 0x0011_1822;

const DEMO_HTML: &str = "<h1>LumiOS Browser</h1>\
<p>A minimal HTML renderer written from scratch: tokenizer, block layout, \
word wrapping, glyph blit. No Servo, no WebKit, no borrowed layout engine.</p>\
<hr>\
<h2>Supported</h2>\
<ul>\
<li>h1 / h2 headings (2x glyph scaling)\
<li>p paragraphs with word wrapping\
<li>hr horizontal rules\
<li>ul / li bullet lists\
<li>entities: &amp;amp; &amp;lt; &amp;gt;\
</ul>\
<hr>\
<h2>Next</h2>\
<p>Split rendering into its own window buffer, add scrolling and links, \
then decide whether a real engine can be ported at all.</p>";

struct R<'a> {
    cv: &'a Canvas,
    x: i32,
    y: i32,
    max_x: i32,
    scale: i32,
}

impl<'a> R<'a> {
    fn glyph(&self, x: i32, y: i32, ch: u8, color: u32) {
        if self.scale <= 1 {
            self.cv.glyph(x, y, ch, color);
            return;
        }
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
                    self.cv.fill(x + dx as i32 * 2, y + dy as i32 * 2, 2, 2, color);
                }
            }
        }
    }

    fn newline(&mut self, h: i32) {
        self.x = 48;
        self.y += h;
    }

    fn text(&mut self, s: &str, color: u32, size: i32) {
        let cw = 8 * size;
        let lh = 16 * size + 4;
        let bytes = s.as_bytes();
        let mut i = 0usize;
        while i < bytes.len() {
            let mut j = i;
            while j < bytes.len() && bytes[j] != b' ' {
                j += 1;
            }
            let word = &s[i..j];
            let w = word.len() as i32 * cw;
            if self.x + w > self.max_x && self.x > 48 {
                self.newline(lh);
            }
            for b in word.bytes() {
                self.glyph(self.x, self.y, b, color);
                self.x += cw;
            }
            if j < bytes.len() {
                self.x += cw;
            }
            i = j + 1;
        }
        self.newline(lh);
    }
}

fn strip_ws<'a>(s: &str, out: &'a mut [u8]) -> &'a str {
    let mut n = 0usize;
    let mut last_space = false;
    let b = s.as_bytes();
    let mut i = 0usize;
    while i < b.len() && n < out.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            if !last_space {
                out[n] = b' ';
                n += 1;
                last_space = true;
            }
            i += 1;
            continue;
        }
        last_space = false;
        if c == b'&' {
            let rest = &s[i..];
            if rest.starts_with("&amp;") {
                out[n] = b'&';
                n += 1;
                i += 5;
                continue;
            }
            if rest.starts_with("&lt;") {
                out[n] = b'<';
                n += 1;
                i += 4;
                continue;
            }
            if rest.starts_with("&gt;") {
                out[n] = b'>';
                n += 1;
                i += 4;
                continue;
            }
        }
        out[n] = c;
        n += 1;
        i += 1;
    }
    // SAFETY: 只写 ASCII 字节
    unsafe { core::str::from_utf8_unchecked(&out[..n]) }
}

fn render(cv: &Canvas, src: &str) {
    cv.clear(BG);
    // 顶部标题条
    cv.fill(0, 0, cv.width as i32, 40, HEAD_BG);
    cv.text(16, 12, "LumiOS Browser  -  minimal HTML", 0x00E6_E8EC);

    let mut r = R {
        cv,
        x: 48,
        y: 60,
        max_x: cv.width as i32 - 48,
        scale: 1,
    };
    let b = src.as_bytes();
    let mut i = 0usize;
    let mut text_start = 0usize;

    while i < b.len() {
        if b[i] == b'<' {
            if text_start < i {
                let raw = &src[text_start..i];
                let mut buf = [0u8; 512];
                let clean = strip_ws(raw, &mut buf);
                if !clean.is_empty() {
                    r.text(clean, FG, r.scale);
                }
            }
            let mut j = i + 1;
            let closing = j < b.len() && b[j] == b'/';
            if closing {
                j += 1;
            }
            let ns = j;
            while j < b.len() && b[j].is_ascii_alphanumeric() {
                j += 1;
            }
            let name = &src[ns..j];
            while j < b.len() && b[j] != b'>' {
                j += 1;
            }
            let after = (j + 1).min(b.len());
            match name {
                "h1" => {
                    if closing {
                        r.scale = 1;
                        r.newline(12);
                    } else {
                        r.scale = 2;
                        r.y += 6;
                    }
                }
                "h2" => {
                    if closing {
                        r.newline(10);
                    } else {
                        r.y += 6;
                        r.x = 48;
                    }
                }
                "p" => {
                    if closing {
                        r.newline(8);
                    } else if r.y > 70 {
                        r.newline(4);
                    }
                }
                "hr" => {
                    r.y += 8;
                    cv.fill(48, r.y, cv.width as i32 - 96, 1, RULE);
                    r.y += 10;
                    r.x = 48;
                }
                "li" => {
                    if closing {
                        r.newline(4);
                    } else {
                        r.x = 48;
                        r.glyph(48, r.y, b'-', ACCENT);
                        r.x = 66;
                    }
                }
                "ul" => {
                    if closing {
                        r.newline(6);
                    }
                }
                _ => {}
            }
            i = after;
            text_start = i;
        } else {
            i += 1;
        }
    }
    if text_start < b.len() {
        let raw = &src[text_start..];
        let mut buf = [0u8; 512];
        let clean = strip_ws(raw, &mut buf);
        if !clean.is_empty() {
            r.text(clean, FG, r.scale);
        }
    }
    let _ = DIM;
}

/// 从 /data/win.conf 读握手信息："id w h"
fn read_conf() -> Option<(i64, u32, u32)> {
    let fd = open("/data/win.conf", O_RDONLY);
    if fd < 0 {
        return None;
    }
    let mut buf = [0u8; 64];
    let n = read(fd, &mut buf);
    close(fd);
    if n <= 0 {
        return None;
    }
    let n = n as usize;
    let mut nums = [0i64; 3];
    let mut idx = 0usize;
    let mut cur: i64 = 0;
    let mut any = false;
    for i in 0..n {
        let c = buf[i];
        if c.is_ascii_digit() {
            cur = cur * 10 + (c - b'0') as i64;
            any = true;
        } else if any {
            if idx < 3 {
                nums[idx] = cur;
                idx += 1;
            }
            cur = 0;
            any = false;
        }
    }
    if any && idx < 3 {
        nums[idx] = cur;
        idx += 1;
    }
    if idx < 3 || nums[0] <= 0 {
        return None;
    }
    Some((nums[0], nums[1] as u32, nums[2] as u32))
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    // 客户端模式：**自己**申请一块共享缓冲，把 id 写进握手文件，
    // 窗口服务读到之后映射同一块内存贴到屏幕上。
    // 这样完全不需要 fork（内核直接拉起两个进程）。
    let mut cv_opt: Option<Canvas> = None;
    const BW: u32 = 520;
    const BH: u32 = 300;
    let pages = ((BW * BH * 4) as usize + 4095) / 4096;
    let id = shm_alloc(pages);
    if id > 0 {
        if let Some(va) = shm_map(id) {
            cv_opt = Some(Canvas::from_shm(va, BW, BH));
            // 写握手文件：id 宽 高
            let fd = open(
                "/data/win-browser.conf",
                O_WRONLY | O_CREAT | O_TRUNC,
            );
            if fd >= 0 {
                let mut line = [0u8; 32];
                let mut n = 0usize;
                let mut vals = [id as u64, BW as u64, BH as u64];
                for v in vals.iter_mut() {
                    let mut tmp = [0u8; 20];
                    let mut t = 0usize;
                    let mut x = *v;
                    if x == 0 {
                        tmp[0] = b'0';
                        t = 1;
                    }
                    while x > 0 {
                        tmp[t] = b'0' + (x % 10) as u8;
                        x /= 10;
                        t += 1;
                    }
                    while t > 0 {
                        t -= 1;
                        line[n] = tmp[t];
                        n += 1;
                    }
                    line[n] = b' ';
                    n += 1;
                }
                line[n] = 10;
                n += 1;
                write(fd, &line[..n]);
                close(fd);
            }
        }
    }
    let cv = match cv_opt {
        Some(c) => c,
        None => match Canvas::screen() {
            Some(c) => c,
            None => {
                exit(2);
            }
        },
    };

    static mut PAGE: [u8; 8192] = [0; 8192];
    let mut len = 0usize;
    let fd = open("/data/index.html", O_RDONLY);
    if fd >= 0 {
        let mut chunk = [0u8; 512];
        loop {
            let n = read(fd, &mut chunk);
            if n <= 0 {
                break;
            }
            for i in 0..n as usize {
                if len < 8192 {
                    unsafe {
                        PAGE[len] = chunk[i];
                    }
                    len += 1;
                }
            }
        }
        close(fd);
    }
    if len > 0 {
        let src = unsafe { core::str::from_utf8_unchecked(&PAGE[..len]) };
        render(&cv, src);
    } else {
        render(&cv, DEMO_HTML);
    }

    // **客户端不读键盘**：内核的输入事件队列是全局的，谁先取谁拿走。
    // 之前这里每个循环都 input_read()，把按键全吞了，
    // 导致窗口服务收不到任何键（表现为"按 F2 没反应、终端打不了字"）。
    // 键盘归窗口服务独占，客户端要事件得等 IPC 通道做好。
    loop {
        for _ in 0..200_000 {
            core::hint::spin_loop();
        }
    }
}
