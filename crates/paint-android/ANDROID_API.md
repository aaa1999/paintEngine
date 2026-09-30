# paintEngine Android 库 API

其他 Android 应用接入 paintEngine 引擎的完整参考。库的形态:
**一个 AAR(`com.paintengine.android:paint-engine`) = Kotlin 视图 `PaintEngineView`
+ Rust 引擎动态库 `libpaint_android.so`(arm64-v8a / x86_64)**。宿主零
androidx 依赖;引擎自绘图层/笔刷/文字/历史/导入导出全部在库内闭环。

```
┌────────── 宿主 App ──────────┐
│  Activity / Compose / XML    │
│  ┌────────────────────────┐  │
│  │ PaintEngineView (AAR)  │  │  控制面:工具/笔刷/图层/导入导出…
│  │  Kotlin 视图(输入翻译) │  │  回调:onTextAnchor / onTextEdit
│  └──────────┬─────────────┘  │
└─────────────┼────────────────┘
         JNI(RegisterNatives,46 个方法)
        ┌─────┴──────┐
        │ libpaint_  │  Rust:paint-core + paint-render
        │ android.so │  引擎状态机、软件渲染、.ora/PNG 编解码
        └────────────┘
```

## 快速接入

### 方式 A:AAR 依赖(推荐)

```bash
cd crates/paint-android/android
./gradlew :library:publishToMavenLocal   # 产出 AAR(自动先编 .so)
```

宿主 `settings.gradle.kts` 加 `mavenLocal()`,然后:

```kotlin
dependencies {
    implementation("com.paintengine.android:paint-engine:0.1.0")
}
```

布局里直接用(`com.paintengine.android.PaintEngineView`),或代码构造。
**View 类名/包名是库身份的一部分,随 AAR 固定**——宿主不必也不能改它。

### 方式 B:源码模块依赖(同一构建内)

```kotlin
// settings.gradle.kts
include(":paintengine")
project(":paintengine").projectDir =
    File("../paintEngine/crates/paint-android/android/library")
// 宿主
dependencies { implementation(project(":paintengine")) }
```

### 方式 C:fork 改包名

类名不再绑定 JNI 符号(见「JNI 桥」节),改包名只需两步:

1. 改 `PaintEngineView.kt` 的 `package`(以及 `R` 引用,若有);
2. 改 Rust 侧 `crates/paint-android/src/lib.rs` 的 `JNI_CLASS` 常量,
   `./gradlew :library:buildRust` 重编 `.so`。

## 构建与发布

```bash
cd crates/paint-android/android

./gradlew assembleDebug            # 演示 APK(自动触发 Rust 构建)
./gradlew :library:buildRust       # 只重编 .so → library/src/main/jniLibs/
./gradlew :library:assembleRelease # 只出 AAR
./gradlew :library:publishToMavenLocal
```

`.so` 由 `buildRust` 任务调 **cargo-ndk** 交叉编译(NDK 由
`local.properties` 的 `sdk.dir` 自动定位),随 `preBuild` 自动联动。
策略用 Gradle 属性控制:

| `-PpaintEngine.rustBuild=` | 行为 |
|---|---|
| `auto`(默认) | 工具链齐备(cargo + cargo-ndk + rustup 双 target)才编,否则沿用 jniLibs 既有 `.so` |
| `on` | 强制编译;缺工具链直接失败(CI 推荐) |
| `off` | 跳过(纯 Kotlin 改动提速) |

工具链安装:

```bash
rustup target add aarch64-linux-android x86_64-linux-android
cargo install cargo-ndk
```

发布远端 Maven:`publishToMavenLocal` 换 `publish`(在 `library/build.gradle.kts`
的 `publishing` 块配置 `repositories`)。版本号改同文件 `version` 字段。

## PaintEngineView API 参考

### 生命周期与线程规则

- View 构造即 `nativeCreate()` 引擎,`onDetachedFromWindow()` 自动销毁——
  **宿主不需要手动管理引擎生命周期**;
- 每个 View 实例独享一个引擎实例,多 View 共存互不干扰;
- **所有方法必须在 UI 线程调用**(引擎句柄非线程安全);
- 尺寸/焦点/触摸由 View 自动处理,宿主只调「控制面」;
- 画布坐标 = 视图物理像素坐标,缩放/旋转由引擎视口内部换算;
- **冷启动提示**(演示 app 实测):`loadOra`/`setTextFont` 等重操作
  (大字体解析、存档解码可达秒级)不要放在 `onCreate` 首帧路径——
  文件读放后台线程,解码经 `View.post` 延后到首帧之后;启动窗口
  背景应与首帧布局同色(见 `app/src/main/res/drawable/launch_background.xml`)
  以消除白屏感。分段计时可参照 MainActivity 的 `[startup]` 日志。

### 工具码常量(`PaintEngineView` 伴生对象)

