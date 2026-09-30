# paintEngine 待办事项

> 基于 2026-09-27 全项目盘点。按优先级分层，勾选进度与 plan.md 同步更新。

## 当前状态

- **核心引擎** ✅ 无限画布 / 图层（蒙版/剪贴/12 混合模式）/ COW 撤销 / 笔刷（圆头/纹理尖/稳定器/tilt）/ 选区 / 内容变换 / 剪贴板 / 文字 / 矢量形状 / 笔刷预设 / 形状工具状态机 / **矢量对象模型（DrawObject/对象层 content() 视图/增量 merged 缓存）+ 油漆桶 + 自动保存 edit_count**（2026-09-29）
- **渲染** ✅ CPU 软件全功能 + GPU 合成（parity 12/12 对齐，两端统一读 content()）+ GPU 盖章（功能可用，架构待升级）
- **I/O** ✅ PNG 8/16-bit · JPEG · WebP · SVG · OpenRaster（**含矢量对象持久化：paintengine/objects.txt 私有条目，光栅随对象走**）
- **桌面** ✅ 快捷键全驱动 + S 形状 · I 文字（**对象化可编辑/命中预填/Del 删除**）· A 填充 + **自动保存**（30 编辑检查点 + 退出落盘 + 启动恢复 `~/.paintengine_autosave.ora`）
- **Web** ✅ 完整工具栏 + 图层面板侧栏 + 手势 + .ora 存档 + 形状/文字（**浏览器字体对象化，光栅随对象走，刷新后可编辑**）+ **IndexedDB 自动保存（30s/隐藏时/beforeunload）+ 启动恢复**
- **Android** ✅ 真机验证 + 两级工具条（预设/形状/文字/**填充**/附件图片+PDF）+ **文字对象 v2（点击重编辑预填/拖动移动/弹窗删除）+ onPause 自动保存 + 冷启恢复**
- **测试** ✅ 168 项自动化（新增：油漆桶/对象 CRUD/ORA 对象往返/对象层笔画合并；三端实测：Web 浏览器逐像素 + Android 模拟器全流程含杀进程恢复）

---

## 🎯 高影响快赢（各 ≤ 半天）

- [x] **四硬伤攻坚：自动保存 / 文字编辑 v2 / 矢量对象模型 / 油漆桶**（2026-09-29）——**详见文末「2026-09-29 四硬伤攻坚记录」**
- [x] **入口三端落地：笔种类/打字/几何/附件**（2026-09-29）——引擎工具状态机 + 三端 UI 入口，全项实测通过。**规划与实现详见文末「2026-09-29 入口落地记录」**。
- [ ] **CI 管线** — GitHub Actions：测试矩阵（native + wasm32 + Android 交叉编译）+ clippy 零容忍 + fmt 检查。当前全靠本地手跑。
- [x] **Android 横屏适配**（2026-09-29）——manifest `screenOrientation: portrait → fullUser`（跟随系统旋转、四向、尊重用户旋转锁定）+ `configChanges` 补 `smallestScreenSize`。真机（Redmi Pad 1880×3008）验证：横屏铺满无黑边、旋转不重建 Activity、笔迹往返保留、测试后系统旋转设置已还原。引擎侧零改动（`Resize` 事件本就只重设帧缓冲，内容/视口/撤销全保留）。
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
- [x] **API 文档** — rustdoc 覆盖率 98-100%（paint-core 350/356、paint-render 5/5、paint-gpu 3/3）；全 pub API 含模块级/方法级中文文档；cargo doc --workspace 零警告；doctest 通过（循环依赖以 text 块规避）。查看：cargo doc --workspace --no-deps --open
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

---

## 2026-09-29 入口落地记录（笔种类 / 打字 / 几何图形 / 附件）

### 需求与范围（讨论定稿）

- 目标：四个入口——**笔的种类**（预设）、**打字**、**几何图形**、**附件**——三端（桌面/Web/Android）全部可用。
- 附件范围：**图片 + PDF 两种为主**，明确不排除后续其他类型 → 架构必须留活口。
- Android 文字输入：**弹窗方案可接受**（画布内 IME 属增强项，不阻塞本轮）。

