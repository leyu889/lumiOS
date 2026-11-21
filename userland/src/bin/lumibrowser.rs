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

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let cv = match Canvas::screen() {
        Some(c) => c,
        None => {
            println("[lumibrowser] 没有帧缓冲");
            exit(2);
        }
    };
    println("[lumibrowser] 独立进程启动，开始渲染");

    // 优先读 /data/index.html
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
    println("[lumibrowser] 渲染完成");

    loop {
        // 简单事件循环：ESC 退出，其余忽略
        if let Some(k) = input_read() {
            if k.pressed && k.code == KEY_ESC {
                exit(0);
            }
        }
        for _ in 0..20_000 {
            core::hint::spin_loop();
        }
    }
}
