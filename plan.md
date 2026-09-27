# paintEngine 开发计划

跨平台绘画引擎内核，Rust 实现。本文档记录已定的架构决策、API 蓝图与里程碑，作为开发的基准文档；后续调整直接改这里，并以 git 提交历史追踪决策演变。

## 一、已定决策

| 决策点 | 结论 | 影响 |
|--------|------|------|
| 引擎定位 | 绘画应用内核 | 图层、笔刷、撤销是一等公民；2D 绘图 API 是内部手段 |
| 目标平台 | 桌面三端 + Web/WASM + iOS + Android | 四个平台壳层；输入抽象需覆盖压感笔/触摸/鼠标 |
| 渲染后端 | Renderer trait 双后端 | 软件实现（tiny-skia）先行，wgpu 实现并进 |
| 画布模型 | 无限平铺画布 | 稀疏瓦片存储；增量撤销；内存治理是常态课题 |

核心原则：**引擎核心只负责"算出像素"（瓦片），平台层只负责"呈现像素 + 喂事件"**。`paint-core` 不知道屏幕的存在。

## 二、架构分层与 Crate 布局

```
paintEngine/  (workspace)
├── crates/
│   ├── paint-core/        # 零平台依赖：瓦片/图层/笔画/撤销/文档/视口/PNG
│   ├── paint-render/      # Renderer trait + 软件实现（手写盖章/合成/合并）
│   ├── paint-gpu/         # Renderer 的 wgpu 实现（feature 门控，M2 尾/M3）
│   ├── paint-desktop/     # winit + softbuffer/wgpu
│   ├── paint-wasm/        # canvas 元素 + 指针事件，wasm-bindgen 导出（+www/ 演示页）
│   ├── paint-ios/         # C ABI staticlib，Swift 薄壳调用（M3）
│   └── paint-android/     # cdylib + JNI，Kotlin View 子类调用（M3）
```

（实际落地为扁平 crates/ 布局，与初版蓝图的 shells/ 子目录等价。）

## 三、依赖选型

现成轮子只用于**光栅化与平台接入**两层；引擎核心（瓦片/图层/笔刷/撤销）全部手写——Rust 生态没有现成的平铺绘画引擎内核，这部分即本项目本体。

### 选用清单

| Crate | 引入时机 | 用在哪 | 干什么 | 选择理由 |
|---|---|---|---|---|
| `tiny-skia` | M1 | paint-render | 软件光栅化：路径填充/描边、抗锯齿、图像采样 | Skia 子集的纯 Rust 移植，resvg 作者维护，无 C 依赖，四端交叉编译无痛 |
| `winit` | M1 | paint-desktop | 桌面窗口 + 事件循环 | 桌面三端窗口事实标准 |
| `softbuffer` | M1 | paint-desktop / paint-wasm | 把 CPU 像素缓冲呈现到窗口/canvas | 纯 CPU 路线不碰 GPU API 即可上屏 |
| `wgpu` | M2 | paint-gpu | GPU 抽象层：合成走 Vulkan/Metal/DX12/WebGPU | Rust 图形事实标准，桌面到 wasm 一套 API |
| `wasm-bindgen` + `web-sys` | M2 | paint-wasm | JS 互操作、canvas 元素、指针事件 | wasm 事实标准 |
| `png` | M2 | IO | PNG 编解码 | 只需 PNG，不拖整个 image 生态 |
| `zip` | M4 | 存档 | OpenRaster(.ora) 容器读写 | .ora 本质是 zip + XML + 每层一个 PNG |

M1 实际引入 `winit` / `softbuffer` 两个（软件渲染器为手写实现，未引入 tiny-skia；tiny-skia 随 P1 路径光栅化 `rasterize` 落地）。

### 考虑过但放弃

| 方案 | 放弃理由 |
|---|---|
| `skia-safe`（Skia 官方绑定） | 功能最全，但带巨大 C++ 构建链，iOS/Android/wasm 交叉编译痛苦，与"四端一份核心"目标冲突 |
| `raqote` | 同为纯 Rust 光栅化，但维护强度弱于 tiny-skia |
| `vello` / `lyon` | GPU 矢量渲染路线；本项目矢量需求占小头，合成与盖章才是主战场 |
| `femtovg` | OpenGL 系 GPU 画布，后端覆盖不如 wgpu，定位偏 UI 而非位图合成 |
| `image` | 全家桶过重，只需 PNG 就引 `png` |
| `glam` / `euclid` | 变换矩阵与 2D 几何量小且语义特殊（预乘 alpha、瓦片坐标），手写更可控 |