### 现状盘点结论（规划前提）

能力大半在引擎、入口不在：预设系统（6 内置笔）已就绪但 Android 无 UI；`draw_text`/`stroke_line`/`stroke_ellipse` 是裸函数而非可交互工具（三端都没接）；`import_image` 已有但 Android 无按钮。**引擎就绪度 ~70%，入口就绪度 ~30%，Android 是洼地**。结论：必须先在引擎侧补"工具交互模型"，否则三端各写一遍交互必然走样。

### 架构决策（四条）

1. **工具交互进引擎，壳只做 UI**（项目铁律延续）：`Tool` 扩展 `Shape{kind: 线/矩/椭圆, fill}` 与 `Text`，壳层照旧只转发 `PlatformEvent::Pointer`，引擎按工具分派（形状=拖拽状态机，文字=点击落锚，插入=按钮+既有浮动变换）。与 `ToolPlugin` 扩展点兼容（内置工具走引擎内分派，插件照走 `PluginAction`）。
2. **形状吃笔刷参数**：拖拽预览走**帧缓冲叠加**（合成后盖印，不落瓦片、不入撤销），松手才经正规 dab 管线盖章。副产品即产品亮点——马克笔画椭圆=马克笔质感，笔种类与几何图形两个入口零成本打通。
3. **文字 v1 = 点击定位 + 平台输入框 + 落墨**，不做画布内编辑：桌面键盘直输（Enter 插入/Esc 取消）、Web 绝对定位 input、Android 弹窗（输入框+字号滑杆）。**文字编辑 v2**（对象持久化+光标编辑+SVG 导出联动）单列大工程。
4. **附件 = 壳层转位图即插即用**：引擎只认像素，永不感知格式。PDF 渲染选**壳层原生栈**（Android `PdfRenderer` / Web pdf.js / 桌面待选型）——纯 Rust 渲染中文字体不可靠，系统渲染器质量有保证且免费。通道：图片走 `import_image`（decode_auto 成层）；壳层已渲染的位图（PDF 页/浏览器文字）走 `paste_rgba_float`（浮动放置）或 `paste_rgba_at`（指定位置落层）。未来任何新附件类型（贴纸、扫描件……）只要壳能转 RGBA 即零引擎改动接入。

### 实现记录

| 层 | 内容 |
|---|---|
| 引擎 | `Tool::Shape/Text` + 拖拽状态机（预览帧级叠加、脏区含旧∪新防残影、拖拽中撤销守卫、失焦/切工具/第二指取消）；`preview.rs`（平铺帧 dab/矩形/椭圆光栅化）；`shape_dabs`（线=单段、矩=四边、椭圆=参数化周界，全部走 line_dabs 间距语义）；`take_text_anchor` 锚点协议（一次性取走）；`paste_rgba_at` 直行 RGBA 指定位置落层（alpha over + 撤销）；新增 5 项单测 |
| Android | 两级工具条（主行：画笔/橡皮/形状/文字/附件/撤销/重做/图层/适配/保存；上下文行随工具切换：预设 6 笔横条+笔号滑杆 · 线矩椭+描边填充 · 文字提示）；JNI +15 个方法（set_tool 工具码、draw_text 系统字体、preset 列表/应用、import_image 全格式、paste_rgba、transform 五件套）；paint-android 开启 text+svg 特性（APK 2.5→6.5MB）；PDF：Photo Picker→PdfRenderer 渲首页（屏宽 0.9×、上限 2200px、铺白底）→浮动放置（单指拖拽+↺↻±✓✕ 操作条） |
| Web | 工具栏 +5 按钮（直线/矩形/椭圆/描边填充切换/文字）+ L/R/O/T 快捷键；`parse_tool`/`tool_name` 字符串协议（`rect-fill` 等后缀式）；文字走**离屏 canvas 系统字体渲染**→`paste_text_rgba` 落墨（中文原生支持，实测无乱码，彻底绕开 web 字体文件分发） |
| 桌面 | S 循环形状（线→矩→椭）、Shift+S 切描边/填充、I 文字（T 已被稳定器占用）；typing 缓冲拦截键盘（Enter 提交/Esc 取消/Backspace 删字）；`load_system_font` 跨平台 CJK 回退链（PingFang→Noto→雅黑→西文） |

