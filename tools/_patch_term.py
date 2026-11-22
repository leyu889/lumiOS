# -*- coding: utf-8 -*-
"""给终端加 ubuntu/windows 模式切换 + Windows 风格命令别名。"""
import io

p = 'userland/src/bin/lumiwin.rs'
s = io.open(p, encoding='utf-8').read()

# 1) 模式字段
if 'term_mode' not in s:
    s = s.replace('    /// 浏览器地址栏内容',
                  '    /// 终端模式：0=内建（Windows 风格），1=busybox（Ubuntu）\n    term_mode: u8,\n    /// 浏览器地址栏内容')
    s = s.replace('            fm_last_click: usize::MAX,',
                  '            fm_last_click: usize::MAX,\n            term_mode: 0,')

# 2) term_run 开头插入模式处理
anchor = '''    let mut it = cmdline.split_whitespace();
    let cmd = it.next().unwrap_or("");
    let arg = it.next().unwrap_or("");

    match cmd {'''

block = '''    let mut it = cmdline.split_whitespace();
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
    if d.term_mode == 1 && !cmdline.is_empty() {
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

    match cmd {'''

assert anchor in s
s = s.replace(anchor, block, 1)

# 3) Windows 风格命令
s = s.replace('''    match cmd {
        "" => {}''', '''    match cmd {
        "" => {}
        "dir" => {
            // dir 等价 ls
            let mut ents = [DirEnt { name: [0; 32], len: 0, is_dir: false }; DIR_MAX];
            let n = readdir(if arg.is_empty() { "." } else { arg }, &mut ents);
            for i in 0..n.max(0) as usize {
                out_str(d, ents[i].as_str());
                out_str(d, "  ");
            }
            out_nl(d);
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
        }''', 1)

io.open(p, 'w', encoding='utf-8').write(s)
print('已写入；term_mode 出现', s.count('term_mode'), '次')