文字渲染库 `swash` vs `cosmic-text` 的选型推迟到 M4（见"暂缓与待议"）。

## 四、API 蓝图

### 1. 瓦片与文档（paint-core, P0）

```rust
pub const TILE: u32 = 256;                      // RGBA8 预乘，每瓦 256KB
pub struct TileId { x: i32, y: i32 }

pub struct TileGrid {                           // 稀疏哈希：TileId → 瓦片
    fn get_or_create(&mut self, id: TileId) -> &mut Tile;
    fn prune(&mut self);                        // 回收全透明瓦片（内存治理）
    fn content_bounds(&self) -> Option<Rect>;   // 非空包围盒，导出/缩放适配用
}

pub struct Document {
    fn layers(&self) -> &LayerStack;
    fn viewport(&self) -> &Viewport;
    fn active_layer(&self) -> LayerId;
    fn undo(&mut self) -> bool;
    fn redo(&mut self) -> bool;
}
```

### 2. 图层栈（P0 基础 / P1 全量）

```rust
impl LayerStack {
    fn insert(&mut self, above: Option<LayerId>) -> LayerId;   // P0
    fn remove/duplicate/merge_down(&mut self, id: LayerId);    // P1
    fn reorder(&mut self, id: LayerId, to: usize);             // P1
    fn flatten(&self) -> TileGrid;                             // P1，导出用
}
pub struct Layer { opacity: f32, blend_mode: BlendMode /* P0 仅 Normal */,
                   visible: bool, name: String, tiles: TileGrid }
```

### 3. Renderer trait（paint-render, P0 定义 + 软件实现；wgpu P1/P2）

```rust
pub trait Renderer: Send {
    /// 笔刷盖章热路径：把 dab 序列合成进瓦片
    fn stamp_dabs(&mut self, tiles: &mut TileGrid, dabs: &[Dab], brush: &StampBrush);
    /// 矢量绘制：路径填充/描边（套索填充、形状工具）
    fn rasterize(&mut self, tiles: &mut TileGrid, ops: &[DrawOp]);
    /// 合成：可见瓦片 × 图层栈 × 视口 → 目标 surface，只处理脏区
    fn composite(&mut self, doc: &Document, target: &mut SurfaceDesc, dirty: Option<Rect>);
}
```

### 4. 笔刷与输入（paint-core, P0）

```rust
pub struct PointerSample {
    x: f64, y: f64,                    // 视口坐标，引擎负责换算画布坐标
    pressure: Option<f32>,             // 归一化 0..1
    tilt: Option<(f32, f32)>,
    kind: PointerKind,                 // Pen | Eraser | Touch | Mouse
    t_us: u64,                         // 时间戳：平滑、速度、补采都靠它
}

pub struct Dab { x: f64, y: f64, radius: f32, hardness: f32,
                 color: Color, alpha: f32, mode: DabMode }   // Wash | Buildup

/// 采样流 → dab 流：间距切分、压感映射、平滑都在这层（可替换）
pub trait StrokeGen { fn begin/extend/end(...) -> Vec<Dab>; }

pub trait Tool { fn on_pointer(&mut self, ctx, ev) -> ToolResponse; }
// P0 内置：Pen(圆头笔) + Pan；P1：Eraser(同笔刷、dst-out 合成)
```

### 5. 视口（paint-core, P0 平移缩放 / P2 旋转翻转）

```rust
pub struct Viewport {
    fn pan_by(&mut self, dx: f64, dy: f64);
    fn zoom_at(&mut self, anchor: (f32, f32), factor: f32);
    fn screen_to_canvas(&self, p: (f32, f32)) -> (f64, f64);
}
```

### 6. 平台抽象（核心定义 trait，各壳实现, P0）

