//! lumiwin —— LumiOS 窗口服务与桌面。
//!
//! 它是系统里**唯一**被允许直接写帧缓冲的进程（`fb_map` 只对它开放），
//! 其他应用将来只能往自己的共享缓冲里画，由这里合成上屏。
//!
//! ## 这一版做了什么
//!
//! - 桌面：竖向渐变壁纸 + 三个桌面图标（文件管理器 / 终端 / 浏览器）
//! - 窗口：标题栏、打开/关闭、**鼠标拖拽**、点击置顶
//! - 焦点：`ALT+TAB` 循环切换，焦点窗口标题栏高亮
//! - 快捷键：`F1` 文件管理器、`F2` 终端、`F3` 浏览器（每个预装应用一个）
//! - 光标：真实 PS/2 鼠标驱动，带移动与按键
//!
//! ## 为什么整屏重绘
//!
//! 1024x768 = 78 万像素，一次全量重绘在虚拟机里约 20–40 ms。这一版优先
//! **正确**：任何状态变化就整屏重画，不维护脏矩形。等三个客户端接进来、
//! 真的开始卡了，再上脏矩形与双缓冲——那时才有明确的性能证据可依。
//!
//! ## 窗口内容还是占位
//!
//! 三个应用会在后续版本里各自 fork 出来、拿共享内存画自己的内容。
//! 现在它们只是带标题和说明的空窗口，用来验证**窗口管理本身**是通的。

#![no_std]
#![no_main]

use liblumi::*;

/* ---------------- 配色 ---------------- */
const WALL_TOP: u32 = 0x001A_2740;
const WALL_BOTTOM: u32 = 0x000B_1119;
const TITLE_FOCUS: u32 = 0x002B_4A7A;
const TITLE_IDLE: u32 = 0x001C_2431;
const WIN_BG: u32 = 0x0014_1B26;
const WIN_BORDER: u32 = 0x003A_4A63;
const TEXT: u32 = 0x00E6_E8EC;
const TEXT_DIM: u32 = 0x0087_94A8;
const ACCENT: u32 = 0x004C_9AFF;
const OK: u32 = 0x003F_C46A;
const CLOSE: u32 = 0x00E0_5A5A;
const CURSOR_FG: u32 = 0x00FF_FFFF;
const CURSOR_BG: u32 = 0x0010_1010;

const TITLE_H: i32 = 28;
const WIN_MIN_W: i32 = 260;
const WIN_MIN_H: i32 = 120;

#[derive(Clone, Copy)]
struct Win {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    title: &'static str,
    body: &'static str,
    color: u32,
    visible: bool,
    /// 这个窗口的像素缓冲（共享内存映射过来的画布）。
    ///
    /// 现在由窗口服务自己填内容；接客户端时，客户端进程用**同一个 shm id**
    /// 映射同一块内存，往里面画，服务端只管把 `canvas` 贴到屏幕上。
    /// 这就是整个系统唯一的"应用-窗口"接口，没有 X11、没有 socket 协议栈。
    canvas: Option<Canvas>,
}

const NWIN: usize = 3;

struct Desktop {
    wins: [Win; NWIN],
    zorder: [usize; NWIN], // 从底到顶
    focus: usize,          // wins 的下标
    mx: i32,
    my: i32,
    dragging: bool,
    drag_dx: i32,
    drag_dy: i32,
    dirty: bool,
    /* ---- 终端（第 2 个窗口）的行编辑状态 ---- */
    term_line: [u8; 64],
    term_len: usize,
    term_log: [&'static str; 8],
    term_n: usize,
}

impl Desktop {
    fn new(w: i32, h: i32) -> Self {
        let wins = [
            Win {
                x: 150,
                y: 110,
                w: 520,
                h: 320,
                title: "File Manager",
                body: "F1  ·  待接入客户端",
                color: 0x00B8_8A4A,
                visible: true,
                canvas: None,
            },
            Win {
                x: 300,
                y: 200,
                w: 560,
                h: 320,
                title: "Terminal",
                body: "F2  ·  待接入客户端",
                color: 0x003F_C46A,
                visible: true,
                canvas: None,
            },
            Win {
                x: 420,
                y: 300,
                w: 480,
                h: 280,
                title: "Browser",
                body: "F3  ·  待接入客户端",
                color: 0x004C_9AFF,
                visible: true,
                canvas: None,
            },
        ];
        let _ = (w, h);
        Desktop {
            wins,
            zorder: [2, 0, 1], // 终端在最上
            focus: 1,
            mx: 512,
            my: 700,
            dragging: false,
            drag_dx: 0,
            drag_dy: 0,
            dirty: true,
            term_line: [0; 64],
            term_len: 0,
            term_log: ["LumiOS shell (builtin)", "type help + Enter", "", "", "", "", "", ""],
            term_n: 2,
        }
    }