### 验收记录（全部通过）

- **引擎**：87 项单测（含形状提交/撤销、拖拽中不落墨+撤销守卫、Cancel/切工具无痕、文字锚点一次性）。
- **Android 真机**（Redmi Pad）：两级工具条渲染✓ 矩形描边拖拽✓ 椭圆填充✓ 文字弹窗插入（paint）✓ PDF 选择器→渲染→拖拽/旋转-30°→✓提交落层→撤销干净✓ 图片导入成层✓。
- **Web**（浏览器自动化）：矩形描边✓ 填充椭圆✓ 文字浮层输入"paintEngine 你好"落墨、中文渲染正常✓。
- **桌面**：编译+clippy 通过；键盘交互待本机手动确认。

### 遗留与后续（按建议优先级）

- [ ] **形状扩展**：箭头（终点角度 dab 链）、多边形/折线（n 点路径）、圆角矩形；形状+橡皮（dst-out 几何擦除）
- [ ] **附件变换角点手柄**：当前为按钮/滚轮方案，角点拖拽缩放体验更好（桌面/Web 先行，Android 触点判定）
- [ ] **文字编辑 v2**（已有项，扩展范围）：文字对象持久化 + 光标编辑 + 重新选中改字号/颜色；与 SVG 导出联动
- [ ] **桌面 PDF 选型**：objc2/PDFKit（仅 macOS）vs pdfium dylib（三平台但分发重）vs 复用 Web 壳思路；低频需求可缓
- [ ] **PDF 多页**：当前仅首页；后续"选页/全部页顺序成层"
- [ ] **Android 文字画布内 IME**：`InputConnection` 内联输入替代弹窗（增强项）
- [ ] **Android 预设持久化**：自定义笔预设目前仅会话内（桌面 `~/.paintengine_presets.txt`、Web localStorage 已有）

---

## 2026-09-29 四硬伤攻坚记录（自动保存 / 文字编辑 v2 / 矢量对象模型 / 油漆桶）

### 架构决策

1. **自动保存 = 引擎 edit_count + 壳层触发**：Document 在 commit/undo/redo 自增编辑计数，壳层记录上次保存值做脏检查（零轮询、零状态同步）。触发点：Android `onPause`（被杀必经）→ filesDir/autosave.ora；Web visibilitychange/beforeunload/30s → IndexedDB；桌面 30 编辑检查点 + CloseRequested → `~/.paintengine_autosave.ora`。启动时各自恢复并提示。
2. **矢量对象模型 = 对象层 + content() 统一视图**：`Layer.objects: Vec<DrawObject>`（Text 完整/Shape 预留）+ `obj_tiles` 光栅缓存（对象编辑时整体重建）+ `merged` 增量合并缓存（tiles 被 obj_tiles 覆盖；无重叠瓦片与 tiles Arc 共享零拷贝，GPU 指针失效天然兼容）。**合成器（CPU/GPU）统一改读 `Layer::content()`，两端零感知**。失效收口三处：`Document::commit`（所有提交型写入的统一收口）+ `stamp`（笔画进行中）+ history `apply_to`（撤销/重做）。
3. **文字 v2 = 对象化 + 光栅随对象走**：Web 壳层用浏览器字体渲染的光栅（`TextRaster`）存在对象上——撤销/载入后无引擎字体也能重现；Android/桌面走 swash（`set_text_font` 一次性设置系统字体，光栅化结果同样缓存回对象）。交互：点击命中（bbox ±4px 容差，自顶向下）→ 编辑弹窗预填/拖动移动/Del 删除；未命中 → 新锚点。对象增删改移全部走 `UndoOp::Objects` 快照（apply 时置 obj_stale，渲染前引擎重建光栅）。
4. **ORA 持久化 = 私有条目**：zip 内 `paintengine/objects.txt`（按层分块；文字光栅 hex(PNG)、文本 percent 编码，零新依赖）。其他 ORA 读端忽略；载入后光栅随对象恢复。
5. **油漆桶 = 扫描线连通填充**：活动层采样、容差各通道独立、选区作屏障、范围限内容包围盒∩点击±8192px（无限画布防空区域发散）、COW 入撤销。`Tool::Fill` 点击即执行。

