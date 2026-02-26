# renpak

Ren'Py 游戏资产压缩工具链。将 RPA 包中的图片重编码为 AVIF，大幅缩小体积。

## 效果

| 游戏 | 原始 RPA | 压缩后 | 图片压缩率 | 耗时 |
|------|----------|--------|-----------|------|
| Agent17 0.25.9 | 2.3 GB | 1.3 GB | 33% | 5 min (16 核) |
| Eternum 0.9.5 | 11.5 GB | 7.4 GB | 21% | 9 min (16 核) |

游戏运行时透明加载 AVIF，不修改引擎源码。

## 架构

```
crates/renpak-core/     Rust 构建引擎 + CLI
  src/lib.rs              libavif FFI、AVIF/AVIS 编码
  src/rpa.rs              RPA-3.0 读写（pickle 索引）
  src/pipeline.rs         并行编码管线（Rayon）
  src/main.rs             CLI 入口

crates/renpak-rt/       Rust 运行时解码器（cdylib）
  src/lib.rs              AVIS 帧级随机访问，extern "C" API

python/runtime/         Ren'Py 运行时插件
  renpak_init.rpy         init -999 启动钩子
  renpak_loader.py        文件拦截 + AVIF 加载

install.sh              一键压缩 + 部署脚本
```

## 构建

依赖：Rust toolchain、libavif (with rav1e encoder)、pkg-config

```bash
cargo build --release
```

## 使用

### 一键安装

```bash
./install.sh /path/to/game/root

# 自定义参数
./install.sh /path/to/game/root -q 50 -s 6

# 排除额外前缀（gui/ 始终自动排除）
./install.sh /path/to/game/root -x images/ui/ -x images/portrait/
```

脚本自动完成：编译 → 压缩 RPA → 备份原文件 → 替换 + 部署运行时插件。

### 手动使用

```bash
# 压缩 RPA
./target/release/renpak input.rpa output.rpa -q 60 -s 8

# 排除前缀
./target/release/renpak input.rpa output.rpa -x images/ui/ -x portrait/

# 指定线程数
./target/release/renpak input.rpa output.rpa -j 8
```

部署到游戏：

1. 用压缩后的 RPA 替换原 RPA
2. 复制 `python/runtime/renpak_init.rpy` 和 `renpak_loader.py` 到 `game/` 目录
3. 启动游戏，图片自动从 AVIF 加载

### 回滚

```bash
cd /path/to/game/root/game
mv .renpak_backup/*.rpa .
rm -f renpak_init.rpy renpak_loader.py
```

## 许可证

MPL-2.0
