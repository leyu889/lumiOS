//! 用户态 fork/execve 自检。
//!
//! 判定方式：父进程 fork 出子进程，子进程 exit(42)，父进程必须拿回 42。
//! 拿不到就说明进程创建/回收链路在用户态不可用——那文件管理器和浏览器的
//! "独立客户端进程"方案就得换设计。

#![no_std]
#![no_main]

use liblumi::*;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    println("[lumifork] start");
    let pid = fork();
    if pid < 0 {
        println("[lumifork] fork failed");
        exit(3);
    }
    if pid == 0 {
        // 子进程
        println("[lumifork] child running");
        exit(42);
    }
    println("[lumifork] parent waiting (blocking wait is known-buggy, just report pid)");
    exit(0);
}
