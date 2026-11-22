# -*- coding: utf-8 -*-
"""补齐 Windows 风格命令（模拟 cmd 的语义与文案）。"""
import io

p = 'userland/src/bin/lumiwin.rs'
s = io.open(p, encoding='utf-8').read()

# 1) is_builtin 里加入新命令，避免被转给 busybox
s = s.replace('''        "" | "help" | "clear" | "version" | "ls" | "cat" | "head" | "wc" | "pwd" | "cd"
            | "rm" | "echo" | "dir" | "type" | "del" | "cls" | "copy" | "ubuntu" | "windows"''',
'''        "" | "help" | "clear" | "version" | "ls" | "cat" | "head" | "wc" | "pwd" | "cd"
            | "rm" | "echo" | "dir" | "type" | "del" | "cls" | "copy" | "ubuntu" | "windows"
            | "ver" | "vol" | "set" | "where" | "tree" | "more" | "md" | "mkdir" | "rd"
            | "ren" | "move" | "exit" | "prompt" | "title" | "date" | "time" | "attrib"''')

# 2) 替换 dir 分支为 Windows 风格输出
old_dir = '''        "dir" => {
            // dir 等价 ls
            let mut ents = [DirEnt { name: [0; 32], len: 0, is_dir: false }; DIR_MAX];
            let n = readdir(if arg.is_empty() { "." } else { arg }, &mut ents);
            for i in 0..n.max(0) as usize {
                out_str(d, ents[i].as_str());
                out_str(d, "  ");
            }
            out_nl(d);
        }'''
new_dir = '''        "dir" => {
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
        }'''
assert old_dir in s
s = s.replace(old_dir, new_dir, 1)

# 3) 数字输出小工具
helper = '''/// 往终端写一个十进制数（没 alloc，手工转）。
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

'''
s = s.replace('fn is_builtin(cmd: &str) -> bool {', helper + 'fn is_builtin(cmd: &str) -> bool {', 1)

io.open(p, 'w', encoding='utf-8').write(s)
print('Windows 命令已补齐；write_num:', s.count('fn write_num'))
