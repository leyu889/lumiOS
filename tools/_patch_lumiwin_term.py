# -*- coding: utf-8 -*-
"""把 lumiwin 的终端从"静态串日志"改成"真实命令 + 动态输出缓冲"。"""
import io

P = r'E:\workbuddy\Deepseek_OS\kernel\userland\src\bin\lumiwin.rs'
s = io.open(P, encoding='utf-8').read()

# ---- 1) 字段：静态串数组 -> 字节流缓冲 ----
s = s.replace(
    "    term_log: [&'static str; 14],\n    term_n: usize,",
    "    /// 终端输出（UTF-8 字节流，按换行符分行绘制）\n    term_text: [u8; 2048],\n    term_n: usize,",
)
s = s.replace(
    '''            term_log: ["LumiOS shell (builtin)", "type help + Enter", "", "", "", "", "", "", "", "", "", "", "", ""],
            term_n: 2,''',
    '''            term_text: [0; 2048],
            term_n: 0,''',
)

# ---- 2) 替换 term_run + repaint_term ----
start = s.index('fn term_run(d: &mut Desktop) {')
end = s.index('/// 把窗口内容画进它自己的缓冲。')

NEW = r'''/// 往终端输出追加一段文本（满两千字节就不再涨）。
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
    let line = unsafe { core::str::from_utf8_unchecked(&d.term_line[..d.term_len]) };
    let cmdline = line.trim();

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
                let fd = open(arg, O_RDONLY);
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
                let fd = open(arg, O_RDONLY);
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
                let fd = open(arg, O_RDONLY);
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

'''
s = s[:start] + NEW + s[end:]

# ---- 3) 启动问候 ----
s = s.replace(
    '    repaint_term(&mut d);',
    '    out_str(&mut d, "LumiOS shell - type help");\n    out_nl(&mut d);\n    repaint_term(&mut d);',
    1,
)

io.open(P, 'w', encoding='utf-8').write(s)
print('lumiwin patched')