```rust
pub enum PlatformEvent {
    Pointer { phase: PointerPhase, sample: PointerSample },
    PenInRange(bool),        // 手掌拒绝：笔在感应区时忽略触摸
    Resize { w: u32, h: u32, scale: f32 },
    Focus(bool),             // 失焦取消进行中的笔画
}
pub trait Surface {
    fn present_cpu(&mut self, rgba_premul: &[u8], dirty: Option<Rect>);
    fn texture_target(&mut self) -> Option<&mut GpuTarget>;
}
impl Engine {
    fn new(renderer: Box<dyn Renderer>, config: EngineConfig) -> Self;
    fn handle_event(&mut self, ev: PlatformEvent);
    fn render(&mut self, surface: &mut dyn Surface);   // 只重合成脏区
    fn document_mut(&mut self) -> &mut Document;       // 图层管理等走这
}
```

### 7. 撤销（paint-core, P0）

瓦片用 `Arc` 共享、写入时写时复制（COW）。一笔 = 一个撤销组，只记录脏瓦片的旧 `Arc` 快照，撤销近乎免费；按**内存限额**淘汰历史（无限画布不能按步数限）。

```rust
impl History {
    fn begin_group(&mut self, label: &'static str);
    fn record_tiles(&mut self, before: Vec<(LayerId, TileId, Arc<TileData>)>);
    fn set_memory_limit(&mut self, bytes: usize);
}
```

### 8. IO（P1/P2）

```rust
pub fn export_png(doc: &Document, bounds: Option<Rect>, scale: f32) -> Vec<u8>;
pub fn import_image_as_layer(doc: &mut Document, png: &[u8]) -> LayerId;
// 分层存档采用 OpenRaster (.ora) 标准格式（P2），不自造格式
```

## 五、关键设计判断

- **双后端切分顺序**：wgpu 先做"合成"（`composite`），笔刷盖章（`stamp_dabs`）仍走 CPU。GPU 盖章需要 compute + 避免回读，复杂度高一个量级；CPU 盖章 + GPU 合成的混合形态已能拿到大画布流畅滚动的收益。
- **输入归一化的坑**：Web 端需要 `pointerrawupdate` 拿高采样率输入；Android 的 `MotionEvent` 必须展开 `getHistorical*` 批量历史点。这就是 `PointerSample.t_us` 存在的原因——两个平台都会一次给一串不同时间戳的采样。
- **坐标分工**：平台事件只带视口坐标，对画布坐标系一无所知；`Viewport` 负责全部换算。旋转、翻转以后只动这一个模块。
- **颜色**：全链路 RGBA8 预乘 alpha，sRGB；16 位深色 P3 以后再议。

## 六、里程碑

### M1（P0）：桌面可画、可撤销 ✅（2026-09-27）

- [x] Cargo workspace 搭建：paint-core / paint-render / paint-desktop 三个 crate 骨架
- [x] TileGrid：稀疏存储、get_or_create、prune、content_bounds
- [x] COW 瓦片 + History（撤销组、内存限额淘汰）
- [x] LayerStack 基础：insert / opacity / visible / blend(Normal)
- [x] Renderer trait + 软件实现：stamp_dabs（圆头 dab、硬度、压感）、composite（脏区）
- [x] RoundBrush（size/hardness/opacity/flow/spacing）+ 压感映射
- [x] Viewport：平移、缩放、坐标换算
- [x] Engine 骨架：handle_event / render / undo / redo
- [x] 桌面壳：winit + softbuffer，鼠标输入，呈现 CPU 帧缓冲
- [x] 验收：绘画-撤销-重做全循环、合成压感笔宽变化、平移缩放重绘均有自动化测试覆盖（35 项测试全绿）

实现备注（与蓝图的偏差与补充）：

- **Renderer trait 定义在 paint-core**（消费方）而非 paint-render，保证引擎不依赖具体渲染实现；paint-render 提供 SoftwareRenderer。
- **M1 未引入 tiny-skia**：dab 盖章与脏区合成为手写标量循环（最近邻采样、预乘 alpha、瓦片行内缓存），路径光栅化需求（P1 套索/形状工具）落地时再引入。
- **桌面端无真实压感**：winit 不透传数位笔压感数据。压感链路（PointerSample.pressure → 笔宽变化）已由 e2e 合成压感测试验证；真实压感随 M2（Web Pointer Events）与 M3（iOS/Android）落地。
- Viewport 带 revision 版本号：壳层直接改视口（拖拽平移等）时引擎检测到版本变化自动全量重绘。
- UndoGroup 按内存限额近似记账（跨组共享 Arc 重复计，宁多勿少），淘汰保留至少一组。

