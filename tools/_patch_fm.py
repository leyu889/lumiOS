# -*- coding: utf-8 -*-
"""把第 0 个窗口（File Manager）做成真正能浏览目录的文件管理器。"""
import io

P = r'E:\workbuddy\Deepseek_OS\kernel\userland\src\bin\lumiwin.rs'
s = io.open(P, encoding='utf-8').read()

# ---- 1) 状态字段 ----
s = s.replace(
    "    /// 终端输出（字节流，按换行分行绘制）",
    "    /* ---- 文件管理器（第 0 个窗口）---- */\n"
    "    fm_entries: [DirEnt; DIR_MAX],\n"
    "    fm_n: usize,\n"
    "    fm_sel: usize,\n"
    "    /// 终端输出（字节流，按换行分行绘制）",
)
s = s.replace(
    "            term_text: [0; 2048],",
    "            fm_entries: [DirEnt { name: [0; 32], len: 0, is_dir: false }; DIR_MAX],\n"
    "            fm_n: 0,\n"
    "            fm_sel: 0,\n"
    "            term_text: [0; 2048],",
)

# ---- 2) 文件管理器函数（插在 paint_body 之前）----
FM = r'''
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

'''
s = s.replace("/// 把窗口内容画进它自己的缓冲。", FM + "/// 把窗口内容画进它自己的缓冲。", 1)

# ---- 3) 键盘：方向键 / Enter / Backspace 操作文件管理器 ----
s = s.replace(
    "                KEY_ENTER => term_run(&mut d),",
    """                KEY_UP => {
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
                KEY_ENTER => term_run(&mut d),""",
)

# ---- 4) 启动时读一次目录 ----
s = s.replace(
    '    out_str(&mut d, "LumiOS shell - type help");',
    '    fm_refresh(&mut d);\n\n    out_str(&mut d, "LumiOS shell - type help");',
    1,
)

io.open(P, 'w', encoding='utf-8').write(s)
print('file manager patched')
