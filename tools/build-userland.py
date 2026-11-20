#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""构建 LumiOS 用户态程序（no_std Rust → 静态 ELF）。

为什么要单独一个脚本、还要显式设 `RUSTFLAGS`：
内核的 `.cargo/config.toml` 把 `-Tlinker.ld --oformat=binary` 放在
`[target.x86_64-unknown-none]` 下，而 cargo 的配置是**逐层继承**的——
在 `kernel/userland/` 里构建时，内核那份仍然会生效，结果链接出一个
没有 ELF 头的裸二进制，内核的 exec 直接装不进去。

环境变量 `RUSTFLAGS` 是唯一能**整体替换**（而不是追加）配置里 rustflags
的入口，所以这里用它把内核那套彻底盖掉。
"""

import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
USERLAND = os.path.join(ROOT, "userland")
TOOLCHAIN = os.path.join(ROOT, "tools", "rust")
CARGO = os.path.join(TOOLCHAIN, "cargo", "bin", "cargo.exe")

TARGET = "x86_64-unknown-none"

# 用户态：ELF、小代码模型、入口 0x400000（见 userland/user.ld）
RUSTFLAGS = " ".join([
    "-C", "link-arg=-Tuser.ld",
    "-C", "link-arg=--build-id=none",
    "-C", "relocation-model=static",
    "-C", "code-model=small",
])

# 用户态要构建哪些程序：名字 → ELF 文件名（与 Cargo.toml 的 [[bin]] 对应）
BINS = ["lumiwin", "lumidemo"]


def main():
    if not os.path.isfile(CARGO):
        print("[userland] 找不到自带 cargo：%s" % CARGO)
        return 1

    env = os.environ.copy()
    env["RUSTFLAGS"] = RUSTFLAGS
    env["RUSTUP_HOME"] = os.path.join(TOOLCHAIN, "rustup")
    env["CARGO_HOME"] = os.path.join(TOOLCHAIN, "cargo")

    cmd = [CARGO, "build", "--release", "--target", TARGET]
    for b in BINS:
        cmd += ["--bin", b]

    print("[userland] 构建：%s" % " ".join(BINS))
    r = subprocess.run(cmd, cwd=USERLAND, env=env)
    if r.returncode != 0:
        print("[userland] 构建失败")
        return r.returncode

    out = os.path.join(USERLAND, "target", TARGET, "release")
    for b in BINS:
        p = os.path.join(out, b)
        if not os.path.isfile(p):
            print("[userland] 缺少产物 %s" % p)
            return 1
        print("        %-12s %6.1f KiB" % (b, os.path.getsize(p) / 1024))
    return 0


if __name__ == "__main__":
    sys.exit(main())
