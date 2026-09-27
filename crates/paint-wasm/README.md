# paintEngine Web 壳

浏览器端绘画演示：画笔/橡皮（真实压感来自 Pointer Events）、双指平移缩放、
12 种混合模式、图层增删合并、PNG 导入导出。

## 运行

```bash
# 1. 安装 wasm-pack（一次性）
cargo install wasm-pack

# 2. 构建（产物输出到 www/pkg/）
wasm-pack build crates/paint-wasm --target web --out-dir www/pkg --release

# 3. 起本地静态服务（任意方式均可）
cd crates/paint-wasm/www && python3 -m http.server 8000
```

打开 http://localhost:8000 — 需要支持 WASM 的现代浏览器。
数位笔压感在 Chromium 系浏览器最佳（`pointerrawupdate` 高采样输入）。

## 交互

- 画笔/触摸绘画：单指或鼠标左键
- 平移缩放：双指（触摸）／Ctrl+滚轮近似（桌面浏览器滚轮暂未绑定，见 M3 任务）
- 快捷键：B 画笔 · E 橡皮 · Ctrl+Z / Ctrl+Shift+Z 撤销重做
- 工具栏：颜色/大小/混合模式/图层/导入导出
