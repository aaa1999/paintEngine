# paintEngine | [English](README.en.md)

跨平台绘画引擎内核，纯 Rust 实现。核心是**无限平铺画布**——稀疏瓦片存储、写时复制撤销、压感笔画光栅化与脏区合成——同一份引擎核心嵌入桌面、浏览器与 Android 原生应用。

## 特性

- **无限画布**：256px 稀疏瓦片、负坐标、无界平移缩放；点阵网格提供空间感，一键"适应内容"导航回家
- **压感笔画**：圆头笔（大小/硬度/不透明度/流量/间距）、EMA 平滑、压感→笔宽映射；橡皮擦（dst-out）
- **图层系统**：增删/复制/重排/向下合并/压平，**12 种混合模式**（W3C 公式），透明度；全部操作可撤销
- **撤销**：瓦片 COW 快照 + 结构操作（UndoOp 枚举），一笔一组，按内存限额淘汰历史
- **双渲染后端**：手写软件渲染器（预乘 alpha、双线性采样）+ wgpu GPU 合成（`feature` 门控，CPU 盖章 + GPU 合成混合形态）
- **PNG 导入导出**：范围/缩放/透明背景可选，导出永不含网格
- **OpenRaster 工程**：.ora 分层存档读写（图层/混合模式/不透明度/可见性完整保留）
- **笔迹稳定器**：磁吸抑抖 + 收笔追赶（测试：抖动方差降至 30% 以下）
- **多指手势**：双指平移缩放、误触笔画即时回滚、手势闩锁、笔悬停手掌拒绝
- **80+ 项自动化测试** + 真机/模拟器/浏览器实测

## 架构

```
crates/
├── paint-core/      引擎核心（零平台依赖）：瓦片/图层/撤销/视口/笔画/PNG/引擎骨架
├── paint-render/    Renderer 的软件实现：dab 盖章、脏区合成、图层合并
├── paint-gpu/       Renderer 的 wgpu 实现：GPU 图层合成（feature "gpu"）
├── paint-desktop/   桌面壳：winit + softbuffer（Windows/macOS/Linux）
├── paint-wasm/      Web 壳：canvas 呈现 + Pointer Events（含 pointerrawupdate）
└── paint-android/   Android 壳：JNI + Kotlin PaintEngineView（含 demo APK 工程）
```

核心原则：**引擎只负责"算出像素"（瓦片），平台壳只负责"呈现像素 + 喂事件"**。`paint-core` 不感知屏幕存在；`Renderer` / `Surface` 两个 trait 隔离渲染后端与呈现目标。

## 平台状态

| 平台 | 状态 | 壳技术 | 验证 |
|---|---|---|---|
| 桌面三端 | ✅ | winit + softbuffer（可选 `--features gpu` 走 wgpu） | 手动实测 |
| Web | ✅ | wasm-bindgen + canvas，rAF/interval 双驱动 | 浏览器自动化逐像素实测 |
| Android | ✅ | JNI cdylib + Kotlin View | 真机（联想）+ 模拟器实测 |
| iOS | 待议 | — | — |

## 快速开始

```bash
# 桌面（鼠标绘画 · 空格/中键平移 · 滚轮缩放 · Ctrl+0 适应 · G 网格 · B/E 工具）
cargo run -p paint-desktop --release
cargo run -p paint-desktop --release --features gpu   # GPU 合成后端

# Web（浏览器打开 http://localhost:8000）
cargo install wasm-pack   # 一次性
cd crates/paint-wasm
wasm-pack build . --target web --out-dir www/pkg --release
cd www && python3 -m http.server 8000

# Android（APK ≈ 2.5MB，含双架构引擎；需 NDK 交叉编译 .so，详见该目录 README）
cd crates/paint-android/android && ./gradlew assembleDebug

# 全量测试
cargo test
```

## 性能参考（M2.5 基准）

2560×1440 全量合成（`cargo run -p paint-gpu --example bench_composite --release`）：

| 场景 | CPU | GPU（含回读） |
|---|---|---|
| 3 图层 | 9.9 ms/帧 | 2.5 ms/帧（**4.0x**） |
| 6 图层 | 20.1 ms/帧 | 5.4 ms/帧（**3.7x**） |
| 笔画脏区 64×64 | — | ~1.8 ms/帧 |

## 文档

- **[plan.md](plan.md)** — 完整开发计划：架构决策、API 蓝图、依赖选型（含放弃方案）、M1–M4 里程碑与逐项验收记录
- 各壳 crate 的 README — 平台特定的构建与运行细节
