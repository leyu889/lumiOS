# -*- coding: utf-8 -*-
"""给 lumiwin 加一个最小 HTML 渲染器，把第 2 个窗口变成浏览器。"""
import io

P = r'E:\workbuddy\Deepseek_OS\kernel\userland\src\bin\lumiwin.rs'
s = io.open(P, encoding='utf-8').read()

HTML = r'''
/* ============================ 最小 HTML 渲染 ============================ */

/// 内置演示页。等 `/data/index.html` 存在时优先读文件。
const DEMO_HTML: &str = "<h1>LumiOS Browser<\/h1>\
<p>This is a <b>minimal HTML renderer<\/b> written from scratch. \
No Servo, no WebKit, no layout engine borrowed from anywhere.<\/p>\
<hr>\
<h2>What it supports<\/h2>\
<ul>\
<li>h1 / h2 headings, larger glyphs by double-height drawing\
<li>p paragraphs with word wrapping\
<li>hr horizontal rule\
<li>ul / li bullet lists\
<li>inline text, entities &amp;amp; &amp;lt; &amp;gt;\
<\/ul>\
<hr>\
<h2>Why not Servo yet<\/h2>\
<p>Servo needs std threads, futex, file-backed mmap and a GL surface. \
The kernel has none of those four today, so wiring Servo in would stop at \
the first pthread_create. This renderer is the test bed: it proves the \
graphics stack can host a document pipeline, and it is the thing a real \
engine would eventually replace.<\/p>\
<p>Rendering pipeline: tokenize -> block layout -> glyph blit.<\/p>";

/// 一个极简的行式渲染器：只认块级标签，不做 CSS，不做行内排版。
///
/// 这不是"半个浏览器"，是**渲染链路的验证台**：它证明从字节流到像素
/// 这条路是通的（词法 → 布局 → 字模），将来换真引擎时这段整体被替换。
struct Renderer<'a> {
    cv: &'a Canvas,
    x: i32,
    y: i32,
    max_x: i32,
    /// 当前字号倍率（h1/h2 用 2/1，正文 1）
    scale: i32,
}

impl<'a> Renderer<'a> {
    fn new(cv: &'a Canvas) -> Self {
        Renderer {
            cv,
            x: 20,
            y: 18,
            max_x: cv.width as i32 - 20,
            scale: 1,
        }
    }

    /// 画一个字符；倍率 2 时用 2x2 像素块模拟大字号（没有矢量字库）。
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
                    let px = x + dx as i32 * 2;
                    let py = y + dy as i32 * 2;
                    self.cv.fill(px, py, 2, 2, color);
                }
            }
        }
    }

    fn newline(&mut self, h: i32) {
        self.x = 20;
        self.y += h;
    }

    /// 输出一段纯文本，按词折行。
    fn text(&mut self, s: &str, color: u32, size: i32) {
        let cw = 8 * size;
        let lh = 16 * size + 4;
        let mut i = 0usize;
        let bytes = s.as_bytes();
        while i < bytes.len() {
            // 取一个词（到空格或行尾）
            let mut j = i;
            while j < bytes.len() && bytes[j] != b' ' {
                j += 1;
            }
            let word = &s[i..j];
            let w = word.len() as i32 * cw;
            if self.x + w > self.max_x && self.x > 20 {
                self.newline(lh);
            }
            for b in word.bytes() {
                self.glyph(self.x, self.y, b, color);
                self.x += cw;
            }
            // 补一个空格
            if j < bytes.len() {
                self.x += cw;
            }
            i = j + 1;
        }
        self.newline(lh);
    }
}

/// 渲染一段 HTML（只认块级标签 + 实体）。
fn render_html(cv: &Canvas, src: &str) {
    cv.clear(WIN_BG);
    let mut r = Renderer::new(cv);
    let mut i = 0usize;
    let b = src.as_bytes();
    let mut text_start = 0usize;

    // 把 [text_start, i) 这段当纯文本吐出去
    macro_rules! flush_text {
        ($color:expr, $size:expr) => {{
            if text_start < i {
                let raw = &src[text_start..i];
                let clean = strip_ws(raw);
                if !clean.is_empty() {
                    r.text(&clean, $color, $size);
                }
            }
        }};
    }

    while i < b.len() {
        if b[i] == b'<' {
            flush_text!(TEXT, r.scale);
            // 解析标签名
            let mut j = i + 1;
            let closing = j < b.len() && b[j] == b'/';
            if closing {
                j += 1;
            }
            let name_start = j;
            while j < b.len() && (b[j].is_ascii_alphanumeric()) {
                j += 1;
            }
            let name = &src[name_start..j];
            while j < b.len() && b[j] != b'>' {
                j += 1;
            }
            let after = (j + 1).min(b.len());

            match name {
                "h1" => {
                    if closing {
                        r.scale = 1;
                        r.newline(10);
                    } else {
                        r.scale = 2;
                        r.y += 6;
                    }
                }
                "h2" => {
                    if closing {
                        r.scale = 1;
                        r.newline(8);
                    } else {
                        r.scale = 1;
                        r.y += 4;
                    }
                }
                "p" => {
                    if closing {
                        r.newline(8);
                    } else if r.y > 24 {
                        r.newline(4);
                    }
                }
                "hr" => {
                    r.y += 6;
                    cv.fill(20, r.y, cv.width as i32 - 40, 1, WIN_BORDER);
                    r.y += 8;
                    r.x = 20;
                }
                "li" => {
                    if closing {
                        r.newline(4);
                    } else {
                        r.x = 20;
                        r.glyph(20, r.y, b'-', ACCENT);
                        r.x = 34;
                    }
                }
                "ul" => {
                    if closing {
                        r.newline(6);
                    }
                }
                "br" => {
                    r.newline(16 * r.scale + 4);
                }
                _ => {} // b / i / a / span 等：忽略标签本身，保留文字
            }
            i = after;
            text_start = i;
        } else {
            i += 1;
        }
    }
    flush_text!(TEXT, r.scale);
}

/// 压缩空白并解码几个常见实体（&amp; &lt; &gt;）。
fn strip_ws(s: &str) -> alloc::string::String {
    use alloc::string::String;
    let mut out = String::new();
    let mut last_space = false;
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
            i += 1;
            continue;
        }
        last_space = false;
        if c == b'&' {
            let rest = &s[i..];
            if rest.starts_with("&amp;") {
                out.push('&');
                i += 5;
                continue;
            }
            if rest.starts_with("&lt;") {
                out.push('<');
                i += 4;
                continue;
            }
            if rest.starts_with("&gt;") {
                out.push('>');
                i += 4;
                continue;
            }
        }
        out.push(c as char);
        i += 1;
    }
    out
}

/// 打开浏览器：优先读 /data/index.html，没有就用内置演示页。
fn browse(d: &mut Desktop) {
    let w = d.wins[2];
    let cv = match w.canvas {
        Some(c) => c,
        None => return,
    };
    let mut buf = alloc::string::String::new();
    let fd = liblumi::open("/data/index.html", liblumi::O_RDONLY);
    if fd >= 0 {
        let mut chunk = [0u8; 512];
        loop {
            let n = read(fd, &mut chunk);
            if n <= 0 {
                break;
            }
            for i in 0..n as usize {
                buf.push(chunk[i] as char);
            }
        }
        close(fd);
    }
    let src: &str = if buf.is_empty() { DEMO_HTML } else { &buf };
    render_html(&cv, src);
}

'''

s = s.replace("/// 把窗口内容画进它自己的缓冲。", HTML + "/// 把窗口内容画进它自己的缓冲。", 1)

# 启动时渲染一次浏览器窗口
s = s.replace(
    '    out_str(&mut d, "LumiOS shell - type help");',
    '    browse(&mut d);\n\n    out_str(&mut d, "LumiOS shell - type help");',
    1,
)

io.open(P, 'w', encoding='utf-8').write(s)
print('浏览器渲染器已加入')