| 常量 | 值 | 说明 |
|---|---|---|
| `TOOL_BRUSH` | 0 | 笔刷(默认) |
| `TOOL_ERASER` | 1 | 橡皮 |
| `TOOL_MASK` | 2 | 蒙版 |
| `TOOL_LINE` / `TOOL_LINE_FILL` | 3 / 6 | 直线(描边/填充) |
| `TOOL_RECT` / `TOOL_RECT_FILL` | 4 / 7 | 矩形 |
| `TOOL_ELLIPSE` / `TOOL_ELLIPSE_FILL` | 5 / 8 | 椭圆 |
| `TOOL_TEXT` | 9 | 文字(走回调协议,见下) |
| `TOOL_FILL` | 10 | 油漆桶(容差 32) |

### 工具与笔刷

| 方法 | 说明 |
|---|---|
| `setToolCode(code: Int)` | 切工具(上表常量) |
| `setToolEraser(eraser: Boolean)` | 笔刷/橡皮快捷切换 |
| `setBrushSize(size: Float)` | 直径,画布像素,1..512 自动夹取 |
| `setBrushColor(r: Int, g: Int, b: Int)` | 0..255 自动夹取 |
| `presetNames(): List<String>` | 预设名(内置 + 用户自定义) |
| `applyPreset(name: String): Boolean` | 按名应用笔刷预设 |

### 图层

| 方法 | 说明 |
|---|---|
| `addLayer(): Boolean` | 当前层之上新建并选中 |
| `mergeDown(): Boolean` | 当前层向下合并 |
| `flatten(): Boolean` | 全部合并为单层 |
| `layerCount(): Int` | 图层数 |
| `activeLayerIndex(): Int` | 选中下标,无文档 -1 |
| `selectLayerIndex(index: Int): Boolean` | 按下标选中(0 起,底部为 0) |
| `layerNameAt(index: Int): String?` | 图层名,越界 null |

### 文字(回调协议)

文字工具要求宿主提供输入框,协议分「新建」与「编辑」两条路径:

```kotlin
view.onTextAnchor = { canvasX, canvasY ->
    // 空白处抬手:新文字锚点(画布坐标),弹输入框后:
    view.addTextObject(text, canvasX, canvasY, sizePx)
}
view.onTextEdit = { text, size, colorHex ->
    // 点击命中已有文字对象:预填编辑框,确认后:
    view.updateTextObject(newText, newSizePx)
    // 或 view.deleteTextObject()
}
```

| 方法 | 说明 |
|---|---|
| `setTextFont(font: ByteArray)` | 字体字节(如从 `/system/fonts` 读的 .ttf);**启动时调一次**,文字对象光栅化用 |
| `addTextObject(text, x, y, size): Boolean` | 新增非破坏、可重编辑文字对象(y 为基线锚点) |
| `updateTextObject(text, size): Boolean` | 更新最近命中的对象 |
| `deleteTextObject(): Boolean` | 删除最近命中的对象 |
| `drawText(font, text, x, y, size): Boolean` | 即时落墨(压平,不走对象系统) |

### 视口

| 方法 | 说明 |
|---|---|
| `fitToContent()` | 适配可见内容(留边距) |
| `zoom100()` | 缩放复位 100% |
| `setShowGrid(show: Boolean)` | 网格显隐 |

双指平移/缩放由引擎内置手势状态机处理(误触笔画自动回滚),宿主无需接线。

### 导入导出与工程

| 方法 | 说明 |
|---|---|
| `exportPng(): ByteArray?` | PNG 字节(可见内容包围盒、透明背景) |
| `importPng(data: ByteArray): Boolean` | PNG → 新图层 |
| `importImage(data: ByteArray): Boolean` | PNG/JPEG/WebP/SVG 自动识别 → 新图层 |
| `pasteRgba(rgba, w, h): Boolean` | 直行 RGBA(如 PDF 壳层渲染页)→ 浮动层放置 |
| `saveOra(): ByteArray?` | 存 `.ora` 工程(带图层)字节 |
| `loadOra(data: ByteArray): Boolean` | 载入 `.ora` 替换当前文档 |

### 浮动变换(附件放置)

`pasteRgba`/导入附件后进入放置模式:单指拖拽由 View 自动驱动平移,
宿主接其余操作:

| 方法 | 说明 |
|---|---|
| `isTransforming(): Boolean` | 是否处于放置模式 |
| `transformRotate(deltaDeg: Double)` | 旋转增量(度) |
| `transformScale(factor: Double)` | 缩放因子(>1 放大) |
| `commitTransform(): Boolean` | 提交落墨 |
| `cancelTransform(): Boolean` | 取消丢弃 |

### 监控与诊断

