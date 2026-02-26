# renpak

Ren'Py 游戏资产压缩工具链。将 RPA 包中的图片重编码为 AVIF，大幅缩小体积。

## 效果

以 Eternum 0.9.5 为例：

| 指标 | 原始 | 压缩后 | 比率 |
|------|------|--------|------|
| RPA 总大小 | 11.5 GB | 7.4 GB | 64% |
| 图片数据 | 5.2 GB | 1.1 GB | 21% |
| 图片数量 | 12732 张 | 12732 张 | — |
| 构建耗时 | — | 9 分钟 (16 核) | — |
| 构建内存 | — | ~8 MB | — |

游戏运行时透明加载 AVIF，不修改引擎源码。

## 架构

```
crates/renpak-core/     Rust 构建引擎 + CLI
  src/lib.rs              libavif FFI、AVIF/AVIS 编码
  src/rpa.rs              RPA-3.0 读写（pickle 索引）
  src/pipeline.rs         并行编码管线（Rayon）
  src/main.rs             CLI 入口

python/runtime/         Ren'Py 运行时插件
  renpak_init.rpy         init -999 启动钩子
  renpak_loader.py        文件拦截 + AVIF 加载
```

## 构建

依赖：Rust toolchain、libavif (with rav1e encoder)、pkg-config

```bash
cd crates/renpak-core
cargo build --release
```

## 使用

```bash
# 压缩 RPA（quality=60, speed=8, 自动检测核心数）
./target/release/renpak input.rpa output.rpa -q 60 -s 8

# 指定线程数
./target/release/renpak input.rpa output.rpa -q 60 -s 8 -j 8
```

部署到游戏：

1. 用压缩后的 RPA 替换原 RPA
2. 复制 `python/runtime/renpak_init.rpy` 和 `renpak_loader.py` 到 `game/` 目录
3. 启动游戏，图片自动从 AVIF 加载

## 许可证

MPL-2.0
