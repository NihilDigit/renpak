# renpak

Ren'Py 游戏资产压缩工具链，面向 HS2/Koikatsu 等 3D 渲染视觉小说。
构建期 CLI (Python + Rust) + 运行时 Ren'Py 插件 (Python + Rust cdylib)。

许可证：MPL-2.0

## 架构

两个 Rust crate + 一个 Python 包：

- `crates/renpak-core/` — 构建期核心：RPA 读写、AVIF/AVIS 编码、场景分析
- `crates/renpak-rt/` — 运行时解码器：AVIS 帧级随机访问，导出 extern "C" API 供 ctypes 调用
- `python/renpak/` — CLI 编排层，调用 Rust 库完成构建流程
- `python/runtime/` — 部署到游戏 `game/` 目录的运行时插件 (.rpy + .py)

详细设计见 `docs/PLAN.md`。

## 构建

```bash
# Rust crates
cargo build --release

# Python CLI (用 uv，禁止 pip install)
uv run python -m renpak --help
```

## 技术约束

### Ren'Py 运行时环境

- 运行时插件跑在 Ren'Py 内置的 Python 3.9.10 (CPython) 上，不是系统 Python
- 原生库通过 ctypes.CDLL 加载，必须导出纯 C ABI (extern "C")，不用 PyO3
- 运行时 Python 代码不能依赖任何第三方包，只能用标准库 + Ren'Py 自带模块
- Ren'Py 的图像预加载在后台线程运行，Rust 解码器必须线程安全（用 per-thread context，无全局状态）

### 编码硬约束

- AVIF 色彩空间：必须显式设置 CICP (primaries=1, transfer=13, matrix=1, range=full)，否则 HS2 渲染图会偏色
- 分辨率：编码前 pad 到 8 的倍数，解码后裁剪回原始尺寸（Ren'Py Issue #5061）
- AVIS GOP：默认星型 (帧0=I帧，其余 P 帧只参考帧0)，保证 O(1) 随机访问
- 视频重编码保持 .webm 容器，Ren'Py 的 ffmpeg 自动识别 AV1 codec，无需运行时钩子

### 钩子机制

运行时通过以下 Ren'Py 钩子接入，不修改引擎源码：

- `config.file_open_callback` — 拦截文件请求，做文件名映射和序列解码
- `config.loadable_callback` — 报告原始文件名可加载
- monkey-patch `renpy.display.pgrender.load_image` — 修正 AVIF 的扩展名提示让 SDL2_image 正确识别

### 跨平台

运行时 .so/.dll/.dylib 需要为每个目标平台预编译：
- Linux x86_64
- Windows x86_64
- macOS x86_64 + aarch64

## 代码规范

### Rust

- Edition 2021，MSRV 跟随当前 stable
- `renpak-rt` 编译为 cdylib，导出函数用 `#[no_mangle] pub extern "C"`
- 错误处理：核心库用 `Result<T, renpak_core::Error>`；FFI 边界用返回码 (0=成功, 负数=错误)
- FFI 内存：Rust 分配的 buffer 必须通过 `renpak_free_buffer` 释放，不能让 Python 侧 free

### Python

- 用 uv 管理依赖，禁止 pip install / 全局安装
- 运行时代码 (runtime/) 必须兼容 Python 3.9，不能用 3.10+ 语法 (match/case, type union X|Y 等)
- CLI 代码 (python/renpak/) 可以用更高版本特性

### 测试

- Rust: `cargo test`
- Python: `uv run pytest`
- 端到端: `tests/test_roundtrip.py` — 原图 → 编码 → 解码 → 比对尺寸和像素差异

## 常用命令

```bash
cargo build --release                    # 构建 Rust
cargo test                               # Rust 测试
uv run python -m renpak build game/      # 完整构建流程
uv run python -m renpak analyze game/    # 仅分析资产
uv run python -m renpak verify output/   # 验证压缩结果
uv run pytest                            # Python 测试
```

## 不要做的事

- 不要修改 Ren'Py 引擎源码，所有集成通过钩子和 monkey-patch 完成
- 不要在运行时 Python 代码中引入第三方依赖
- 不要用 PyO3，ctypes 是唯一的 FFI 路径
- 不要用链式 GOP 作为默认值，随机访问延迟不可控
- 不要假设 Limited Range YUV，必须显式指定 Full Range
- 不要把构建输出放到 /tmp — 本机 /tmp 是 tmpfs (12GB)，装不下 RPA 输出（单个 RPA 可达 12GB）。构建输出一律放 /home 下
