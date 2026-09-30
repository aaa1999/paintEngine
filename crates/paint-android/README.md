# paintEngine Android 壳

Rust 引擎（paint-core + paint-render）经 JNI 嵌入 Android，以 **AAR 库**
形式对外交付。Kotlin 侧只有一个 `PaintEngineView`（自定义 View）；演示
Activity 在 `app/` 模块，库本体在 `library/` 模块。

**宿主应用接入方法、完整 API 参考、JNI 描述符清单见 [ANDROID_API.md](./ANDROID_API.md)。**

## 模块结构

```
crates/paint-android/
├── src/lib.rs            # Rust JNI 壳（JNI_OnLoad + RegisterNatives）
├── ANDROID_API.md        # 库 API 文档（接入方读这个）
└── android/
    ├── library/          # com.android.library → paint-engine AAR
    │   ├── .../PaintEngineView.kt
    │   └── src/main/jniLibs/{arm64-v8a,x86_64}/   # .so（cargo-ndk 产出，gitignore）
    └── app/              # 演示宿主（依赖 :library）
        └── .../MainActivity.kt
```

## 构建

### 1. 交叉编译 Rust cdylib（自动）

`.so` 由 Gradle 任务 `buildRust`（调 cargo-ndk）产出，随 `preBuild` 自动
联动，无需手动拷贝。工具链一次性安装：

```bash
rustup target add aarch64-linux-android x86_64-linux-android
cargo install cargo-ndk
```

无 Rust 工具链时默认（`auto` 模式）沿用 jniLibs 既有 `.so`，纯 Kotlin
改动不受影响。策略开关：`-PpaintEngine.rustBuild=auto|on|off`。

手动路径（等价）：

```bash
cargo ndk -t arm64-v8a -t x86_64 \
  -o crates/paint-android/android/library/src/main/jniLibs \
  build --release -p paint-android
```

### 2. 构建 APK / 发布 AAR（已含 gradle wrapper，无需 Android Studio）

```bash
cd crates/paint-android/android
./gradlew assembleDebug                    # 演示 APK（自动先编 .so）
./gradlew :library:publishToMavenLocal     # 发布 AAR 到 mavenLocal
adb install -r app/build/outputs/apk/debug/app-debug.apk
adb shell am start -n com.paintengine.android/.MainActivity
```

工具链组合：Gradle 9.7 + AGP 9.0（内置 Kotlin 支持，无需单独 kotlin 插件）+
compileSdk 35 + 纯框架 UI（无 androidx 运行时依赖；双 ABI APK ≈ 5.6MB，
单 ABI 实装 ≈ 3MB）。
`local.properties` 指向本机 SDK 路径（已 gitignore，按需修改）。
Android Studio 打开 `android/` 目录亦可直接 Run。

## 行为说明

- **呈现**：`onDraw` → `nativeRender` 经 `AndroidBitmap_lockPixels` 把引擎帧
  直接写入 ARGB_8888 Bitmap（预乘一致，零中间拷贝）
- **输入**：`MotionEvent` 全量翻译——`ACTION_MOVE` 先展开
  `getHistorical*` 批量历史点再发当前采样（高频输入不丢点）；
  压感取 `getPressure`（笔/橡皮），手指/鼠标传无压感
- **笔悬停**：`ACTION_HOVER_*` + `TOOL_TYPE_STYLUS` → `PenInRange`
  （引擎据此做手掌拒绝）
- **手势**：双指平移缩放由引擎内置状态机处理（误触笔画自动回滚），
  View 层只转发原始触摸事件
- **JNI 绑定**：`JNI_OnLoad` + `RegisterNatives` 注册表（46 方法），
  不导出名字混淆符号——改包名只动 `JNI_CLASS` 一个常量
- **保存**：演示页"保存"按钮导出 PNG 到系统相册 `Pictures/paintEngine`
  （透明背景、可见内容包围盒）

## 已知限制（M3 范围外）

- SurfaceView/TextureView 独立渲染线程（当前 View.onDraw 直绘，M3 后续视性能决定）
- `.so` 仅编 arm64-v8a + x86_64（`library/build.gradle.kts` 的 `rustAbis` 可扩）