运行：`cargo run -p paint-desktop --release`（左键绘画 · 中键/空格拖拽平移 · 滚轮缩放 · Ctrl+Z 撤销 · [ ] 笔刷大小）

### M2（P1）：完整绘画应用（Web 优先）✅（2026-09-27，wgpu 单列遗留）

- [x] 12 种混合模式（Multiply/Screen/Overlay/SoftLight/…）
- [x] 橡皮擦（dst-out）：Dab.erase + Engine::Tool；桌面 E 键/右键临时擦
- [x] 图层全量：remove / duplicate / reorder / merge_down / flatten（撤销扩展为 UndoOp 枚举，结构操作全可撤销）
- [x] PNG 导入导出：io.rs 预乘↔直行换算；export_png 支持自定义范围/缩放/透明背景
- [x] wasm 壳：canvas 元素呈现（ImageData）、pointerrawupdate 高采样输入、ResizeObserver（DPR 感知）、rAF 按需渲染、www/ 演示页
- [x] 多指手势：双指平移缩放（防瞬时重合缩放跳变）+ 误触笔画即时回滚 + 手势闩锁 + 笔接管 + 手掌拒绝（PenInRange）
- [x] History 内存限额接入 prune：淘汰时 Arc 释放自动生效；笔画结束回收空瓦片；结构操作按瓦片数近似记账
- [x] wgpu composite 实现（2026-09-27，M2.5 完成）：`paint-gpu` crate 提供 `WgpuRenderer`——CPU 盖章 + GPU 合成混合形态（盖章/合并委托软件路径）；瓦片纹理缓存按 Arc 指针身份增量上传、ping-pong 累积纹理跨帧保留支持脏区增量、12 种混合模式 WGSL 移植、点阵网格解析式着色、结果回读进引擎 CPU 帧缓冲（引擎与全部壳层零改动）；桌面端 `--features gpu` 启用（初始化失败自动回退软件）。验收：GPU↔CPU 合成逐像素对齐 5 项 parity 测试（全混合模式 ±2 / 双线性 ±12 / 网格 ≤1 点边界翻转 / 透明导出 / 脏区增量一致）。对齐过程修掉 CPU 双线性半像素锚点偏差（对齐标准纹理语义，Web/桌面 CPU 路径同步受益）
- [x] 验收：67 项自动化测试覆盖绘画全流程（含混合模式像素断言、合并/压平往返不变、PNG 往返逐像素一致、手势 e2e）；浏览器手动验收见 `crates/paint-wasm/README.md`（wasm32 编译通过，浏览器运行需 wasm-pack 构建）

实现备注：

- 混合模式公式取自 W3C Compositing and Blending Level 1，Normal 走快速路径，其余 11 种走通用 `composite_pixel`（预乘域、支持半透明底，merge 与屏幕合成共用）。
- 合并（merge_down/flatten）通过 `Renderer::merge_layers` 瓦片对瓦片 1:1 完成，隐藏图层不贡献像素（与主流软件一致）。
- 结构撤销：UndoOp 枚举（Tiles/InsertLayer/RemoveLayer/MoveLayer），逆序应用、逆操作自动捕获，redo 重放前向顺序。
- 手势状态机在引擎内（壳层只转发原始触摸事件），桌面/移动端行为一致。

### M2.7 无限画布体验（2026-09-27）

引擎核心自 M1 起即为无限画布（稀疏瓦片、负坐标、无界平移），本轮补全用户可感知的部分：

- [x] 点阵网格：空白区画布空间锚定的网格点（平移随动，传达空间感），间距在 256 倍数中自适应（屏幕间距 ≥ 32px），颜色随背景亮度取柔和对比；导出永远不带网格；`show_grid` 开关默认开（G 键 / 工具栏按钮）
- [x] 适应内容（fit-to-content）：`TileGrid::content_bounds_precise` 像素精确包围盒 + `Viewport::fit_to` 内容中心对齐屏幕中心；平移迷路后的"回家"操作（Ctrl+0 / 适应内容按钮 / Android 适配按钮）；空画布 fit → 原点居中
- [x] 100% 缩放（保持屏幕中心）：Ctrl+1
- [x] Web 壳鼠标导航补全：滚轮锚点缩放、空格+左键/中键拖拽平移（此前 Web 端鼠标用户无法平移缩放）
- [x] 浏览器实测：60 网格点精确命中、滚轮缩放生效、fit 后内容中心 (636.5,332) ≈ 屏幕中心 (640,337.5)、网格开关生效

