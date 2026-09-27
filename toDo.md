# paintEngine 待办事项

> 基于 2026-09-27 全项目盘点。按优先级分层，勾选进度与 plan.md 同步更新。

## 当前状态

- **核心引擎** ✅ 无限画布 / 图层（蒙版/剪贴/12 混合模式）/ COW 撤销 / 笔刷（圆头/纹理尖/稳定器/tilt）/ 选区 / 内容变换 / 剪贴板 / 文字 / 矢量形状 / 笔刷预设
- **渲染** ✅ CPU 软件全功能 + GPU 合成（parity 12/12 对齐）+ GPU 盖章（功能可用，架构待升级）
- **I/O** ✅ PNG 8/16-bit · JPEG · WebP · SVG · OpenRaster
- **桌面** ✅ 快捷键全驱动（绘画/图层/变换/剪贴板/预设/取色）
- **Web** ✅ 完整工具栏 + 图层面板侧栏 + 手势 + .ora 存档
- **Android** ✅ 真机验证（JNI 壳 + 图层对话框 + APK）
- **测试** ✅ 126 项自动化（单测 + e2e + CPU↔GPU parity + 真机/浏览器实测）

---

## 🎯 高影响快赢（各 ≤ 半天）

- [ ] **CI 管线** — GitHub Actions：测试矩阵（native + wasm32 + Android 交叉编译）+ clippy 零容忍 + fmt 检查。当前全靠本地手跑。
- [x] **对称绘画** — 水平/垂直/双轴/径向（N 分旋转）镜像；引擎 SymmetryMode + dab 展开副本；桌面 X 键循环 / Web 按钮；各向异性角度同步翻转。
- [x] **画布尺寸** — Document.canvas 字段（Option<Rect>）；合成器画布外深灰/画布内正常背景/图层裁剪到画布/1px 边框；导出默认取画布尺寸；桌面 Ctrl+Shift+C 切换/Web 下拉预设（1080p/2K/4K/A4/方形）。
- [x] **WebGPU 启用** — paint-wasm `--features gpu` 启用（GPU 尝试 → CPU 回退）；仅增 120KB（2.74 vs 2.61MB）。注意：浏览器 WebGPU 初始化是异步的，pollster block_on 在部分浏览器可能不工作；实际性能提升需在 Chrome 113+ 验证。

## 🔧 中等工程（各 1-3 天）

- [x] **滤镜系统** — Filter 框架（瓦片提取→处理→写回 + 选区蒙版混合 + 撤销）；首批 5 滤镜：模糊（box×3 近似高斯）/亮度对比度/色相饱和度（HSL 全链）/反色/灰度；桌面 F 键 / Web 按钮组。
- [x] **图层组（轻量 MVP）** — Layer.group 标签分组 + 批量可见性切换 + Web 面板缩进显示；不做嵌套合成（组内层独立合成）。
- [x] **桌面可视面板** — 呈现拦截架构（引擎 Resize 传减面板宽，App 侧引擎帧+面板拼接为全宽帧呈现）；swash 光栅化文字（系统 Helvetica，零资源依赖）；图层行（●眼切标记/层名含蒙版剪贴后缀/透明度条/活动高亮）+ 底部快捷键提示；Hit 命中测试框架（点击选层/眼切，后续接线）。
- [x] **Wasm 瘦身** — 三档产物（`./build-wasm.sh all`）：slim **837KB**（无 SVG/文字，-62%）/ full **2.1MB**（全功能）/ gpu **2.2MB**（+GPU）；opt-level=z + fat LTO + strip + codegen-units=1；paint-core 的 svg/text 改 optional 并在中间 crate 关闭 default-features 传播。
- [x] **多文档** — Engine swap_document/document_take/new_document（Document 携带全部状态：图层/撤销/选区/浮动，切换零丢失）；桌面 Ctrl+N 新建 / Ctrl+W 关闭 + swash 标签栏绘制；Web 标签栏 UI（点击切换/＋新建）。

## 🏗️ 大工程（一周+）

