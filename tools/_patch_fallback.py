# -*- coding: utf-8 -*-
"""把 busybox 从"模式"改成"兜底"：内建没有的命令自动走 busybox。"""
import io

p = 'userland/src/bin/lumiwin.rs'
s = io.open(p, encoding='utf-8').read()

# 1) 把原来 ubuntu 分支里的执行逻辑抽成函数
start = s.index('    // ---- Ubuntu 模式：交给 busybox，输出经 /data/.out 取回 ----')
end = s.index('    match cmd {', start)
body = s[start:end]

helper = '''/// 用 busybox 执行一条命令，并把输出读回终端缓冲。
///
/// 为什么不 fork：窗口服务里 fork 会崩（COW 与共享映射冲突，实测过）。
/// 内核 OP_RUN 直接 spawn，父进程不受影响。
fn run_busybox(d: &mut Desktop, cmdline: &str) {
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
}

'''

# 用函数调用替换原分支
s = s[:start] + '''    // ---- 内建命令之外，一律交给 busybox ----
    // 这样默认就有 ~300 个命令（grep sed awk find sort tar ps ...），
    // 不需要先切模式。ubuntu/windows 只用来切换"习惯风格"。
    const BUILTIN: [&str; 18] = [
        "help", "clear", "version", "ls", "cat", "head", "wc", "pwd", "cd", "rm",
        "echo", "dir", "type", "del", "cls", "copy", "ubuntu", "windows",
    ];
    if !cmdline.is_empty() && (d.term_mode == 1 || !BUILTIN.contains(&cmd)) {
        run_busybox(d, cmdline);
        return;
    }

''' + s[end:]

# 插入 helper 到 term_run 之前
s = s.replace('fn term_run(d: &mut Desktop) {', helper + 'fn term_run(d: &mut Desktop) {', 1)

# 2) help 更新
s = s.replace('''            out_str(d, "real: ls cat head wc pwd cd rm echo clear version");
            out_nl(d);
            out_str(d, "stub: any other name explains why");
            out_nl(d);''','''            out_str(d, "builtin: ls cat head wc pwd cd rm echo clear version");
            out_nl(d);
            out_str(d, "win cmd: dir type del cls copy");
            out_nl(d);
            out_str(d, "anything else -> busybox (grep sed awk find tar ps ...)");
            out_nl(d);''')

io.open(p, 'w', encoding='utf-8').write(s)
print('已改为 busybox 兜底；run_busybox 出现', s.count('fn run_busybox'), '次')