### M3（P1 后半）：iOS / Android（Android 已完成，iOS 待议）

- [ ] paint-ios：C ABI staticlib + Swift 薄壳（UIView 子类、Pencil 事件、Metal/CPU 呈现）——**待议**：方案与时点由后续讨论决定
- [x] paint-android：cdylib + JNI + Kotlin View 子类、MotionEvent 历史点展开（2026-09-27）
- [x] Android 真机验收（2026-09-27）：APK（2.5MB，双架构引擎）安装至联想 23049RAD8C 真机与 x86_64 模拟器——启动无崩溃、点阵网格渲染、触摸绘画、撤销、保存 PNG 到相册、适应内容（笔画中心 538,1136 ≈ 画布中心 540,1150）全部通过；真机手工体验项（手指/笔压感、双指手势、笔悬停手掌拒绝）待实际使用确认

Android 实现备注：

- 呈现：`AndroidBitmap_lockPixels` 直接写 ARGB_8888 Bitmap（预乘一致，按 stride 拷贝，零中间缓冲），`onDraw` 驱动。
- 输入：`ACTION_MOVE` 先展开 `getHistorical*` 批量历史点再发当前采样；笔/橡皮传 `getPressure` 压感，手指/鼠标传无压感；`ACTION_HOVER_*` + `TOOL_TYPE_STYLUS` → `PenInRange` 手掌拒绝；失焦自动收笔。
- 手势（双指平移缩放/误触回滚）由引擎内置状态机处理，View 层只转发原始事件——与桌面/Web 行为一致。
- demo：`crates/paint-android/android/`（Android Studio 直接打开），工具栏含保存 PNG 到系统相册。
- 构建：NDK 交叉编译 .so → 拷入 jniLibs（步骤见 crate README）；.so 不入库。

### M2.5 收尾补记（2026-09-27）

- 浏览器实测（内置自动化）通过：加载/DPR 画布/压感正弦笔迹/撤销重做/橡皮/换色/新图层/PNG 导出（554KB blob）。
- 实测修复三处 Web 壳缺陷：rAF 循环自引用断链（首帧 panic）、部分 webview 不派发 ResizeObserver（构造时同步 + 每帧廉价检查兜底）、个别 webview 不派发 rAF（16ms setInterval 兜底节拍）。
- 桌面壳按需重绘（事件驱动，空闲零开销）；合成器双线性采样（zoom>1，瓦片内钳位防接缝；1:1 与缩小保持最近邻）。

### M4（P2）：进阶能力

- [ ] GPU 盖章（compute shader dab）
- [ ] 图层蒙版 / 剪贴层
- [ ] 画布旋转 / 翻转
- [ ] 笔迹稳定器（平滑/磁吸）
- [ ] 纹理笔刷
- [ ] OpenRaster 分层存档读写
- [ ] 选区（套索/矩形）
- [ ] 文字工具与矢量形状

## 七、暂缓与待议

- **SVG 导入**（2026-09-27 评估）：走 resvg 光栅化导入——渲染到 RGBA8 预乘缓冲后复用既有 PNG 导入管线（bytes → 像素 → 瓦片 → 图层），约一天工作量，feature 门控（wasm 体积考量，usvg 文字需 fontdb 提供字体）。**推荐立项**。
- **SVG 导出**（2026-09-27 评估）：只有"笔迹记录式导出"有真实意义（引擎持有点序列与参数，可生成变宽轮廓填充路径）；软笔刷需模糊滤镜近似、buildup 自交叠加无法完美表达；且经橡皮/合并/导入修改的层已非"矢量干净"，需回退为内嵌 PNG。忠实 v1 约一周。**不建议现在做**——目的是可缩放输出的话 export_png 的 scale 参数已够；分层互通走 M4 的 OpenRaster；M4 矢量形状/文字工具落地时（自带矢量对象模型）SVG 导出才是架构上自然的时机。
- 文字渲染（字体库选型 swash vs cosmic-text）：M4 再定
- 自定义工程格式：倾向只用 OpenRaster，不另造
- 16 位色深 / 宽色域：有真实需求再立项
