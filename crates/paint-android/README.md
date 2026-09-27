# paintEngine Android 壳

Rust 引擎（paint-core + paint-render）经 JNI 嵌入 Android。
Kotlin 侧只有一个 `PaintEngineView`（自定义 View）+ 演示 Activity。

## 构建

### 1. 交叉编译 Rust cdylib（需要 Android NDK）

```bash
# 工程仓库根目录执行
NDK=$HOME/Library/Android/sdk/ndk/27.2.12479018   # 按实际路径/版本调整
TARGET=aarch64-linux-android

CARGO_TARGET_${TARGET//-/_}_LINKER=$NDK/toolchains/llvm/prebuilt/darwin-x86_64/bin/${TARGET}24-clang \
  cargo build -p paint-android --release --target $TARGET

cp target/$TARGET/release/libpaint_android.so \
   crates/paint-android/android/app/src/main/jniLibs/arm64-v8a/
```

（x86_64 模拟器同理：target 换 `x86_64-linux-android`，jniLibs 换 `x86_64/`。
若已安装 cargo-ndk，可直接 `cargo ndk -t arm64-v8k -o <jniLibs> build --release -p paint-android`。）

### 2. 用 Android Studio 打开 `android/` 目录

直接 Sync + Run。首次打开会自动生成 gradle wrapper 与下载依赖。
无 Android Studio 时：`cd android && gradle assembleDebug`（需本机 gradle ≥ 8.7）。

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
- **保存**：演示页"保存"按钮导出 PNG 到系统相册 `Pictures/paintEngine`
  （透明背景、可见内容包围盒）

## 已知限制（M3 范围外）

- 笔倾斜（tilt）未接入（`nativePointer` 已留参数位）
- SurfaceView/TextureView 独立渲染线程（当前 View.onDraw 直绘，M3 后续视性能决定）
- 压感橡皮端（TOOL_TYPE_ERASER）已按工具类型转发，引擎侧自动切 dst-out 语义待 M4 工具系统