- [ ] **iOS 壳** — Swift + Metal。最大缺失平台。需讨论：UIKit View vs SwiftUI、签名调试条件。
- [ ] **PSD 导入** — Adobe 格式极其复杂。建议只做合并层只读（不做分层/效果/文字还原）。
- [x] **调整图层** — LayerAdjustment（亮度/对比度/饱和度/色相 + strength 强度插值，HSL 全链）；作为图层属性合成时应用（nearest 采样像素变换 / bilinear 四邻域逐一变换后插值），存储像素不变（非破坏）；GPU TileUniform 扩展 + WGSL 直行域应用（饱和度走亮度保持近似，色相仅 CPU 精确——parity 容差内）；Web 调整面板（三滑杆 + 清除）。
- [x] **插件 API** — 编译期插件（Rust trait 对象）：TipPlugin/FilterPlugin/ToolPlugin 三扩展点 + PluginRegistry（Arc 共享，宿主持有）；参数系统（Range/Toggle/Choice 定义 + 运行时值）；PluginAction 安全动作模型（工具插件不能直接改图层）；filter.rs apply_filter_via 管道复用（插件处理函数注入）；内置示例：星形/菱形笔尖 + 通道偏移滤镜；Web 插件按钮（JSON 参数）+ 桌面 J 键。运行时插件（WASM 模块）接口已预留——宿主代码只依赖 PluginRegistry。
- [x] **笔刷引擎重构** — brush.rs 模块：BrushTip 形状抽象（圆头 SDF/方形 SDF 含圆角/纹理图采样）+ coverage 统一接口；DualBrush 双重笔尖（Intersect/Union/Subtract 三组合模式 + 副笔尺寸比/角度偏移/间距比）；stamp 热路径内联组合采样（副笔坐标反旋转→副角度系）；RoundBrush 扩展 brush_tip/dual 字段（序列化预设向后兼容：dual 不参与文本格式）。

## ⚠️ 工程健康（不紧急但迟早要做）

- [x] **错误恢复** — 3 处显式 panic 消除（unreachable→防御兜底/panic→清晰断言）；engine/history/layer 非测试 unwrap 全清理（let-else/total_cmp/Option 链）；LayerStack 补 try_get。
- [x] **内存监控** — TileGrid::memory_bytes + Document::tile_memory_bytes/total_memory_bytes + Engine::memory_report()（瓦片/撤销/总计三元组）。
- [x] **边界测试** — 7 项永久回归：1×1 画布+200px 笔刷 / 极端缩放循环 / 空图层栈全操作 / 变换中换文档 / 1e12 超大平移 / 畸形输入矩阵（ORA/PNG/JPEG/WebP/SVG 各变体）/ 内存报告健全性。
- [ ] **文字编辑** — 只能插入不能修改已有文字。需文字对象持久化 + 光标编辑。
- [ ] **形状持久化** — 形状立即栅格化，无矢量对象保留。SVG 导出依赖此项。
- [ ] **API 文档** — rustdoc 覆盖不完整；补全公共接口文档 + 使用示例。
- [ ] **示例集** — 常见用法的可运行示例（嵌入自定义应用/离线渲染/服务器端渲染）。

## 📋 遗留决策记录

| 项 | 状态 | 说明 |
|---|---|---|
| GPU 盖章架构升级 | 条件触发 | 当前逐瓦片上传/回读不如 CPU；需持久化 GPU 瓦片 + 批量 compute + 异步回读。触发条件：大笔刷高频使用掉帧 |
| SVG 导出 | 条件触发 | 等矢量对象模型成熟（形状持久化）时架构上自然 |
| 16-bit 色深 | ✅ 导出路径完成 | f32 中间精度 + 16-bit PNG；瓦片内部仍 u8（性能/内存折中） |
| 自定义工程格式 | 已决策 | 只用 OpenRaster，不自造 |

---

## 建议优先级

如果只做三件事：**CI** → **对称绘画** → **WebGPU 启用**（合计 1.5 天，保护成果 + 功能感知 + 性能翻倍）

如果再做两件：**滤镜框架 + 高斯模糊** → **图层组**（合计 2 天，打开非破坏性编辑和复杂文档组织的基础）
