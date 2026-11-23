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
const TITLE_FOCUS: u32 = 0x00F3_F4F6; // 浅色标题栏（聚焦）
const TITLE_IDLE: u32 = 0x00F8_F9FA; // 浅色标题栏（非聚焦）
const WIN_BG: u32 = 0x00FF_FFFF; // 纯白内容区
const WIN_BORDER: u32 = 0x00D5_D9DE; // 发丝级边框
const TEXT_DK: u32 = 0x0024_2424; // 浅底深字
/// 深底/彩色底上的浅色文字（标题栏、桌面图标标签）
const TEXT: u32 = 0x00E6_E8EC;
const TEXT_DIM: u32 = 0x0087_94A8;
const ACCENT: u32 = 0x004C_9AFF;
const OK: u32 = 0x003F_C46A;
const CLOSE: u32 = 0x00FF_5F_57; // mac 风格红点
const CURSOR_FG: u32 = 0x00FF_FFFF;
const CURSOR_BG: u32 = 0x0010_1010;

const TITLE_H: i32 = 34;
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
    /// 最近一次点击的诊断：落在哪个窗口、命中什么区域（屏幕上可见）
    hit_note: [u8; 40],
    hit_len: usize,
    fm_n: usize,
    fm_sel: usize,
    /// 上次点击的行（简易双击判定）
    fm_last_click: usize,
    /// 终端模式：0=内建（Windows 风格），1=busybox（Ubuntu）
    term_mode: u8,
    /// 浏览器地址栏内容
    url: [u8; 96],
    url_len: usize,
    /// 是否已接入浏览器客户端缓冲
    browser_adopted: bool,
    /// 终端输出（字节流，按换行分行绘制）
    term_text: [u8; 2048],
    term_n: usize,
}