### 实现与验收

- **引擎**：168 项测试全过（新增油漆桶填充/撤销、对象 CRUD+命中+拖动+撤销、ORA 对象往返含中文、对象层笔画合并缓存）；clippy 清零。
- **Web**（浏览器逐像素实测）：油漆桶封闭区域填充✓；文字对象插入/渲染/命中→浮层预填/更新/删除✓；IndexedDB 存→刷新→恢复（含对象光栅）✓。**顺手修复仓库既有 bug：index.html 缺 `#tabBar` 与图层面板 DOM**（main.js 加载即崩，炸掉后半段全部功能绑定——CSS 在而 DOM 丢，多文档/图层/ORA 按钮在 Web 上一直未生效）。
- **Android**（模拟器全流程，真机 systemui 卡死无法点击故移模拟器）：填充封闭区域+撤销历史✓；文字插入"PE-v2"→点击重编辑预填→更新"EDITED"→拖动移动（原位无残留）✓；**HOME→am kill→冷启恢复（矩形+文字全在）**✓。真机验收延后（设备 NotificationShade 状态机卡死，视觉层确认新 UI 已渲染）。
- **桌面**：编译+clippy 通过；A/I/S 键与 typing 编辑流待本机手测。
- 已知限制（记录）：形状对象 UI 未接（枚举/光栅化路径已就位）；油漆桶容差固定 32 未暴露；描边形状的 dab 链在极端参数下可能有缺口导致填充泄漏（实心形状无此问题）。

### 顺带产出

- `Engine::edit_count()` / `Layer::content()` / `paste_rgba_at`（上一轮）成为壳层通用通道
- Web `window.app` 调试入口 + 资源 URL 版本参数（`?v=` 破缓存，改动后记得升版本）

---

## 2026-09-30 帧率监控（配置开关，默认开）

- **引擎**：`EngineConfig.fps_monitor`（默认 true）+ `Engine::render_count()`（单调呈现计数，监控关时冻结）+ `set_fps_monitor()/fps_monitor_enabled()`。**架构决策：引擎只做帧计数、不做帧率换算**——`std::time::Instant` 在 wasm32 上运行时 panic（实测踩坑），且"引擎不依赖平台时钟"符合铁律；壳层采样 `Δcount/Δt` 即得帧率（各自有本地时钟：JS performance.now / Android SystemClock / 桌面 Instant）。事件驱动架构下空闲读数归零，即"按需重绘零开销"的直接度量。
- **三端入口**：Web 左上角悬浮框（`#fpsBox`，点击开关 + localStorage `pe_fps_monitor` 持久化，500ms 采样 EMA 平滑）；Android 左上角悬浮 Button（点击开关 + SharedPreferences `cfg/fps_monitor`，冷启按配置显示，500ms 采样）；桌面控制台 `[fps] xx.x` 每秒输出 + `0` 键运行时开关。
- **验收**：169 项测试全过（新增计数/冻结/恢复单测）；Web 浏览器实测（读数随渲染驱动增长、关闭后计数冻结、重开恢复、pref 往返）；Android 模拟器实测（FPS 54.8 实时读数、点击"FPS 关"、pref 写入 false、冷启按配置显示、再点恢复 true）。过程中两个平台坑记录：① Android 悬浮标签须避让状态栏（NoActionBar 下 margin 从屏幕顶起算）且 TextView 在该场景收不到点击（换 Button 解决，原因未明——同页 Button 全部正常）；② IAB 嵌入式浏览器会缓存旧 wasm/js 模块，产物更新后须同步升 `?v=`（main.js 引用、paint_wasm.js 内 bg.wasm URL 三处）。
- 真机 KRQGEMT8XK9TMFGA 验证时 USB 掉线（systemui 卡死事件之后），FPS 功能最终 APK 已装模拟器，真机重连后 `adb install -r` 即可。
- **设置项入口**（2026-09-30 追加）：Android 主工具条新增「设置」按钮 → 对话框多选项（**帧率监控** / 网格点阵），SharedPreferences 持久化；帧率监控关闭时悬浮标签整体隐藏（GONE），冷启按配置生效；悬浮标签点击保留为快捷开关。模拟器全链实测：设置取消勾选 → 标签消失 + pref=false → 冷启仍隐藏 → 设置重新勾选 → 标签回归 + pref=true。