| 属性/方法 | 说明 |
|---|---|
| `editCount(): Long` | 编辑计数(自动保存脏检查:与上次保存不同即脏) |
| `renderCount(): Long` | 呈现帧计数(采样差值算帧率) |
| `setFpsMonitor(on: Boolean)` | 帧率监控开关 |
| `currentToolCode: Int` | 当前工具码镜像 |
| `engineHandle: Long` | 引擎句柄(诊断;勿缓存跨 View) |

logcat 过滤 tag `paintEngine`;日志级别经 `nativeSetLogLevel`(预留接口,
可从 adb 侧调试工具调用)。

## JNI 桥(RegisterNatives)

`.so` **不导出任何 `Java_<类全名>_<方法>` 名字混淆符号**(可用
`nm -gD libpaint_android.so | grep Java_` 验证,应为 0 条)。取而代之:

- 库加载时 `JNI_OnLoad` 把 46 个方法经 `RegisterNatives` 一次性注册到
  `JNI_CLASS`(`"com/paintengine/android/PaintEngineView"`)——类名在 Rust
  侧只出现这一个常量;
- Kotlin `external fun` 声明不变,调用方式与以前完全一致;
- 注册表在 `native_methods()`(`crates/paint-android/src/lib.rs`),名字与
  JNI 描述符须与 Kotlin 逐字对应;单测 `method_table_well_formed` 做结构
  完整性检查(数量 46、名字唯一、指针非空、描述符形状);
- 签名不匹配时 `RegisterNatives` 失败 → `System.loadLibrary` 抛
  `UnsatisfiedLinkError`,同时 logcat 有 `RegisterNatives 失败` 明细。

描述符清单(与 `native_methods()` 一一对应,便于核对 Kotlin 声明):

```
nativeCreate ()J                          nativeUndo (J)Z
nativeDestroy (J)V                        nativeRedo (J)Z
nativeResize (JIIF)V                      nativeAddLayer (J)Z
nativePointer (JIIDDDDDIJ)V              nativeMergeDown (J)Z
nativePenInRange (JZ)V                    nativeFlatten (J)Z
nativeFocus (JZ)V                         nativeLayerCount (J)I
nativeRender (JLandroid/graphics/Bitmap;)Z nativeActiveLayerIndex (J)I
nativeSetTool (JI)V                       nativeSelectLayerIndex (JI)Z
nativeTakeTextAnchor (J)[F                nativeLayerNameAt (JI)Ljava/lang/String;
nativeDrawText (J[BLjava/lang/String;DDD)Z nativeFitToContent (J)V
nativeSetTextFont (J[B)V                  nativeZoom100 (J)V
nativeAddTextObject (JLjava/lang/String;DDD)Z nativeSetShowGrid (JZ)V
nativeTakeTextEdit (J)[Ljava/lang/String;  nativeExportPng (J)[B
nativeUpdateTextObject (JLjava/lang/String;D)Z nativeImportPng (J[B)Z
nativeDeleteTextObject (J)Z               nativeImportImage (J[B)Z
nativeEditCount (J)J                      nativePasteRgba (J[BII)Z
nativeRenderCount (J)J                    nativeTransforming (J)Z
nativeSetFpsMonitor (JZ)V                 nativeTransformTranslateScreen (JDD)V
nativeSaveOra (J)[B                       nativeTransformRotate (JD)V
nativeLoadOra (J[B)Z                      nativeTransformScale (JD)V
nativeSetBrushSize (JD)V                  nativeCommitTransform (J)Z
nativeSetBrushColor (JIII)V               nativeCancelTransform (J)Z
nativePresetNames (J)[Ljava/lang/String;  nativeApplyPreset (JLjava/lang/String;)Z
```

> ABI 注意:JNI 参数类型必须两侧严格一致(Kotlin `Float` ↔ Rust `jfloat`)。
> 历史上 `nativeResize` 的 `scale` 曾是 Kotlin `Float`/Rust `jdouble` 错位
> (寄存器解读错误、density 失真),RegisterNatives 化时已修正并以此清单锁定。

## 注意事项与已知限制

- 渲染走 `View.onDraw` + `AndroidBitmap_lockPixels` 直写 Bitmap(预乘
  ARGB_8888 零拷贝),无独立渲染线程;SurfaceView 路线在 M3 后评估;
- `MotionEvent` 批量历史点已展开,高频笔输入不丢采样;压感/倾斜/橡皮头
  按工具类型自动分流;
- minSdk 26;`.so` 目前只编 `arm64-v8a` + `x86_64`(改 `library/build.gradle.kts`
  的 `rustAbis` 可加 armv7/x86);
- 引擎句柄是裸指针往返,**严格单 UI 线程**;
- AAR 未做 ProGuard 规则——View 是运行时反射加载 native 的入口
  (`System.loadLibrary` 在伴生对象),宿主混淆时请 keep
  `com.paintengine.android.PaintEngineView`。
