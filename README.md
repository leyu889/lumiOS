# LumiOS

**极度轻量化的自研图形操作系统。** 内核与用户态全部从零写起，不继承 Linux、不引入 systemd / GNOME / KDE / X11。

只预装四个组件，多一个都不给：

| # | 组件 | 作用 | 快捷键 | 状态 |
|---|---|---|---|---|
| 1 | **桌面**（`lumiwin`） | 窗口管理、壁纸、桌面图标、窗口开关与拖拽 | — | 第一版可运行 |
| 2 | **文件管理器** | 图形化浏览目录、基础文件操作 | `F1` | 待接入 |
| 3 | **终端** | 图形终端，执行 shell 命令 | `F2` | 待接入 |
| 4 | **浏览器** | Servo 内核 | `F3` | 待评估 |

- 窗口切换：`ALT+TAB`；关闭窗口：`ESC` 或标题栏右侧 `x`
- **不内置文本编辑器** —— 需要时自己在终端里装。

## 现在能跑成什么样

```
qemu-system-x86_64 -cdrom LumiOS-0.1.iso -m 512 -serial stdio
```

内核自检 **63 项全绿**之后自动进入桌面：渐变壁纸、三个桌面图标、三个可拖拽窗口、
任务栏、真实鼠标光标、`ALT+TAB` 焦点切换。

## 图形栈是自研的，没有 X11

```
应用（文件管理器 / 终端 / 浏览器）
        |  共享内存像素缓冲 + 事件
lumiwin   窗口服务（唯一被允许写帧缓冲的进程）
        |  fb_map
内核      VBE 线性帧缓冲 + 图形控制台 + 键盘/鼠标驱动
```

- **帧缓冲**：`boot/stage2.asm` 实模式里 VBE 枚举模式，选 1024x768x32，参数写到物理
  `0x9380` 交接块；内核 `video.rs` 把它映射到独立虚拟窗口（帧缓冲通常在 1 GiB
  直接映射区之外，直接映射过去必 #PF）。
- **输入**：PS/2 键盘 + 鼠标，i8042 轮询（8259A 在 APIC 模式下整体屏蔽，
  用中断得配 IOAPIC；轮询最坏 10 ms 延迟，够用且现场可查）。
- **系统调用**：Linux 子集之外，LumiOS 自己的东西走 `0x400` 号 + 子操作，
  不污染 Linux 号段：`fb_info / fb_map / input_read / mouse_read /
  shm_alloc / shm_map / shm_free`。

## 目录

| 路径 | 内容 |
|---|---|
| `lumikernel/` | 内核（Rust，no_std，x86_64 宏内核） |
| `boot/` | 自研引导链（stage1 -> stage2 -> 长模式 / UEFI 加载器） |
| `crates/` | 内核组件：`knet` 协议栈、`kfat` FAT16/32、`kfs`、`ksched`、`kqa` 等 |
| `userland/` | 用户态（no_std Rust -> 静态 ELF）：`liblumi` + 各应用 |
| `res/fonts/` | 字库（Spleen 8x16，BSD-2-Clause，许可证同目录） |
| `docs/` | 设计文档与工程规约（含 70+ 条踩坑记录） |
| `tools/` | 构建与打包脚本（自研汇编器、镜像生成、字库转换等） |

## 构建

```bash
python build.py     # 产出 out/LumiKernel-0.1.iso 与 UEFI 镜像
```

需要 Rust（自带离线工具链在 `tools/rust/`，也可只装 rustup 的
`x86_64-unknown-none` 目标）。用户态由 `tools/build-userland.py` 单独构建，
`build.py` 会自动调用。

## 诚实的边界

- **文件管理器 / 终端 / 浏览器还没接入**：窗口服务已有，三个应用窗口目前是占位内容。
- **Servo 现在接不了**：它需要 std 线程、futex、文件映射与 GL/WebRender 表面，
  这四样内核都还没有。计划是先补内核能力 + 做最小 HTML 渲染子集验证图形栈，
  再往上接 Servo；路径会单独写成文档。
- **窗口重绘是整屏刷新**：78 万像素一次全画，虚拟机里 20-40 ms。
  等真的卡了再上脏矩形与双缓冲。
- **SMP 真并行未启用**：4 个 AP 能上线，8 核并发在 execve 路径会静默卡死（全局 TSS）。

## 许可

内核与用户态：MIT。字库：Spleen 8x16，BSD-2-Clause（见 `res/fonts/LICENSE-spleen`）。