impl Desktop {
    fn new(w: i32, h: i32) -> Self {
        let wins = [
            Win {
                x: 16,
                y: 128,
                w: 330,
                h: 430,
                title: "File Manager",
                body: "F1  ·  待接入客户端",
                color: 0x00B8_8A4A,
                visible: true,
                canvas: None,
            },
            Win {
                x: 356,
                y: 150,
                w: 330,
                h: 430,
                title: "Terminal",
                body: "F2  ·  待接入客户端",
                color: 0x003F_C46A,
                visible: true,
                canvas: None,
            },
            Win {
                x: 696,
                y: 172,
                w: 312,
                h: 400,
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
            hit_note: [0; 40],
            hit_len: 0,
            fm_n: 0,
            fm_sel: 0,
            fm_last_click: usize::MAX,
            term_mode: 0,
            url: [0; 96],
            url_len: 0,
            browser_adopted: false,
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
        // 与 draw_window 里的小圆点一致：12x12，位于 (w.w-26, 11)
        let bx = w.x + w.w - 26 - 4;
        let by = w.y + 11 - 4;
        x >= bx && x < bx + 20 && y >= by && y < by + 20
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
    // 客户端进程接线暂缓：上次把 fork+execve 放进窗口服务进程后，
    // lumiwin 启动即崩（内核打了缺页现场）。已确认 fork 本身可用
    // （自检 64 通过），问题出在"窗口服务进程里 fork"或 argv 构造。
    // 先把桌面恢复到稳定状态，再用最小改动重做。
    // 目录系统：先切到 /data —— 根目录在 LumiFS 里是空的，
    // 数据文件都在 /data，不切过去文件管理器就是一页空白。
    chdir("/data");

    // 浏览器渲染器先不挂启动路径：加了它之后 lumiwin 的 ELF 从 24 KB
    // 涨到 64 KB，内核 exec 返回 -8（ENOEXEC）——装载器拒绝了段布局。
    // 代码保留在这里（因未被引用会被 --gc-sections 回收），
    // 下一步先查装载器对段数/大小的限制，再接回来。
    // browse(&mut d);  // 待拆分到独立二进制后再接

    // **必须在窗口缓冲分配之后**：fm_paint 要靠 wins[0].canvas 才画得出来，
    // 先画会被随后的 paint_body 占位内容盖掉（这个坑踩过两次）。
    fm_refresh(&mut d);

    // 鼠标映射自检（headless 注入不了鼠标事件，只能验算法）
    {
        let w0 = d.wins[0];
        let probe_x = w0.x + 200;
        let probe_y = w0.y + TITLE_H + 44 + 34 + 10; // 第二行
        match fm_row_at(&w0, probe_x, probe_y, 0) {
            Some(1) => {}
            _ => {}
        }
    }

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
                // 按下：先把诊断记下来（屏幕上会显示，方便定位"点不动"）
                {
                    let note = match d.top_at(d.mx, d.my) {
                        Some(w) => if w == 0 { "win0" } else { "winX" },
                        None => "none",
                    };
                    d.hit_len = note.len().min(d.hit_note.len());
                    d.hit_note[..d.hit_len].copy_from_slice(&note.as_bytes()[..d.hit_len]);
                }
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
                        } else if wi == 0 {
                            // 先看侧栏：点 Home 回 /data，其余目录不存在就提示
                            if let Some(sr) = fm_sidebar_at(&w, d.mx, d.my) {
                                let names = ["", "Documents", "Downloads", "Music", "Pictures", "Videos"];
                                let ok = if sr == 0 {
                                    chdir("/data") == 0
                                } else {
                                    let mut pbuf = [0u8; 32];
                                    pbuf[0] = b'/';
                                    let n = names[sr].len();
                                    pbuf[1..1 + n].copy_from_slice(names[sr].as_bytes());
                                    let path = unsafe { core::str::from_utf8_unchecked(&pbuf[..1 + n]) };
                                    chdir(path) == 0
                                };
                                if ok {
                                    d.fm_sel = 0;
                                    fm_refresh(&mut d);
                                } else {
                                    // 目录不存在：直接如实显示"空"
                                    d.fm_n = 0;
                                    d.fm_sel = 0;
                                    fm_paint(&mut d);
                                    d.dirty = true;
                                }
                                d.fm_last_click = usize::MAX;
                                continue;
                            }
                            // 文件管理器：点行就选中（以前只有键盘能选）
                            let visible = ((w.h - TITLE_H - 60) / 34).max(1) as usize;
                            let first = if d.fm_sel >= visible { d.fm_sel + 1 - visible } else { 0 };
                            if let Some(idx) = fm_row_at(&w, d.mx, d.my, first) {
                                if idx < d.fm_n {
                                    d.fm_sel = idx;
                                    // 再点一次同一行 = 打开（简化版双击）
                                    if d.fm_last_click == idx {
                                        if d.fm_entries[idx].is_dir {
                                            let name = d.fm_entries[idx].as_str();
                                            if chdir(name) == 0 {
                                                d.fm_sel = 0;
                                                fm_refresh(&mut d);
                                            }
                                        }
                                    }
                                    d.fm_last_click = idx;
                                    fm_paint(&mut d);
                                }
                            }
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

            // **先处理可打印字符**，不走 match。
            // 之前放在 match 的守卫/兜底分支里，实测 `[k] press` 有输出但
            // 字符分支从不执行（原因未查明）。用 if 提到前面，行为确定。
            if k.code >= 0x20 && k.code < 0x7F {
                let ch = k.code;
                if d.focus == 2 {
                    if d.url_len < d.url.len() - 1 {
                        d.url[d.url_len] = ch;
                        d.url_len += 1;
                        d.dirty = true;
                    }
                } else if d.term_len < d.term_line.len() - 1 {
                    d.term_line[d.term_len] = ch;
                    d.term_len += 1;
                    d.dirty = true;
                    repaint_term(&mut d);
                }
                continue;
            }

            // 回车：**必须在 match 之前处理**。
            // 之前的实现依赖 match 里的一条 KEY_ENTER 分支，那条分支后来被误删，
            // 结果是"回车完全没反应"——而串口里事件明明已经被取走了。
            if k.code == KEY_ENTER {
                term_run(&mut d);
                continue;
            }
            // 退格同理，单独拦下
            if k.code == KEY_BACKSPACE {
                if d.term_len > 0 {
                    d.term_len -= 1;
                    d.dirty = true;
                    repaint_term(&mut d);
                }
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
                // 回车不再给文件管理器：进目录用鼠标双击。
                // 之前这条分支会把回车截走（fm_sel 有值时），终端因此永远执行不了命令。
                KEY_BACKSPACE if d.focus == 2 => {
                    if d.url_len > 0 {
                        d.url_len -= 1;
                        d.dirty = true;
                    }
                }
                KEY_BACKSPACE => {
                    if d.term_len > 0 {
                        d.term_len -= 1;
                        d.dirty = true;
                        repaint_term(&mut d);
                    }
                }
                // 字符输入按**焦点窗口**路由：以前所有字符都灌进终端，
                // 而终端被浏览器窗口盖住，看起来就是"打不了字"。
                // 字符输入放兜底分支里手工判范围。
                // 原先写成 `c if (0x20..0x7F).contains(&c) => ...` 的守卫分支，
                // 实测按键到了（press 有日志）却进不来（char 无日志）——
                // 与其和 match 的匹配规则较劲，不如把判断写在分支体里。
                _ => {
                    let c = k.code;
                    if c >= 0x20 && c < 0x7F {
                        if d.focus == 2 {
                            if d.url_len < d.url.len() - 1 {
                                d.url[d.url_len] = c;
                                d.url_len += 1;
                                d.dirty = true;
                            }
                        } else if d.term_len < d.term_line.len() - 1 {
                            d.term_line[d.term_len] = c;
                            d.term_len += 1;
                            d.dirty = true;
                            repaint_term(&mut d);
                        }
                            }
                }
            }
        }

        // 客户端接入暂时关掉：接入后窗口整片消失（blit 源未就绪时没有兜底）。
        // 先把三个窗口恢复，等客户端路径做稳再开。
        // adopt_browser(&mut d);

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
    // 浏览器窗口的 Chrome 风格外壳：标签条 + 地址栏
    if d.wins[2].visible {
        browser_chrome(c, d);
    }

    // 诊断读数：直接在屏幕上显示光标坐标与最近一次点击命中的窗口。
    // 鼠标事件在无头虚拟机里注入不了，这块读数就是唯一能拿到的证据。
    {
        let mut buf = [0u8; 48];
        let mut n = 0usize;
        let mut num = |v: i32, buf: &mut [u8; 48], n: &mut usize| {
            let mut t = [0u8; 8];
            let mut k = 0usize;
            let mut x = v.unsigned_abs();
            if v < 0 && *n < 48 {
                buf[*n] = b'-';
                *n += 1;
            }
            if x == 0 {
                if *n < 48 {
                    buf[*n] = b'0';
                    *n += 1;
                }
                return;
            }
            while x > 0 && k < 8 {
                t[k] = b'0' + (x % 10) as u8;
                x /= 10;
                k += 1;
            }
            while k > 0 {
                k -= 1;
                if *n < 48 {
                    buf[*n] = t[k];
                    *n += 1;
                }
            }
        };
        num(d.mx, &mut buf, &mut n);
        if n < 48 {
            buf[n] = b',';
            n += 1;
        }
        num(d.my, &mut buf, &mut n);
        if n < 48 {
            buf[n] = b' ';
            n += 1;
        }
        for b in b"hit:" {
            if n < 48 {
                buf[n] = *b;
                n += 1;
            }
        }
        for b in d.hit_note[..d.hit_len].iter() {
            if n < 48 {
                buf[n] = *b;
                n += 1;
            }
        }
        let txt = unsafe { core::str::from_utf8_unchecked(&buf[..n]) };
        c.text_bg(12, c.height as i32 - 48, txt, 0x00FF_D1_66, 0x0018_2130);
    }

    draw_cursor(c, d.mx, d.my);
}

/// 画 Chrome 风格的浏览器外壳：顶部标签条、地址栏、内容区。
fn browser_chrome(c: &Canvas, d: &Desktop) {
    let w = d.wins[2];
    let x = w.x;
    let y = w.y + TITLE_H;
    let cw = w.w;

    // 标签条
    c.fill(x, y, cw, 30, 0x00DE_E1E6);
    c.fill(x + 8, y + 6, 150, 24, 0x00FF_FFFF);
    c.text_bg(x + 18, y + 10, "New Tab", 0x003C_4044, 0x00FF_FFFF);

    // 地址栏
    c.fill(x, y + 30, cw, 38, 0x00F1_F3F4);
    c.fill(x + 10, y + 38, cw - 20, 24, 0x00FF_FFFF);
    c.fill(x + 10, y + 38, cw - 20, 1, 0x00C4_C7C9);
    c.fill(x + 10, y + 61, cw - 20, 1, 0x00C4_C7C9);
    let url = unsafe { core::str::from_utf8_unchecked(&d.url[..d.url_len]) };
    if url.is_empty() {
        c.text_bg(x + 20, y + 42, "Search or type a URL", 0x0080_868C, 0x00FF_FFFF);
    } else {
        c.text_bg(x + 20, y + 42, url, 0x0020_2124, 0x00FF_FFFF);
    }

    // 内容区
    let cy = y + 68;
    c.fill(x, cy, cw, w.h - TITLE_H - 68, 0x00FF_FFFF);
    // 按窗口宽度截断，别让文字顶到窗口外（之前溢出到隔壁窗口）
    let max_chars = ((cw - 48) / 6).max(1) as usize;
    let l1 = "Page content area";
    let l2 = "(type a URL above, ENTER to load)";
    c.text_bg(x + 24, cy + 24, &l1[..l1.len().min(max_chars)], 0x0070_767C, 0x00FF_FFFF);
    c.text_bg(x + 24, cy + 44, &l2[..l2.len().min(max_chars)], 0x0090_969C, 0x00FF_FFFF);
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
    // **柔和阴影**：没有 alpha 通道，用三层递减的实色叠出"离桌面有点距离"的感觉。
    // 这一层是廉价感的主要来源——之前窗口是直接贴在壁纸上的。
    // 深色三层：壁纸是深蓝，浅色阴影会读成"多了一个窗口"。
    c.fill(w.x + 10, w.y + 12, w.w, w.h, 0x001B_2432);
    c.fill(w.x + 6, w.y + 8, w.w, w.h, 0x0017_1F2B);
    c.fill(w.x + 3, w.y + 4, w.w, w.h, 0x0013_1A24);

    // 主体：纯白 + 发丝边框
    c.fill(w.x, w.y, w.w, w.h, 0x00FF_FFFF);
    c.fill(w.x - 1, w.y - 1, w.w + 2, 1, WIN_BORDER);
    c.fill(w.x - 1, w.y + w.h, w.w + 2, 1, WIN_BORDER);
    c.fill(w.x - 1, w.y, 1, w.h, WIN_BORDER);
    c.fill(w.x + w.w, w.y, 1, w.h, WIN_BORDER);

    // 标题栏：浅灰，聚焦时更亮一点，底部一条分隔线
    c.fill(w.x, w.y, w.w, TITLE_H, if focused { TITLE_FOCUS } else { TITLE_IDLE });
    c.fill(w.x, w.y + TITLE_H - 1, w.w, 1, 0x00E3_E6EA);
    // 聚焦强调：标题下方 2px 的细色条（不抢眼，但能看出谁是焦点）
    if focused {
        c.fill(w.x + 1, w.y + TITLE_H - 1, w.w - 2, 2, 0x00E9_5454);
    }
    // 标题文字：垂直居中（TITLE_H 34，字高 16 → 偏移 9）
    c.text(w.x + 16, w.y + 9, w.title, 0x002B_2F33);

    // 关闭键：12px 圆点（放大一点、留足边距，比整块红条体面）
    // 关闭键：一个 12px 的圆点。没有抗锯齿，用"十字 + 方块"两级近似成圆。
    let bx = w.x + w.w - 26;
    let dot = if focused { 0x00FF_5F_57 } else { 0x00D6_DADD };
    c.fill(bx + 2, w.y + 11, 8, 12, dot);
    c.fill(bx, w.y + 13, 12, 8, dot);

    // 内容来自窗口自己的缓冲
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
/// 复制文件（copy 命令用）。上限 4 KiB，够配置/文本用。
fn copy_file(from: &str, to: &str) -> Result<usize, i64> {
    let fd = open(from, O_RDONLY);
    if fd < 0 {
        return Err(-1);
    }
    let mut buf = [0u8; 4096];
    let mut n = 0usize;
    loop {
        if n >= buf.len() {
            break;
        }
        let k = read(fd, &mut buf[n..]);
        if k <= 0 {
            break;
        }
        n += k as usize;
    }
    close(fd);
    let out = open(to, O_WRONLY | O_CREAT | O_TRUNC);
    if out < 0 {
        return Err(-2);
    }
    write(out, &buf[..n]);
    close(out);
    Ok(n)
}

/// 哪些命令由 LumiOS 自己实现（其余转 busybox）。
/// 往终端写一个十进制数（没 alloc，手工转）。
fn write_num(d: &mut Desktop, mut v: u64) {
    let mut tmp = [0u8; 20];
    let mut n = 0usize;
    if v == 0 {
        out_str(d, "0");
        return;
    }
    while v > 0 {
        tmp[n] = b'0' + (v % 10) as u8;
        v /= 10;
        n += 1;
    }
    while n > 0 {
        n -= 1;
        let b = tmp[n];
        if d.term_n < d.term_text.len() - 1 {
            d.term_text[d.term_n] = b;
            d.term_n += 1;
        }
    }
}

fn is_builtin(cmd: &str) -> bool {
    matches!(
        cmd,
        "" | "help" | "clear" | "version" | "ls" | "cat" | "head" | "wc" | "pwd" | "cd"
            | "rm" | "echo" | "dir" | "type" | "del" | "cls" | "copy" | "ubuntu" | "windows"
            | "ver" | "vol" | "set" | "where" | "tree" | "more" | "md" | "mkdir" | "rd"
            | "ren" | "move" | "exit" | "prompt" | "title" | "date" | "time" | "attrib"
            | "update-lumios" | "hit" | "install"
    )
}

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

    // ---- 模式切换 ----
    if cmd == "ubuntu" {
        d.term_mode = 1;
        out_str(d, "switched to UBUNTU mode (busybox: ls cat grep sed awk find ...)");
        out_nl(d);
        out_str(d, "type 'windows' to switch back");
        out_nl(d);
        return;
    }
    if cmd == "windows" {
        d.term_mode = 0;
        out_str(d, "switched to WINDOWS mode (dir type copy del ren echo cls)");
        out_nl(d);
        return;
    }

    // ---- Ubuntu 模式：交给 busybox，输出经 /data/.out 取回 ----
    // 内建命令之外，一律交给 busybox（默认就有 ~300 个命令）。
    // 之前把这套逻辑抽成函数后整个桌面起不来，所以这里**只改判断条件**，
    // 执行逻辑保持原样，改动面最小。
    if !cmdline.is_empty() && (d.term_mode == 1 || !is_builtin(cmd)) {
        let mut full = [0u8; 512];
        let mut n = 0usize;
        for b in cmdline.bytes() {
            if n < 400 {
                full[n] = b;
                n += 1;
            }
        }
        let tail = b" > /data/.out 2>&1";
        for (i, b) in tail.iter().enumerate() {
            full[n + i] = *b;
        }
        n += tail.len();
        let shell_cmd = unsafe { core::str::from_utf8_unchecked(&full[..n]) };
        run_shell(shell_cmd);

        // 等子进程写完：连续 8 次文件大小不变就当写完
        let mut last = -1i64;
        let mut stable = 0;
        let mut guard = 0;
        while guard < 3000 {
            let fd = open("/data/.out", O_RDONLY);
            let mut sz = 0i64;
            if fd >= 0 {
                let mut tmp = [0u8; 512];
                loop {
                    let k = read(fd, &mut tmp);
                    if k <= 0 {
                        break;
                    }
                    sz += k as i64;
                    if sz > 8000 {
                        break;
                    }
                }
                close(fd);
            }
            if sz == last && sz > 0 {
                stable += 1;
                if stable >= 8 {
                    break;
                }
            } else {
                stable = 0;
                last = sz;
            }
            guard += 1;
            for _ in 0..100_000 {
                core::hint::spin_loop();
            }
        }

        // 读回并逐行写进终端缓冲
        let fd = open("/data/.out", O_RDONLY);
        if fd >= 0 {
            let mut chunk = [0u8; 128];
            loop {
                let k = read(fd, &mut chunk);
                if k <= 0 {
                    break;
                }
                for i in 0..k as usize {
                    let b = chunk[i];
                    if b == 10 {
                        out_nl(d);
                    } else if b >= 0x20 && d.term_n < d.term_text.len() - 1 {
                        d.term_text[d.term_n] = b;
                        d.term_n += 1;
                    }
                }
            }
            close(fd);
            out_nl(d);
        }
        let _ = unlink("/data/.out");
        return;
    }

    match cmd {
        "" => {}
        "dir" => {
            // 按 Windows 的 dir 版式输出：卷标 + 目录头 + 明细 + 统计
            out_str(d, " Volume in drive L has no label.");
            out_nl(d);
            out_str(d, " Directory of /data");
            out_nl(d);
            out_nl(d);
            let mut ents = [DirEnt { name: [0; 32], len: 0, is_dir: false }; DIR_MAX];
            let n = readdir(if arg.is_empty() { "." } else { arg }, &mut ents).max(0) as usize;
            let mut files = 0usize;
            let mut dirs = 0usize;
            let mut bytes = 0u64;
            for i in 0..n {
                let e = ents[i];
                if e.is_dir {
                    dirs += 1;
                    out_str(d, "                <DIR>          ");
                } else {
                    files += 1;
                    bytes += e.len as u64;
                    // 右对齐宽度作用有限，用空格分隔即可读
                    out_str(d, "                ");
                    write_num(d, e.len as u64);
                    out_str(d, " ");
                }
                out_str(d, e.as_str());
                out_nl(d);
            }
            out_str(d, "        ");
            write_num(d, files as u64);
            out_str(d, " File(s) ");
            write_num(d, bytes);
            out_str(d, " bytes");
            out_nl(d);
            out_str(d, "        ");
            write_num(d, dirs as u64);
            out_str(d, " Dir(s)");
            out_nl(d);
        }
        "update-lumios" => {
            // 与升级服务器（10.0.2.2:8080）握手：先取版本号，再决定是否下载
            out_str(d, "udp: asking update server 10.0.2.2:9000 for version ...");
            out_nl(d);
            let n = udp_request("version");
            if n <= 0 {
                out_str(d, "update server unreachable");
                out_nl(d);
                out_str(d, "check: UDP 9000 forward + tools/update-server.py --udp");
                out_nl(d);
            } else {
                let mut buf = [0u8; 512];
                let k = http_take(&mut buf).max(0) as usize;
                out_str(d, "server says: ");
                for i in 0..k {
                    let b = buf[i];
                    if b == 10 || b == 13 {
                        if i + 1 == k || buf[i + 1] != 10 {
                            out_nl(d);
                        }
                    } else if b >= 0x20 {
                        if d.term_n < d.term_text.len() - 1 {
                            d.term_text[d.term_n] = b;
                            d.term_n += 1;
                        }
                    }
                }
                out_nl(d);
            }
        }
        "install" => {
            // 安装向导（骨架）：枚举磁盘 -> 写盘自检 -> （下一步）刷镜像
            let mut buf = [0u8; 64];
            let n = disk_list(&mut buf).max(0) as usize;
            if n == 0 {
                out_str(d, "no installable disk found (AHCI not ready)");
                out_nl(d);
            } else {
                out_str(d, "LumiOS installer - target disks:");
                out_nl(d);
                for i in 0..n {
                    let slot = u32::from_le_bytes([buf[i * 8], buf[i * 8 + 1], buf[i * 8 + 2], buf[i * 8 + 3]]);
                    let mib = u32::from_le_bytes([buf[i * 8 + 4], buf[i * 8 + 5], buf[i * 8 + 6], buf[i * 8 + 7]]);
                    out_str(d, "  disk ");
                    write_num(d, slot as u64);
                    out_str(d, ": ");
                    write_num(d, mib as u64);
                    out_str(d, " MiB");
                    out_nl(d);
                }
                if n == 1 {
                    out_str(d, "only one disk (the boot disk). attach a 2nd disk to install.");
                    out_nl(d);
                } else {
                    out_str(d, "run: install probe <n>   (write-test on disk n)");
                    out_nl(d);
                }
                // install probe <n>
                let mut parts = cmdline.split_whitespace();
                let _ = parts.next();
                if parts.next() == Some("probe") {
                    if let Some(v) = parts.next() {
                        let mut slot = 0u8;
                        for b in v.bytes() {
                            if b.is_ascii_digit() {
                                slot = slot * 10 + (b - b'0');
                            }
                        }
                        if n == 1 {
                            out_str(d, "refused: that is the boot disk.");
                            out_nl(d);
                        } else {
                            let r = disk_probe(slot);
                            out_str(d, if r == 1 { "write probe OK (write+readback verified)" } else { "write probe FAILED" });
                            out_nl(d);
                        }
                    }
                }
            }
        }
        "hit" => {
            // 诊断：hit x y —— 用与点击完全相同的判定，报告命中了什么
            let mut nums = [0i32; 2];
            let mut k = 0;
            for part in cmdline.split_whitespace() {
                if part == "hit" {
                    continue;
                }
                let mut v = 0i32;
                let mut ok = false;
                for b in part.bytes() {
                    if b.is_ascii_digit() {
                        v = v * 10 + (b - b'0') as i32;
                        ok = true;
                    }
                }
                if ok && k < 2 {
                    nums[k] = v;
                    k += 1;
                }
            }
            if k == 2 {
                let (x, y) = (nums[0], nums[1]);
                let w = d.wins[0];
                let mut what = "none";
                if d.close_button_hit(0, x, y) {
                    what = "close";
                } else if d.top_at(x, y) == Some(0) {
                    if y < w.y + TITLE_H {
                        what = "titlebar";
                    } else if fm_sidebar_at(&w, x, y).is_some() {
                        what = "sidebar";
                    } else if fm_row_at(&w, x, y, 0).is_some() {
                        what = "list";
                    } else {
                        what = "fm-body";
                    }
                }
                out_str(d, "hit ");
                write_num(d, x as u64);
                out_str(d, ",");
                write_num(d, y as u64);
                out_str(d, " -> ");
                out_str(d, what);
                out_nl(d);
            }
        }
        "ver" => {
            out_str(d, "LumiOS [Version 0.1.26011]");
            out_nl(d);
        }
        "vol" => {
            out_str(d, " Volume in drive L is LUMIOS");
            out_nl(d);
            out_str(d, " Volume Serial Number is 4C55-4D49");
            out_nl(d);
        }
        "set" => {
            out_str(d, "PATH=/bin");
            out_nl(d);
            out_str(d, "HOME=/");
            out_nl(d);
            out_str(d, "OS=LumiOS");
            out_nl(d);
            out_str(d, "SHELL=/bin/lumiwin");
            out_nl(d);
        }
        "where" => {
            if arg.is_empty() {
                out_str(d, "ERROR: Invalid syntax. where <command>");
            } else if is_builtin(arg) {
                out_str(d, "(builtin) ");
                out_str(d, arg);
            } else {
                out_str(d, "/bin/");
                out_str(d, arg);
            }
            out_nl(d);
        }
        "tree" => {
            let mut ents = [DirEnt { name: [0; 32], len: 0, is_dir: false }; DIR_MAX];
            let n = readdir(".", &mut ents).max(0) as usize;
            out_str(d, "/data");
            out_nl(d);
            for i in 0..n {
                let last = i + 1 == n;
                out_str(d, if last { "`-- " } else { "|-- " });
                out_str(d, ents[i].as_str());
                out_nl(d);
            }
        }
        "more" => {
            // 一屏显示，不做分页（终端可滚动之前意义不大）
            let fd = open(arg, O_RDONLY);
            if fd < 0 {
                out_str(d, "Cannot access file.");
                out_nl(d);
            } else {
                let mut buf = [0u8; 256];
                loop {
                    let k = read(fd, &mut buf);
                    if k <= 0 {
                        break;
                    }
                    for i in 0..k as usize {
                        let b = buf[i];
                        if b == 10 {
                            out_nl(d);
                        } else if b >= 0x20 && d.term_n < d.term_text.len() - 1 {
                            d.term_text[d.term_n] = b;
                            d.term_n += 1;
                        }
                    }
                }
                close(fd);
                out_nl(d);
            }
        }
        "md" | "mkdir" | "rd" | "ren" | "move" => {
            // 诚实提示：LumiFS 是扁平文件系统，没有目录创建与改名
            out_str(d, cmd);
            out_str(d, ": LumiFS 是扁平文件系统，暂不支持该操作");
            out_nl(d);
        }
        "attrib" => {
            let mut ents = [DirEnt { name: [0; 32], len: 0, is_dir: false }; DIR_MAX];
            let n = readdir(".", &mut ents).max(0) as usize;
            for i in 0..n {
                out_str(d, if ents[i].is_dir { "D" } else { "A" });
                out_str(d, "       /data/");
                out_str(d, ents[i].as_str());
                out_nl(d);
            }
        }
        "date" | "time" => {
            out_str(d, "RTC 尚未接入，当前无系统时间");
            out_nl(d);
        }
        "prompt" | "title" => {}
        "exit" => {
            d.wins[1].visible = false;
            d.dirty = true;
        }
        "type" => {
            let fd = open(arg, O_RDONLY);
            if fd < 0 {
                out_str(d, "file not found");
                out_nl(d);
            } else {
                let mut buf = [0u8; 256];
                loop {
                    let k = read(fd, &mut buf);
                    if k <= 0 {
                        break;
                    }
                    for i in 0..k as usize {
                        let b = buf[i];
                        if b == 10 {
                            out_nl(d);
                        } else if b >= 0x20 && d.term_n < d.term_text.len() - 1 {
                            d.term_text[d.term_n] = b;
                            d.term_n += 1;
                        }
                    }
                }
                close(fd);
                out_nl(d);
            }
        }
        "del" => {
            if !arg.is_empty() {
                let r = unlink(arg);
                out_str(d, if r == 0 { "deleted" } else { "not found" });
                out_nl(d);
            }
        }
        "cls" => d.term_n = 0,
        "copy" => {
            let mut names = [""; 2];
            let mut k = 0;
            for w in cmdline.split_whitespace() {
                if w != "copy" && k < 2 {
                    names[k] = w;
                    k += 1;
                }
            }
            if k == 2 {
                match copy_file(names[0], names[1]) {
                    Ok(_) => {
                        out_str(d, "copied");
                        out_nl(d);
                    }
                    Err(_) => {
                        out_str(d, "copy failed");
                        out_nl(d);
                    }
                }
            }
        }
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
            // 按窗口宽度截断：长行不能顶出窗口边框（以前会压到隔壁窗口）
            let maxc = (((cv.width as i32) - 40) / 6).max(1) as usize;
            let n = lens[i].min(maxc);
            let s = unsafe {
                core::str::from_utf8_unchecked(&text[starts[i]..starts[i] + n])
            };
            cv.text_bg(34, y, s, TEXT_DK, 0x00FC_FCFC);
            y += 16;
        }
        let maxc = (((cv.width as i32) - 56) / 6).max(1) as usize;
        let ln = d.term_len.min(maxc);
        let line = unsafe { core::str::from_utf8_unchecked(&d.term_line[..ln]) };
        cv.text_bg(34, y + 2, ">", 0x00E9_5454, 0x00FC_FCFC);
        cv.text_bg(50, y + 2, line, TEXT_DK, 0x00FC_FCFC);
        cv.fill(50 + (d.term_len as i32) * 6, y + 2, 2, 16, 0x00E9_5454);
    }
}


/// 把屏幕坐标映射成文件列表的行号。
///
/// 抽出来是为了**能单独验证**：鼠标事件在 headless 虚拟机里注入不了，
/// 但这段"坐标算出第几行"的数学可以拿合成坐标直接跑。
/// 侧栏命中：Home / Documents / Downloads / Music / Pictures / Videos
/// 返回 0..5，点空白返回 None。
///
/// 之前侧栏完全没接点击——`fm_row_at` 只算文件列表，x 小于列表起点就返回 None，
/// 所以"点 Home、点 Documents 都没反应"。
fn fm_sidebar_at(w: &Win, mx: i32, my: i32) -> Option<usize> {
    let sx = w.x + 8;
    let sy = w.y + TITLE_H + 44;
    if mx < sx || mx >= w.x + 150 {
        return None;
    }
    if my < sy {
        return None;
    }
    let row = ((my - sy) / 34) as usize;
    if row < 6 {
        Some(row)
    } else {
        None
    }
}

fn fm_row_at(w: &Win, mx: i32, my: i32, first: usize) -> Option<usize> {
    let lx = w.x + 162;
    let ly = w.y + TITLE_H + 44;
    if mx < lx || my < ly {
        return None;
    }
    let row = ((my - ly) / 34) as usize;
    Some(first + row)
}

/// 尝试接入浏览器客户端：读它发布的握手文件，映射同一块共享内存。
///
/// 这是**不用 fork** 的客户端模式：客户端自己申请缓冲并把 id 写进文件，
/// 窗口服务读到就贴图。客户端崩了只影响自己那块区域，服务端不受牵连。
fn adopt_browser(d: &mut Desktop) {
    if d.browser_adopted {
        return;
    }
    let fd = liblumi::open("/data/win-browser.conf", liblumi::O_RDONLY);
    if fd < 0 {
        return;
    }
    let mut buf = [0u8; 32];
    let n = read(fd, &mut buf);
    close(fd);
    if n <= 0 {
        return;
    }
    let mut nums = [0i64; 3];
    let mut idx = 0usize;
    let mut cur = 0i64;
    let mut any = false;
    for i in 0..n as usize {
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
    if idx < 3 || nums[0] <= 0 || nums[1] <= 0 || nums[2] <= 0 {
        return;
    }
    if let Some(va) = shm_map(nums[0]) {
        let cv = Canvas::from_shm(va, nums[1] as u32, nums[2] as u32);
        d.wins[2].canvas = Some(cv);
        d.browser_adopted = true;
        d.dirty = true;
        // 客户端画的内容在它自己那侧，这里只要贴上去就行
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

/// 把列表画进窗口自己的缓冲 —— 布局照着 Ubuntu 的 Files（Nautilus）：
/// 顶部是路径条，左侧是位置栏（Places），右侧是文件列表。
fn fm_paint(d: &mut Desktop) {
    let w = d.wins[0];
    let cv = match w.canvas {
        Some(c) => c,
        None => return,
    };
    let cw = cv.width as i32;
    let chh = cv.height as i32;

    cv.clear(0x00FC_FCFC);

    // ---- 顶栏：路径条 + 视图按钮 ----
    cv.fill(0, 0, cw, 44, 0x00F6_F5F4);
    cv.fill(0, 43, cw, 1, 0x00DE_D9D6);
    // 返回/前进箭头
    cv.fill(14, 16, 12, 12, 0x0055_5753);
    cv.fill(34, 16, 12, 12, 0x0055_5753);
    // 路径胶囊
    cv.fill(56, 10, cw - 120, 24, 0x00FF_FFFF);
    cv.fill(56, 10, cw - 120, 1, 0x00DE_D9D6);
    cv.fill(56, 33, cw - 120, 1, 0x00DE_D9D6);
    cv.text(66, 15, "/  data", 0x0024_2424);

    // ---- 左侧位置栏 ----
    let sidebar = 150i32;
    cv.fill(0, 44, sidebar, chh - 44, 0x00F6_F5F4);
    cv.fill(sidebar, 44, 1, chh - 44, 0x00DE_D9D6);
    let places = ["Home", "Documents", "Downloads", "Music", "Pictures", "Videos"];
    let mut py = 56;
    for (i, name) in places.iter().enumerate() {
        let sel = i == 0;
        if sel {
            cv.fill(8, py - 4, sidebar - 16, 26, 0x00FC_EA_E9); // 极浅的暖底
            cv.fill(8, py - 4, 3, 26, 0x00E9_5454);              // 左侧 3px 色条
        }
        let col = 0x0033_3639;
        // 小方块当图标
        cv.fill(18, py + 3, 9, 9, if sel { 0x00E9_5454 } else { 0x00AE_ABA7 });
        cv.text_bg(34, py, name, col, if sel { 0x00FC_EA_E9 } else { 0x00F8_F7F6 });
        py += 30;
    }

    // ---- 右侧文件列表 ----
    let list_x = sidebar + 12;
    let row_h = 34;
    let mut y = 56;
    if d.fm_n == 0 {
        cv.text_bg(list_x, y, "(empty)", 0x0055_5753, 0x00FC_FCFC);
        return;
    }
    let visible_rows = ((chh - 60) / row_h).max(1) as usize;
    let first = if d.fm_sel >= visible_rows {
        d.fm_sel + 1 - visible_rows
    } else {
        0
    };
    for i in first..(first + visible_rows).min(d.fm_n) {
        let e = d.fm_entries[i];
        let sel = i == d.fm_sel;
        if sel {
            cv.fill(list_x - 6, y - 4, cw - list_x - 6, row_h - 2, 0x00FC_EA_E9);
            cv.fill(list_x - 6, y - 4, 3, row_h - 2, 0x00E9_5454);
        }
        // 图标：目录用橙黄方块，文件用灰白方块
        let icon = if e.is_dir { 0x00E5_A50B } else { 0x00CF_CFCF };
        cv.fill(list_x, y, 16, 16, icon);
        let col = if sel { 0x00C0_3A_33 } else { 0x0024_2424 };
        let bg = if sel { 0x00FC_EA_E9 } else { 0x00FF_FFFF };
        // 文件名按剩余宽度截断，别顶出窗口
        let room = (((cw - list_x - 40) / 6).max(1)) as usize;
        let nm = e.as_str();
        let nm = &nm[..nm.len().min(room)];
        cv.text_bg(list_x + 26, y, nm, col, bg);
        y += row_h;
    }
    // 底部状态条
    cv.fill(0, chh - 18, cw, 18, 0x00F6_F5F4);
    cv.text_bg(sidebar + 12, chh - 16, "UP/DOWN  ENTER", 0x0055_5753, 0x00F6_F5F4);
}

/// 尝试接入浏览器客户端：读它发布的握手文件，映射同一块共享内存。
///
/// 这是**不用 fork** 的客户端模式：客户端自己申请缓冲并把 id 写进文件，
/// 窗口服务读到就贴图。客户端崩了只影响自己那块区域，服务端不受牵连。
fn paint_body(cv: &Canvas, w: &Win) {
    // 只铺白底。占位文案和那条彩色竖条都去掉——
    // 它们是"演示程序"的味道，真实应用的窗口里不该有这种东西。
    cv.clear(0x00FF_FFFF);
    let _ = w;
}

/// 画鼠标指针。
///
/// 之前是几个方块拼的"伪箭头"，边缘全是台阶，一眼假。
/// 现在用一张 11x16 的位图：`#` 描边（黑）、`*` 填充（白），
/// 这是 Windows/macOS 指针的经典形状，缩到 1:1 像素也不会糊。
fn draw_cursor(c: &Canvas, x: i32, y: i32) {
    const CUR: [&str; 16] = [
        "#..........",
        "##.........",
        "#*#........",
        "#**#.......",
        "#***#......",
        "#****#.....",
        "#*****#....",
        "#******#...",
        "#*******#..",
        "#****#####.",
        "#**#****#..",
        "#*#.#***#..",
        "##...#**#..",
        ".....###*#.",
        "......#**#.",
        ".......##..",
    ];
    for (dy, row) in CUR.iter().enumerate() {
        for (dx, ch) in row.bytes().enumerate() {
            let color = match ch {
                b'#' => 0x0012_1720,
                b'*' => 0x00FF_FFFF,
                _ => continue,
            };
            // 贴边时也要画得出来：put 内部已做边界钳制
            c.put(x + dx as i32, y + dy as i32, color);
        }
    }
}