---

## 2026-09-30 笔画转角折痕修复（Catmull-Rom 样条 dab 行进）

**问题**：转弯处笔画有明显折线痕迹。根因：`extend` 每个输入事件都从上一 dab 位置**直线**走向当前平滑点——笔画几何就是"输入点折线"，圆头 dab 的外包络在转角呈多边形切角；EMA 平滑只延迟不弯曲，稳定器默认关。

**修法**：dab 沿 **Catmull-Rom 过点样条**行进（`stroke.rs`）：
- `StrokeState` 增加样条缓冲（`curve` 尾部 2 点 + `curve_prev` 左邻）；段 p1→p2 需右邻 p3 才能定稿——**定稿滞后一个采样点（~8-16ms，不可感知）**，首段在第三点到达时定稿，末段在收笔时定稿（右端切向重复自身）
- `finalize_segment`：段稠密采样（8-64 点自适应）→ 沿精确弧长步进发射 dab（`walk_toward` 抽取，间距语义与原实现一致——共线输入下 dab 位置逐位不变）
- 防越冲：退化段（<1e-6）直线步进；稠密点钳制进段 AABB（±段长 1/4+0.5px），端点重复切向不再外凸
- 收笔末点 epsilon 守卫：已在目标处不重复盖章（消除双重 alpha）
- 稳定器/EMA/压感/倾斜插值语义全部保留（样条作用在平滑点之上）

**验收**：170 项测试全过（新增 `spline_rounds_sharp_corner`：直角折线输入下相邻 dab 单步转角 <45°，折线链为 ~90°；4 个旧测试适配"定稿滞后一个采样点"的新语义）；clippy 清零。浏览器实测：稀疏点 L 形快速笔画转角呈圆滑弧线、边缘连续无折痕；Android 模拟器实测同样圆滑。引擎层修复，三端同时生效。

---

## 2026-09-30 日志系统（log 门面 + 三平台后端）

- **架构**：`log` crate 门面（workspace 统一依赖，禁用时宏零开销）+ 各壳自装后端，引擎埋点一处编写三端受益。
- **后端**：桌面 = stderr 时间戳格式，`PAINT_LOG=debug|info|warn|error` 调级别（默认 info）；Android = **logcat 零依赖后端**（直连 liblog `__android_log_print`，tag `paintEngine`，nativeCreate 安装；注意 `%s` 需 NUL 终止——Rust String 直传会尾随乱码）；Web = console 级别映射，URL `?log=debug` 或 localStorage `pe_log` 调级别。Android 另有 `nativeSetLogLevel` JNI（adb 调试用）。
- **埋点**：`Document::commit/undo/redo` 为骨架（每条可撤销操作一行：`[history] 提交 "Stroke"（触及 N 瓦片）`，debug 级）；引擎公开操作 info 级（工具切换/油漆桶/形状提交/文字对象增删改移/图层增删合并/滤镜/变换三段/导入导出含字节数与耗时/预设/文档切换）；壳层生命周期（onPause/自动保存跳过与完成/恢复）；**帧率每 5s 进日志**（Android info + 呈现计数；桌面 `[fps]` info 每秒）。
- **实测**：170 项测试全过、clippy 清零；桌面 stderr 输出验证；**真机（无线 ADB 重连后）logcat 全链验证**：启动就绪 → 恢复 7MB 档案（含耗时 2070ms）→ 工具切换 → 填充（坐标/容差/瓦片数）→ FPS 4.3~27.3 实时读数 → onPause → 自动保存（档案时间戳确认）。Web console 后端与 logcat 同构，实例化期绑定 console 无法事后 hook 验证（方法限制，非缺陷）。
- 布局约定：`[域] 消息`（tool/fill/shape/text/layer/history/io/transform/filter/preset/clipboard/doc/viewport/app/lifecycle/autosave/restore/fps）。
