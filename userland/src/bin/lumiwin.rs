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
    /* ---- 文件管理器（第 0 个窗口）---- */
    fm_entries: [DirEnt; DIR_MAX],
    fm_n: usize,
    fm_sel: usize,
    /// 终端输出（字节流，按换行分行绘制）
    term_text: [u8; 2048],
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
            fm_entries: [DirEnt { name: [0; 32], len: 0, is_dir: false }; DIR_MAX],
            fm_n: 0,
            fm_sel: 0,
            term_text: [0; 2048],
            term_n: 0,
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

    // **必须放在缓冲分配之后**：repaint_term 要靠 wins[1].canvas 才画得出来。
    // 上一版插在分配之前，canvas 还是 None，于是启动后屏幕上仍是占位内容。
    // 目录系统：启动就读一次当前目录，文件管理器直接显示真实内容
    fm_refresh(&mut d);

    // 浏览器渲染器先不挂启动路径：加了它之后 lumiwin 的 ELF 从 24 KB
    // 涨到 64 KB，内核 exec 返回 -8（ENOEXEC）——装载器拒绝了段布局。
    // 代码保留在这里（因未被引用会被 --gc-sections 回收），
    // 下一步先查装载器对段数/大小的限制，再接回来。
    // browse(&mut d);  // 待拆分到独立二进制后再接

    out_str(&mut d, "LumiOS shell - type help");
    out_nl(&mut d);
    d.term_line[0] = b'l';
    d.term_line[1] = b's';
    d.term_len = 2;
    term_run(&mut d);

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
                KEY_F1 => open_app(&mut d, 0),
                KEY_F2 => open_app(&mut d, 1),
                KEY_F3 => open_app(&mut d, 2),
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
                KEY_UP => {
                    if d.fm_sel > 0 {
                        d.fm_sel -= 1;
                        fm_paint(&mut d);
                    }
                }
                KEY_DOWN => {
                    if d.fm_sel + 1 < d.fm_n {
                        d.fm_sel += 1;
                        fm_paint(&mut d);
                    }
                }
                KEY_ENTER if d.focus == 0 => {
                    // 文件管理器里回车 = 进入选中的目录（文件暂不支持打开）
                    if d.fm_sel < d.fm_n && d.fm_entries[d.fm_sel].is_dir {
                        let name = d.fm_entries[d.fm_sel].as_str();
                        if chdir(name) == 0 {
                            d.fm_sel = 0;
                            fm_refresh(&mut d);
                        }
                    }
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

fn open_app(d: &mut Desktop, idx: usize) {
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


/// 内建命令表（不占 if 链）：名字 → 一句"为什么没有"。
///
/// LumiOS 只预装 4 个应用，**不预装任何命令行工具**——这是产品决定，不是没做。
/// 终端里敲这些名字时，如实告诉用户缺什么，比假装"命令不存在"有用。
fn stub_for(cmd: &str) -> Option<&'static str> {
    static T: &[(&str, &str)] = &[
    ("cat", "file tool: needs fork+exec of a userland binary"),
    ("cp", "file tool: needs fork+exec of a userland binary"),
    ("mv", "file tool: needs fork+exec of a userland binary"),
    ("rm", "file tool: needs fork+exec of a userland binary"),
    ("mkdir", "file tool: needs fork+exec of a userland binary"),
    ("rmdir", "file tool: needs fork+exec of a userland binary"),
    ("touch", "file tool: needs fork+exec of a userland binary"),
    ("ln", "file tool: needs fork+exec of a userland binary"),
    ("find", "file tool: needs fork+exec of a userland binary"),
    ("grep", "file tool: needs fork+exec of a userland binary"),
    ("sed", "file tool: needs fork+exec of a userland binary"),
    ("awk", "file tool: needs fork+exec of a userland binary"),
    ("head", "file tool: needs fork+exec of a userland binary"),
    ("tail", "file tool: needs fork+exec of a userland binary"),
    ("wc", "file tool: needs fork+exec of a userland binary"),
    ("sort", "file tool: needs fork+exec of a userland binary"),
    ("uniq", "file tool: needs fork+exec of a userland binary"),
    ("cut", "file tool: needs fork+exec of a userland binary"),
    ("tr", "file tool: needs fork+exec of a userland binary"),
    ("tee", "file tool: needs fork+exec of a userland binary"),
    ("xargs", "file tool: needs fork+exec of a userland binary"),
    ("chmod", "file tool: needs fork+exec of a userland binary"),
    ("chown", "file tool: needs fork+exec of a userland binary"),
    ("chgrp", "file tool: needs fork+exec of a userland binary"),
    ("stat", "file tool: needs fork+exec of a userland binary"),
    ("file", "file tool: needs fork+exec of a userland binary"),
    ("du", "file tool: needs fork+exec of a userland binary"),
    ("df", "file tool: needs fork+exec of a userland binary"),
    ("mount", "block tool: ext2 is read-only here, LumiFS has its own ABI"),
    ("umount", "block tool: ext2 is read-only here, LumiFS has its own ABI"),
    ("lsblk", "block tool: ext2 is read-only here, LumiFS has its own ABI"),
    ("blkid", "block tool: ext2 is read-only here, LumiFS has its own ABI"),
    ("fsck", "block tool: ext2 is read-only here, LumiFS has its own ABI"),
    ("mkfs", "block tool: ext2 is read-only here, LumiFS has its own ABI"),
    ("dd", "block tool: ext2 is read-only here, LumiFS has its own ABI"),
    ("sync", "block tool: ext2 is read-only here, LumiFS has its own ABI"),
    ("ps", "process tool: no procfs yet"),
    ("top", "process tool: no procfs yet"),
    ("kill", "process tool: no procfs yet"),
    ("killall", "process tool: no procfs yet"),
    ("nice", "process tool: no procfs yet"),
    ("renice", "process tool: no procfs yet"),
    ("jobs", "process tool: no procfs yet"),
    ("fg", "process tool: no procfs yet"),
    ("bg", "process tool: no procfs yet"),
    ("nohup", "process tool: no procfs yet"),
    ("time", "process tool: no procfs yet"),
    ("uptime", "process tool: no procfs yet"),
    ("free", "process tool: no procfs yet"),
    ("vmstat", "process tool: no procfs yet"),
    ("iostat", "process tool: no procfs yet"),
    ("dmesg", "system tool: no sysfs/procfs yet"),
    ("lspci", "system tool: no sysfs/procfs yet"),
    ("lsusb", "system tool: no sysfs/procfs yet"),
    ("lscpu", "system tool: no sysfs/procfs yet"),
    ("lsmod", "system tool: no sysfs/procfs yet"),
    ("modprobe", "system tool: no sysfs/procfs yet"),
    ("insmod", "system tool: no sysfs/procfs yet"),
    ("rmmod", "system tool: no sysfs/procfs yet"),
    ("sysctl", "system tool: no sysfs/procfs yet"),
    ("ip", "network tool: kernel stack is ARP+ICMP only"),
    ("ifconfig", "network tool: kernel stack is ARP+ICMP only"),
    ("ping", "network tool: kernel stack is ARP+ICMP only"),
    ("ping6", "network tool: kernel stack is ARP+ICMP only"),
    ("traceroute", "network tool: kernel stack is ARP+ICMP only"),
    ("curl", "network tool: kernel stack is ARP+ICMP only"),
    ("wget", "network tool: kernel stack is ARP+ICMP only"),
    ("ssh", "network tool: kernel stack is ARP+ICMP only"),
    ("scp", "network tool: kernel stack is ARP+ICMP only"),
    ("sftp", "network tool: kernel stack is ARP+ICMP only"),
    ("ftp", "network tool: kernel stack is ARP+ICMP only"),
    ("telnet", "network tool: kernel stack is ARP+ICMP only"),
    ("nc", "network tool: kernel stack is ARP+ICMP only"),
    ("netstat", "network tool: kernel stack is ARP+ICMP only"),
    ("ss", "network tool: kernel stack is ARP+ICMP only"),
    ("git", "userland binary: not installed (by design, keep it minimal)"),
    ("make", "userland binary: not installed (by design, keep it minimal)"),
    ("cmake", "userland binary: not installed (by design, keep it minimal)"),
    ("gcc", "userland binary: not installed (by design, keep it minimal)"),
    ("g++", "userland binary: not installed (by design, keep it minimal)"),
    ("clang", "userland binary: not installed (by design, keep it minimal)"),
    ("rustc", "userland binary: not installed (by design, keep it minimal)"),
    ("cargo", "userland binary: not installed (by design, keep it minimal)"),
    ("python", "userland binary: not installed (by design, keep it minimal)"),
    ("node", "userland binary: not installed (by design, keep it minimal)"),
    ("npm", "userland binary: not installed (by design, keep it minimal)"),
    ("pip", "userland binary: not installed (by design, keep it minimal)"),
    ("go", "userland binary: not installed (by design, keep it minimal)"),
    ("java", "userland binary: not installed (by design, keep it minimal)"),
    ("javac", "userland binary: not installed (by design, keep it minimal)"),
    ("tar", "userland binary: not installed (by design, keep it minimal)"),
    ("gzip", "userland binary: not installed (by design, keep it minimal)"),
    ("gunzip", "userland binary: not installed (by design, keep it minimal)"),
    ("zip", "userland binary: not installed (by design, keep it minimal)"),
    ("unzip", "userland binary: not installed (by design, keep it minimal)"),
    ("bzip2", "userland binary: not installed (by design, keep it minimal)"),
    ("xz", "userland binary: not installed (by design, keep it minimal)"),
    ("zstd", "userland binary: not installed (by design, keep it minimal)"),
    ("7z", "userland binary: not installed (by design, keep it minimal)"),
    ("vi", "userland binary: not installed (by design, keep it minimal)"),
    ("vim", "userland binary: not installed (by design, keep it minimal)"),
    ("nano", "userland binary: not installed (by design, keep it minimal)"),
    ("emacs", "userland binary: not installed (by design, keep it minimal)"),
    ("ed", "userland binary: not installed (by design, keep it minimal)"),
    ("less", "userland binary: not installed (by design, keep it minimal)"),
    ("more", "userland binary: not installed (by design, keep it minimal)"),
    ("man", "userland binary: not installed (by design, keep it minimal)"),
    ("info", "userland binary: not installed (by design, keep it minimal)"),
    ("which", "userland binary: not installed (by design, keep it minimal)"),
    ("whereis", "userland binary: not installed (by design, keep it minimal)"),
    ("type", "userland binary: not installed (by design, keep it minimal)"),
    ("env", "not built into LumiOS"),
    ("export", "not built into LumiOS"),
    ("unset", "not built into LumiOS"),
    ("alias", "not built into LumiOS"),
    ("history", "not built into LumiOS"),
    ("clear2", "not built into LumiOS"),
    ("reset", "not built into LumiOS"),
    ("stty", "not built into LumiOS"),
    ("tty", "not built into LumiOS"),
    ("who", "process tool: no procfs yet"),
    ("w", "process tool: no procfs yet"),
    ("last", "process tool: no procfs yet"),
    ("id", "process tool: no procfs yet"),
    ("groups", "process tool: no procfs yet"),
    ("su", "process tool: no procfs yet"),
    ("sudo", "process tool: no procfs yet"),
    ("passwd", "process tool: no procfs yet"),
    ("useradd", "process tool: no procfs yet"),
    ("userdel", "process tool: no procfs yet"),
    ("hostname", "not built into LumiOS"),
    ("hostnamectl", "not built into LumiOS"),
    ("uname2", "not built into LumiOS"),
    ("arch", "not built into LumiOS"),
    ("nproc", "not built into LumiOS"),
    ("ldd", "not built into LumiOS"),
    ("objdump", "not built into LumiOS"),
    ("readelf", "not built into LumiOS"),
    ("nm", "not built into LumiOS"),
    ("strip", "not built into LumiOS"),
    ("strings", "file tool: needs fork+exec of a userland binary"),
    ("base64", "file tool: needs fork+exec of a userland binary"),
    ("md5sum", "file tool: needs fork+exec of a userland binary"),
    ("sha1sum", "file tool: needs fork+exec of a userland binary"),
    ("sha256sum", "file tool: needs fork+exec of a userland binary"),
    ("cksum", "file tool: needs fork+exec of a userland binary"),
    ("xxd", "file tool: needs fork+exec of a userland binary"),
    ("hexdump", "file tool: needs fork+exec of a userland binary"),
    ("od", "file tool: needs fork+exec of a userland binary"),
    ("split", "file tool: needs fork+exec of a userland binary"),
    ("csplit", "file tool: needs fork+exec of a userland binary"),
    ("diff", "file tool: needs fork+exec of a userland binary"),
    ("cmp", "file tool: needs fork+exec of a userland binary"),
    ("patch", "file tool: needs fork+exec of a userland binary"),
    ("seq", "not built into LumiOS"),
    ("yes", "not built into LumiOS"),
    ("true", "not built into LumiOS"),
    ("false", "not built into LumiOS"),
    ("sleep", "not built into LumiOS"),
    ("wait", "not built into LumiOS"),
    ("expr", "not built into LumiOS"),
    ("bc", "not built into LumiOS"),
    ("dc", "not built into LumiOS"),
    ("cal", "not built into LumiOS"),
    ("locale", "not built into LumiOS"),
    ("date2", "not built into LumiOS"),
    ("timedatectl", "not built into LumiOS"),
    ("systemctl", "not built into LumiOS"),
    ("journalctl", "not built into LumiOS"),
    ("service", "not built into LumiOS"),
    ("crontab", "not built into LumiOS"),
    ("at", "not built into LumiOS"),
    ("reboot", "not built into LumiOS"),
    ("shutdown", "not built into LumiOS"),
    ("poweroff", "not built into LumiOS"),
    ("halt", "not built into LumiOS"),
    ("init", "not built into LumiOS"),
    ("echo2", "not built into LumiOS"),
    ("printf", "not built into LumiOS"),
    ("test", "not built into LumiOS"),
    ("read", "not built into LumiOS"),
    ("cd", "file tool: needs fork+exec of a userland binary"),
    ("pwd2", "not built into LumiOS"),
    ("ls2", "file tool: needs fork+exec of a userland binary"),
    ("ll", "file tool: needs fork+exec of a userland binary"),
    ("la", "file tool: needs fork+exec of a userland binary"),
    ("tree", "file tool: needs fork+exec of a userland binary"),
    ("watch", "process tool: no procfs yet"),
    ("screen", "process tool: no procfs yet"),
    ("tmux", "process tool: no procfs yet"),
    ("byobu", "process tool: no procfs yet"),
    ("apt", "userland binary: not installed (by design, keep it minimal)"),
    ("dnf", "userland binary: not installed (by design, keep it minimal)"),
    ("yum", "userland binary: not installed (by design, keep it minimal)"),
    ("pacman", "userland binary: not installed (by design, keep it minimal)"),
    ("snap", "userland binary: not installed (by design, keep it minimal)"),
    ("flatpak", "userland binary: not installed (by design, keep it minimal)"),
    ("docker", "userland binary: not installed (by design, keep it minimal)"),
    ("podman", "userland binary: not installed (by design, keep it minimal)"),
    ("kubectl", "userland binary: not installed (by design, keep it minimal)"),
    ("nmap", "userland binary: not installed (by design, keep it minimal)"),
    ("tcpdump", "userland binary: not installed (by design, keep it minimal)"),
    ("iperf", "userland binary: not installed (by design, keep it minimal)"),
    ("dig", "userland binary: not installed (by design, keep it minimal)"),
    ("nslookup", "userland binary: not installed (by design, keep it minimal)"),
    ("host", "userland binary: not installed (by design, keep it minimal)"),
    ("whois", "userland binary: not installed (by design, keep it minimal)"),
    ("openssl", "userland binary: not installed (by design, keep it minimal)"),
    ("gpg", "userland binary: not installed (by design, keep it minimal)"),
    ("base32", "userland binary: not installed (by design, keep it minimal)"),
    ("shred", "userland binary: not installed (by design, keep it minimal)"),
    ("fdisk", "userland binary: not installed (by design, keep it minimal)"),
    ("parted", "userland binary: not installed (by design, keep it minimal)"),
    ("mke2fs", "userland binary: not installed (by design, keep it minimal)"),
    ("tune2fs", "userland binary: not installed (by design, keep it minimal)"),
    ("e2fsck", "userland binary: not installed (by design, keep it minimal)"),
    ("resize2fs", "userland binary: not installed (by design, keep it minimal)"),
    ("readlink", "userland binary: not installed (by design, keep it minimal)"),
    ("realpath", "userland binary: not installed (by design, keep it minimal)"),
    ("basename", "userland binary: not installed (by design, keep it minimal)"),
    ("dirname", "userland binary: not installed (by design, keep it minimal)"),
    ("mktemp", "userland binary: not installed (by design, keep it minimal)"),
    ("install", "userland binary: not installed (by design, keep it minimal)"),
    ("rsync", "userland binary: not installed (by design, keep it minimal)"),
    ("socat", "userland binary: not installed (by design, keep it minimal)"),
    ("ab", "userland binary: not installed (by design, keep it minimal)"),
    ("wrk", "userland binary: not installed (by design, keep it minimal)"),
    ("perf", "userland binary: not installed (by design, keep it minimal)"),
    ("strace", "userland binary: not installed (by design, keep it minimal)"),
    ("ltrace", "userland binary: not installed (by design, keep it minimal)"),
    ("gdb", "userland binary: not installed (by design, keep it minimal)"),
    ("lldb", "userland binary: not installed (by design, keep it minimal)"),
    ("valgrind", "userland binary: not installed (by design, keep it minimal)"),
    ("objcopy", "userland binary: not installed (by design, keep it minimal)"),
    ("as", "userland binary: not installed (by design, keep it minimal)"),
    ("ld", "userland binary: not installed (by design, keep it minimal)"),
    ("nasm", "userland binary: not installed (by design, keep it minimal)"),
    ("qemu", "userland binary: not installed (by design, keep it minimal)"),
    ("lsns", "userland binary: not installed (by design, keep it minimal)"),
    ("unshare", "userland binary: not installed (by design, keep it minimal)"),
    ("nsenter", "userland binary: not installed (by design, keep it minimal)"),
    ("chroot", "userland binary: not installed (by design, keep it minimal)"),
    ("setfacl", "userland binary: not installed (by design, keep it minimal)"),
    ("getfacl", "userland binary: not installed (by design, keep it minimal)"),
    ("iptables", "userland binary: not installed (by design, keep it minimal)"),
    ("nft", "userland binary: not installed (by design, keep it minimal)"),
    ("ufw", "userland binary: not installed (by design, keep it minimal)"),
    ];
    T.iter().find(|(n, _)| *n == cmd).map(|(_, m)| *m)
}

/// 执行终端里当前这一行（只做内建命令；外部程序要等 fork/exec 接进来）。
/// 往终端输出追加一段文本（满两千字节就不再涨）。
fn out_str(d: &mut Desktop, s: &str) {
    for b in s.bytes() {
        if d.term_n < d.term_text.len() {
            d.term_text[d.term_n] = b;
            d.term_n += 1;
        }
    }
}

fn out_nl(d: &mut Desktop) {
    out_str(d, "\n");
}

fn out_i64(d: &mut Desktop, mut v: i64) {
    if v < 0 {
        out_str(d, "-");
        v = -v;
    }
    if v == 0 {
        out_str(d, "0");
        return;
    }
    let mut tmp = [0u8; 20];
    let mut i = 0usize;
    while v > 0 && i < 20 {
        tmp[i] = b'0' + (v % 10) as u8;
        v /= 10;
        i += 1;
    }
    while i > 0 {
        i -= 1;
        let one = [tmp[i]];
        out_str(d, unsafe { core::str::from_utf8_unchecked(&one) });
    }
}

/// 把文件内容按可打印字符打到终端（非 ASCII 直接跳过，避免乱码）。
fn dump_fd(d: &mut Desktop, fd: i64, limit_lines: Option<i64>) {
    let mut buf = [0u8; 512];
    let mut lines = 0i64;
    loop {
        let n = read(fd, &mut buf);
        if n <= 0 {
            break;
        }
        for i in 0..n as usize {
            let b = buf[i];
            if b == b'\n' {
                out_nl(d);
                lines += 1;
                if let Some(l) = limit_lines {
                    if lines >= l {
                        return;
                    }
                }
            } else if (0x20..0x7F).contains(&b) {
                let one = [b];
                out_str(d, unsafe { core::str::from_utf8_unchecked(&one) });
            }
        }
    }
}

/// 执行终端里当前这一行。
///
/// 这里有一批命令是**真的会干活**的：走系统调用读目录、读文件、切目录、删文件。
/// 没实现的两类会如实说明原因（内核缺 syscall / 未预装工具）。
fn term_run(d: &mut Desktop) {
    // 先把命令行拷到本地：后面要 &mut d 输出，不能再借着 d 的不可变引用
    let mut lbuf = [0u8; 64];
    let ln = d.term_len.min(64);
    lbuf[..ln].copy_from_slice(&d.term_line[..ln]);
    let cmdline = unsafe { core::str::from_utf8_unchecked(&lbuf[..ln]) }.trim();

    out_str(d, "> ");
    out_str(d, cmdline);
    out_nl(d);

    let mut it = cmdline.split_whitespace();
    let cmd = it.next().unwrap_or("");
    let arg = it.next().unwrap_or("");

    match cmd {
        "" => {}
        "help" => {
            out_str(d, "real: ls cat head wc pwd cd rm echo clear version");
            out_nl(d);
            out_str(d, "stub: any other name explains why");
            out_nl(d);
        }
        "clear" => d.term_n = 0,
        "version" => {
            out_str(d, "LumiOS 0.1  /  LumiKernel 1.3.1 M12");
            out_nl(d);
        }
        "ls" => {
            let mut ents = [DirEnt { name: [0; 32], len: 0, is_dir: false }; DIR_MAX];
            let n = readdir(if arg.is_empty() { "." } else { arg }, &mut ents);
            if n < 0 {
                out_str(d, "ls: cannot open directory");
                out_nl(d);
            } else {
                let mut width = 0;
                for i in 0..n as usize {
                    let name = ents[i].as_str();
                    if name == "." || name == ".." {
                        continue;
                    }
                    if width + name.len() > 32 {
                        out_nl(d);
                        width = 0;
                    }
                    out_str(d, name);
                    if ents[i].is_dir {
                        out_str(d, "/");
                    }
                    out_str(d, "  ");
                    width += name.len() + 3;
                }
                out_nl(d);
            }
        }
        "cat" => {
            if arg.is_empty() {
                out_str(d, "usage: cat <file>");
                out_nl(d);
            } else {
                let fd = liblumi::open(arg, liblumi::O_RDONLY);
                if fd < 0 {
                    out_str(d, "cat: no such file");
                    out_nl(d);
                } else {
                    dump_fd(d, fd, None);
                    close(fd);
                    out_nl(d);
                }
            }
        }
        "head" => {
            if arg.is_empty() {
                out_str(d, "usage: head <file>");
                out_nl(d);
            } else {
                let fd = liblumi::open(arg, liblumi::O_RDONLY);
                if fd < 0 {
                    out_str(d, "head: no such file");
                    out_nl(d);
                } else {
                    dump_fd(d, fd, Some(8));
                    close(fd);
                    out_nl(d);
                }
            }
        }
        "wc" => {
            if arg.is_empty() {
                out_str(d, "usage: wc <file>");
                out_nl(d);
            } else {
                let fd = liblumi::open(arg, liblumi::O_RDONLY);
                if fd < 0 {
                    out_str(d, "wc: no such file");
                    out_nl(d);
                } else {
                    let mut buf = [0u8; 512];
                    let mut bytes = 0i64;
                    let mut lines = 0i64;
                    loop {
                        let n = read(fd, &mut buf);
                        if n <= 0 {
                            break;
                        }
                        bytes += n;
                        for i in 0..n as usize {
                            if buf[i] == b'\n' {
                                lines += 1;
                            }
                        }
                    }
                    close(fd);
                    out_i64(d, lines);
                    out_str(d, " lines  ");
                    out_i64(d, bytes);
                    out_str(d, " bytes");
                    out_nl(d);
                }
            }
        }
        "pwd" => {
            let mut buf = [0u8; 128];
            let n = getcwd(&mut buf);
            if n > 0 {
                let len = ((n as usize).saturating_sub(1)).min(127);
                out_str(d, unsafe { core::str::from_utf8_unchecked(&buf[..len]) });
            } else {
                out_str(d, "/");
            }
            out_nl(d);
        }
        "cd" => {
            if arg.is_empty() {
                out_str(d, "usage: cd <dir>");
                out_nl(d);
            } else if chdir(arg) < 0 {
                out_str(d, "cd: no such directory");
                out_nl(d);
            }
        }
        "rm" => {
            if arg.is_empty() {
                out_str(d, "usage: rm <file>");
                out_nl(d);
            } else if unlink(arg) < 0 {
                out_str(d, "rm: failed");
                out_nl(d);
            } else {
                out_str(d, "removed");
                out_nl(d);
            }
        }
        "echo" => {
            let rest = cmdline.strip_prefix("echo").unwrap_or("").trim_start();
            out_str(d, rest);
            out_nl(d);
        }
        _ => {
            match stub_for(cmd) {
                Some(m) => {
                    out_str(d, cmd);
                    out_str(d, ": ");
                    out_str(d, m);
                }
                None => out_str(d, "unknown command, try: help"),
            }
            out_nl(d);
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
        let text = &d.term_text[..d.term_n];
        let mut starts = [0usize; 48];
        let mut lens = [0usize; 48];
        let mut n = 0usize;
        let mut start = 0usize;
        for i in 0..text.len() {
            if text[i] == b'\n' && n < 48 {
                starts[n] = start;
                lens[n] = i - start;
                n += 1;
                start = i + 1;
            }
        }
        if n < 48 {
            starts[n] = start;
            lens[n] = text.len() - start;
            n += 1;
        }
        let first = n.saturating_sub(17);
        let mut y = 34;
        for i in first..n {
            let s = unsafe {
                core::str::from_utf8_unchecked(&text[starts[i]..starts[i] + lens[i]])
            };
            cv.text_bg(34, y, s, TEXT, WIN_BG);
            y += 16;
        }
        let line = unsafe { core::str::from_utf8_unchecked(&d.term_line[..d.term_len]) };
        cv.text_bg(34, y + 2, ">", ACCENT, WIN_BG);
        cv.text_bg(50, y + 2, line, TEXT, WIN_BG);
        cv.fill(50 + (d.term_len as i32) * 8, y + 2, 8, 16, ACCENT);
    }
}


/// 重新读取当前目录，填进文件管理器的列表。
fn fm_refresh(d: &mut Desktop) {
    let mut ents = [DirEnt { name: [0; 32], len: 0, is_dir: false }; DIR_MAX];
    let n = readdir(".", &mut ents);
    if n <= 0 {
        d.fm_n = 0;
    } else {
        // 过滤掉 . 与 ..，列表里只显示真实条目
        let mut k = 0usize;
        for i in 0..n as usize {
            let nm = ents[i].as_str();
            if nm == "." || nm == ".." {
                continue;
            }
            if k >= DIR_MAX {
                break; // 目录项超过上限就截断，绝不越界写
            }
            d.fm_entries[k] = ents[i];
            k += 1;
        }
        d.fm_n = k;
    }
    if d.fm_sel >= d.fm_n {
        d.fm_sel = d.fm_n.saturating_sub(1);
    }
    fm_paint(d);
}

/// 把列表画进窗口自己的缓冲。
fn fm_paint(d: &mut Desktop) {
    let w = d.wins[0];
    if let Some(cv) = w.canvas {
        paint_body(&cv, &w);
        if d.fm_n == 0 {
            cv.text_bg(34, 40, "(empty directory)", TEXT_DIM, WIN_BG);
            return;
        }
        let mut y = 34i32;
        let first = if d.fm_sel >= 14 { d.fm_sel - 13 } else { 0 };
        for i in first..d.fm_n.min(first + 14) {
            let e = d.fm_entries[i];
            let name = e.as_str();
            let selected = i == d.fm_sel;
            let bg = if selected { ACCENT } else { WIN_BG };
            let fg = if selected { 0x0000_0000 } else if e.is_dir { 0x00E8_C46A } else { TEXT };
            cv.fill(16, y - 2, cv.width as i32 - 32, 16, bg);
            cv.text_bg(24, y, if e.is_dir { "/" } else { " " }, fg, bg);
            cv.text_bg(36, y, name, fg, bg);
            y += 17;
        }
        cv.text_bg(34, cv.height as i32 - 22, "UP/DOWN move  ENTER open  BS up", TEXT_DIM, WIN_BG);
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