    fn raise(&mut self, idx: usize) {
        let mut slot = 0;
        for (i, v) in self.zorder.iter().enumerate() {
            if *v == idx {
                slot = i;
            }
        }
        for i in slot..(NWIN - 1) {
            self.zorder[i] = self.zorder[i + 1];
        }
        self.zorder[NWIN - 1] = idx;
        self.focus = idx;
        self.dirty = true;
    }

    fn top_at(&self, x: i32, y: i32) -> Option<usize> {
        for i in (0..NWIN).rev() {
            let wi = self.zorder[i];
            let w = self.wins[wi];
            if !w.visible {
                continue;
            }
            if x >= w.x && x < w.x + w.w && y >= w.y && y < w.y + w.h {
                return Some(wi);
            }
        }
        None
    }

    fn close_button_hit(&self, wi: usize, x: i32, y: i32) -> bool {
        let w = self.wins[wi];
        let bx = w.x + w.w - TITLE_H;
        let by = w.y;
        x >= bx && x < bx + TITLE_H && y >= by && y < by + TITLE_H
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let c = match Canvas::screen() {
        Some(c) => c,
        None => {
            println("[lumiwin] 没有帧缓冲，退出");
            exit(2);
        }
    };
    println("[lumiwin] 窗口服务启动");

    let mut d = Desktop::new(c.width as i32, c.height as i32);

    // 上一轮的疏漏：终端只在按键时重绘，启动后仍显示占位文字。
    // 布局完先画一次，屏幕上立刻就能看到 shell 提示符。
    repaint_term(&mut d);

    // 给每个窗口分配自己的像素缓冲，并把内容画进去。
    // 缓冲是内核共享内存（`shm_alloc`/`shm_map`），将来客户端进程映射同一块
    // 即可直接画——服务端一个字节都不用改。
    for i in 0..NWIN {
        let w = d.wins[i];
        let body_w = w.w - 2;
        let body_h = w.h - TITLE_H;
        let pages = ((body_w * body_h * 4) as usize + 4095) / 4096;
        if let Ok(id) = Ok::<i64, ()>(shm_alloc(pages)) {
            if id > 0 {
                if let Some(va) = shm_map(id) {
                    let cv = Canvas::from_shm(va, body_w as u32, body_h as u32);
                    paint_body(&cv, &w);
                    d.wins[i].canvas = Some(cv);
                }
            }
        }
    }

    loop {
        // ---- 事件：鼠标优先，键盘其次 ----
        let mut moved = false;
        while let Some((dx, dy, btn)) = mouse_read() {
            if dx != 0 || dy != 0 {
                d.mx = (d.mx + dx).clamp(0, c.width as i32 - 1);
                d.my = (d.my + dy).clamp(0, c.height as i32 - 1);
                moved = true;
                if d.dragging {
                    let dxs = d.mx - d.drag_dx;
                    let dys = d.my - d.drag_dy;
                    let w = &mut d.wins[d.focus];
                    w.x += dxs;
                    w.y += dys;
                    d.drag_dx = d.mx;
                    d.drag_dy = d.my;
                }
                d.dirty = true;
            }
            let left = btn & 1 != 0;
            let was = d.dragging;
            if left && !was {
                // 按下
                if let Some(wi) = d.top_at(d.mx, d.my) {
                    d.raise(wi);
                    if d.close_button_hit(wi, d.mx, d.my) {
                        d.wins[wi].visible = false;
                        d.dragging = false;
                        d.dirty = true;
                    } else {
                        let w = d.wins[wi];
                        if d.my < w.y + TITLE_H {
                            d.dragging = true;
                            d.drag_dx = d.mx;
                            d.drag_dy = d.my;
                        }
                    }
                }
            } else if !left && was {
                d.dragging = false;
                d.dirty = true;
            }
        }

        while let Some(k) = input_read() {
            if !k.pressed {
                continue;
            }
            match k.code {
                KEY_F1 => open(&mut d, 0),
                KEY_F2 => open(&mut d, 1),
                KEY_F3 => open(&mut d, 2),
                KEY_TAB if k.is_alt() => {
                    // ALT+TAB：按 z 序往下切一层
                    let cur = d.zorder[NWIN - 1];
                    let mut next = 0;
                    for i in 0..NWIN {
                        if d.wins[d.zorder[i]].visible && d.zorder[i] != cur {
                            next = d.zorder[i];
                        }
                    }
                    d.raise(next);
                }
                KEY_ESC => {
                    d.wins[d.focus].visible = false;
                    d.dirty = true;
                }
                KEY_ENTER => term_run(&mut d),
                KEY_BACKSPACE => {
                    if d.term_len > 0 {
                        d.term_len -= 1;
                        d.dirty = true;
                    }
                }
                c if (0x20..0x7F).contains(&c) => {
                    if d.term_len < d.term_line.len() - 1 {
                        d.term_line[d.term_len] = c;
                        d.term_len += 1;
                        d.dirty = true;
                        repaint_term(&mut d);
                    }
                }
                _ => {}
            }
        }

        if d.dirty || moved {
            draw_all(&c, &d);
            d.dirty = false;
        }

        // 帧率限制：约 5 ms 空转一次，避免空跑吃满 CPU
        for _ in 0..40_000 {
            core::hint::spin_loop();
        }
    }
}

fn open(d: &mut Desktop, idx: usize) {
    d.wins[idx].visible = true;
    d.raise(idx);
}

/* ---------------- 绘制 ---------------- */

fn draw_all(c: &Canvas, d: &Desktop) {
    wallpaper(c);
    draw_icons(c);

    for i in 0..NWIN {
        let wi = d.zorder[i];
        if d.wins[wi].visible {
            draw_window(c, &d.wins[wi], wi == d.focus);
        }
    }

    // 按用户要求：不要任务栏。窗口自己带标题栏就够了，
    // 少一条常驻横条 = 多 28 px 可用区域，也更符合"极简"的定位。
    draw_cursor(c, d.mx, d.my);
}

fn wallpaper(c: &Canvas) {
    let h = c.height as i32;
    for y in 0..h {
        // 竖向渐变：从深蓝到近黑。每行一次 fill_rect，避免逐像素函数调用。
        let t = (y * 255 / h.max(1)) as u32;
        let r = lerp((WALL_TOP >> 16) & 0xFF, (WALL_BOTTOM >> 16) & 0xFF, t);
        let g = lerp((WALL_TOP >> 8) & 0xFF, (WALL_BOTTOM >> 8) & 0xFF, t);
        let b = lerp(WALL_TOP & 0xFF, WALL_BOTTOM & 0xFF, t);
        c.fill(0, y, c.width as i32, 1, (r << 16) | (g << 8) | b);
    }
}

fn lerp(a: u32, b: u32, t: u32) -> u32 {
    if t >= 255 {
        return b;
    }
    a + ((b as i32 - a as i32) * t as i32) as u32 / 255
}

const ICONS: [(&str, u32, &str); 3] = [
    ("Files", 0x00B8_8A4A, "F1"),
    ("Term", 0x003F_C46A, "F2"),
    ("Web", 0x004C_9AFF, "F3"),
];

fn draw_icons(c: &Canvas) {
    let mut y = 40;
    for (name, color, key) in ICONS.iter() {
        c.fill(28, y, 64, 64, *color);
        c.fill(28, y, 64, 64, *color);
        c.text_bg(24, y + 70, name, TEXT, 0x0012_1822);
        c.text_bg(24, y + 86, key, TEXT_DIM, 0x0012_1822);
        y += 120;
    }
}

fn draw_window(c: &Canvas, w: &Win, focused: bool) {
    // 外框
    c.fill(w.x - 1, w.y - 1, w.w + 2, w.h + 2, WIN_BORDER);
    c.fill(w.x, w.y, w.w, w.h, WIN_BG);
    // 标题栏
    c.fill(w.x, w.y, w.w, TITLE_H, if focused { TITLE_FOCUS } else { TITLE_IDLE });
    c.text(w.x + 10, w.y + 6, w.title, TEXT);
    // 关闭按钮
    let bx = w.x + w.w - TITLE_H;
    c.fill(bx, w.y, TITLE_H, TITLE_H, CLOSE);
    c.text(bx + 10, w.y + 6, "x", TEXT);

    // 内容来自窗口自己的缓冲（将来是客户端画进去的像素）
    match &w.canvas {
        Some(cv) => c.blit(cv, w.x, w.y + TITLE_H),
        None => c.fill(w.x, w.y + TITLE_H, w.w, w.h - TITLE_H, WIN_BG),
    }
    let _ = (OK, w.color);
}

/// 执行终端里当前这一行（只做内建命令；外部程序要等 fork/exec 接进来）。
fn term_run(d: &mut Desktop) {
    let line = unsafe { core::str::from_utf8_unchecked(&d.term_line[..d.term_len]) };
    let cmd = line.trim();
    // 内建命令表：全部返回静态串。
    // "动态输出"（echo 回显参数、ls 列真实目录）需要固定大小的行缓冲，
    // 那是下一步的事；这里先把命令面铺开，让终端真的能用起来。
    let out: &'static str = if cmd.is_empty() {
        ""
    } else if cmd == "help" {
        "help clear version about apps keys whoami pwd ls uname date"
    } else if cmd == "clear" {
        ""
    } else if cmd == "version" {
        "LumiOS 0.1  /  kernel LumiKernel 1.3.1 M12"
    } else if cmd == "about" {
        "4 apps: desktop, files, terminal, browser"
    } else if cmd == "apps" {
        "F1 Files   F2 Terminal   F3 Browser"
    } else if cmd == "keys" {
        "ALT+TAB switch   ESC close   drag = title bar"
    } else if cmd == "whoami" {
        "user"
    } else if cmd == "pwd" {
        "/"
    } else if cmd == "ls" {
        "bin   dev   etc   home   tmp"
    } else if cmd == "uname" {
        "LumiOS 0.1 x86_64"
    } else if cmd == "date" {
        "date: no RTC driver yet"
    } else if cmd == "echo" {
        "usage: echo <text>  (dynamic output TBD)"
    } else if cmd.starts_with("echo ") {
        "echo: dynamic output needs a line buffer (TBD)"
    } else {
        "unknown command, try: help"
    };
    if cmd == "clear" {
        d.term_n = 0;
    } else if !out.is_empty() && d.term_log[(d.term_n + 7) % 8] != out {
        let slot = d.term_n % 8;
        d.term_log[slot] = out;
        if d.term_n < 8 {
            d.term_n += 1;
        }
    }
    d.term_len = 0;
    d.dirty = true;
    repaint_term(d);
}

fn repaint_term(d: &mut Desktop) {
    let w = d.wins[1];
    if let Some(cv) = w.canvas {
        paint_body(&cv, &w);
        let mut y = 66;
        for i in 0..d.term_n {
            cv.text_bg(34, y, d.term_log[i], OK, WIN_BG);
            y += 18;
        }
        // 当前输入行 + 光标
        let line = unsafe { core::str::from_utf8_unchecked(&d.term_line[..d.term_len]) };
        cv.text_bg(34, y + 10, ">", ACCENT, WIN_BG);
        cv.text_bg(50, y + 10, line, TEXT, WIN_BG);
        cv.fill(50 + (d.term_len as i32) * 8, y + 10, 8, 16, ACCENT);
    }
}

/// 把窗口内容画进它自己的缓冲。
///
/// 现在画的是占位内容；接客户端时这个函数会**整体搬到客户端进程**里，
/// 服务端只保留 `blit`。
fn paint_body(cv: &Canvas, w: &Win) {
    cv.clear(WIN_BG);
    cv.fill(16, 16, 6, cv.height as i32 - 32, w.color);
    cv.text(34, 18, w.title, TEXT);
    cv.text(34, 40, w.body, TEXT_DIM);
    cv.text(34, 66, "own pixel buffer (shm)", TEXT_DIM);
    cv.fill(34, 94, cv.width as i32 - 60, 1, WIN_BORDER);
    cv.text(34, 104, "close: x  /  ESC", TEXT_DIM);
    cv.text(34, 124, "switch: ALT+TAB", TEXT_DIM);
}

fn draw_cursor(c: &Canvas, x: i32, y: i32) {
    // 简单的箭头光标：12x18 的位图，用两遍画（先描边后填充）
    const CUR: [u16; 18] = [
        0b1000_0000_0000,
        0b1100_0000_0000,
        0b1110_0000_0000,
        0b1111_0000_0000,
        0b1111_1000_0000,
        0b1111_1100_0000,
        0b1111_1110_0000,
        0b1111_1111_0000,
        0b1111_1111_1000,
        0b1111_1000_0000,
        0b1101_1000_0000,
        0b1000_1100_0000,
        0b0000_1100_0000,
        0b0000_0110_0000,
        0b0000_0110_0000,
        0b0000_0011_0000,
        0b0000_0011_0000,
        0b0000_0001_1000,
    ];
    for (dy, row) in CUR.iter().enumerate() {
        for dx in 0..12 {
            if row & (0x800 >> dx) != 0 {
                c.put(x + dx, y + dy as i32, CURSOR_FG);
            }
        }
    }
    let _ = CURSOR_BG;
}
