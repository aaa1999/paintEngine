//! 桌面壳：winit 窗口 + softbuffer CPU 呈现。
//!
//! 交互约定（M1）：
//! - 左键拖动 = 画笔（winit 不透传数位笔压感，真压感随 M2/M3 平台落地）
//! - 中键拖动 或 空格+左键拖动 = 平移画布
//! - 滚轮 = 以光标为锚缩放
//! - Ctrl+Z / Ctrl+Shift+Z（或 Ctrl+Y）= 撤销 / 重做
//! - `[` / `]` = 缩小 / 放大笔刷

use std::num::NonZeroU32;
use std::sync::Arc;

use paint_core::input::{PointerKind, PointerPhase, PointerSample};
use paint_core::render::Surface;
use paint_core::{Engine, EngineConfig, PlatformEvent, Rect};
use paint_render::SoftwareRenderer;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::{Window, WindowId};

/// winit 0.30 的 Window 不是 Clone，且 rwh 0.6 没有 Arc blanket impl；
/// 用包装类型委托 handle trait，让 Context/Surface 能持有窗口共享权。
#[derive(Clone)]
struct SharedWindow(Arc<Window>);

impl HasDisplayHandle for SharedWindow {
    fn display_handle(
        &self,
    ) -> Result<winit::raw_window_handle::DisplayHandle<'_>, winit::raw_window_handle::HandleError>
    {
        self.0.display_handle()
    }
}

impl HasWindowHandle for SharedWindow {
    fn window_handle(
        &self,
    ) -> Result<winit::raw_window_handle::WindowHandle<'_>, winit::raw_window_handle::HandleError>
    {
        self.0.window_handle()
    }
}

/// PNG 字节 → arboard ImageData（直行 RGBA）。
fn image_from_png(png: &[u8]) -> Option<arboard::ImageData<'_>> {
    // copy_selection_png 输出直行 PNG；arboard 接受直行
    let mut rgba = Vec::new();
    let (w, h) = decode_png_straight(png, &mut rgba)?;
    Some(arboard::ImageData {
        width: w,
        height: h,
        bytes: std::borrow::Cow::Owned(rgba),
    })
}

fn decode_png_straight(png: &[u8], out: &mut Vec<u8>) -> Option<(usize, usize)> {
    // 复用 paint-core 的解码（预乘）→ 反预乘回直行
    let (premul, w, h) = paint_core::io::decode_png(png).ok()?;
    let mut straight = premul;
    for px in straight.as_chunks_mut::<4>().0 {
        let a = px[3] as u32;
        if a == 0 {
            continue;
        }
        for c in px.iter_mut().take(3) {
            *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
        }
    }
    *out = straight;
    Some((w as usize, h as usize))
}

mod panel;

fn init_logger() {
    let level = std::env::var("PAINT_LOG")
        .ok()
        .and_then(|v| v.parse::<log::LevelFilter>().ok())
        .unwrap_or(log::LevelFilter::Info);
    // LevelFilter 非 const 构造：用 OnceLock 重建
    struct CfgLogger(log::LevelFilter);
    impl log::Log for CfgLogger {
        fn enabled(&self, m: &log::Metadata) -> bool {
            m.level() <= self.0
        }
        fn log(&self, r: &log::Record) {
            if self.enabled(r.metadata()) {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default();
                let secs = now.as_secs() % 86400;
                eprintln!(
                    "{:02}:{:02}:{:02}.{:03} {:<5} [{}] {}",
                    secs / 3600,
                    (secs % 3600) / 60,
                    secs % 60,
                    now.subsec_millis(),
                    r.level(),
                    r.target(),
                    r.args()
                );
            }
        }
        fn flush(&self) {}
    }
    let _ = log::set_boxed_logger(Box::new(CfgLogger(level)))
        .map_err(|_| eprintln!("日志器已安装，忽略重复初始化"));
    log::set_max_level(level);
    log::info!("[app] 桌面壳启动（日志级别 {level}，PAINT_LOG 可调）");
}

fn main() {
    init_logger();
    let event_loop = winit::event_loop::EventLoop::new().unwrap();
    let mut app = App::new();
    app.load_presets_file();
    app.init_text_font();
    app.try_restore_autosave();
    event_loop.run_app(&mut app).unwrap();
}

/// 桌面自动保存文件路径。
fn autosave_path() -> std::path::PathBuf {
    let mut p = std::env::home_dir().unwrap_or_default();
    p.push(".paintengine_autosave.ora");
    p
}

/// 平台常见系统字体（CJK 优先）。swash 支持 ttf/otf/ttc（取集合首字体）。
fn load_system_font() -> Option<Vec<u8>> {
    const CANDIDATES: &[&str] = &[
        "/System/Library/Fonts/PingFang.ttc",                          // macOS 中文
        "/System/Library/Fonts/Supplemental/Arial.ttf",                // macOS 西文
        "/System/Library/Fonts/HiraginoSansCJK.ttc",                   // macOS 备选
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",      // Linux 中文
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",             // Linux 西文
        "C:\\Windows\\Fonts\\msyh.ttc",                                // Windows 雅黑
        "C:\\Windows\\Fonts\\arial.ttf",                               // Windows 西文
    ];
    for path in CANDIDATES {
        if let Ok(bytes) = std::fs::read(path) {
            return Some(bytes);
        }
    }
    None
}

#[derive(PartialEq)]
enum Drag {
    None,
    Stroke,
    Pan {
        last: (f64, f64),
    },
    Select {
        start: (f64, f64),
        op: paint_core::SelectionOp,
    },
    Float {
        last: (f64, f64),
    },
}

struct App {
    window: Option<Arc<Window>>,
    context: Option<softbuffer::Context<SharedWindow>>,
    surface: Option<softbuffer::Surface<SharedWindow, SharedWindow>>,
    engine: Engine,
    modifiers: ModifiersState,
    space_down: bool,
    drag: Drag,
    cursor: (f64, f64),
    t_us: u64,
    tool_before_erase: Option<paint_core::Tool>,
    /// 文字工具输入态：锚点（画布坐标）+ 已敲入文本 + 是否编辑已有对象。
    typing: Option<((f64, f64), String, bool)>,
    /// 自动保存脏检查基准。
    saved_edit_count: u64,
    /// 编辑已有文字对象时的原字号（提交时沿用）。
    text_size_hint: Option<f32>,
    /// 帧率打印节拍（上次输出时刻）。
    last_fps_print: Option<std::time::Instant>,
    /// 帧率采样基线（时刻, 呈现计数）。
    last_fps_sample: Option<(std::time::Instant, u64)>,
    /// 按需重绘：任何输入/焦点/尺寸事件置位，画完即清。
    needs_redraw: bool,
    layer_panel: panel::Panel,
    /// 多文档：后台文档池（当前文档在 engine 内）。
    docs: Vec<(String, paint_core::Document)>,
    doc_counter: u32,
}

impl App {
    fn new() -> Self {
        #[cfg(feature = "gpu")]
        let renderer: Box<dyn paint_core::render::Renderer> = {
            match paint_gpu::WgpuRenderer::new() {
                Some(r) => Box::new(r),
                None => {
                    eprintln!("wgpu 初始化失败，回退软件渲染");
                    Box::new(SoftwareRenderer::new())
                }
            }
        };
        #[cfg(not(feature = "gpu"))]
        let renderer: Box<dyn paint_core::render::Renderer> = Box::new(SoftwareRenderer::new());
        let engine = Engine::new(renderer, EngineConfig::default());
        Self {
            window: None,
            context: None,
            surface: None,
            engine,
            modifiers: ModifiersState::default(),
            space_down: false,
            drag: Drag::None,
            cursor: (0.0, 0.0),
            t_us: 0,
            tool_before_erase: None,
            typing: None,
            saved_edit_count: 0,
            text_size_hint: None,
            last_fps_print: None,
            last_fps_sample: None,
            layer_panel: panel::Panel::new(),
            needs_redraw: true,
            docs: Vec::new(),
            doc_counter: 1,
        }
    }

    fn pointer_sample(&mut self) -> PointerSample {
        self.t_us += 1;
        PointerSample {
            x: self.cursor.0,
            y: self.cursor.1,
            pressure: None,
            tilt: None,
            kind: PointerKind::Mouse,
            id: 0,
            t_us: self.t_us,
        }
    }

    fn on_resize(&mut self, w: u32, h: u32, scale: f32) {
        if let Some(sb) = self.surface.as_mut() {
            if let (Some(w), Some(h)) = (NonZeroU32::new(w), NonZeroU32::new(h)) {
                let _ = sb.resize(w, h);
            }
        }
        // 引擎只渲染画布区域（右侧留给面板）
        let engine_w = w.saturating_sub(panel::PANEL_W);
        if engine_w > 0 && h > 0 {
            self.engine.handle_event(PlatformEvent::Resize {
                w: engine_w,
                h,
                scale,
            });
        }
    }

    /// 数字键 1-9 快捷色板。
    fn set_palette(&mut self, idx: usize) {
        const PALETTE: [paint_core::Color; 9] = [
            paint_core::Color { r: 0, g: 0, b: 0 },
            paint_core::Color {
                r: 255,
                g: 255,
                b: 255,
            },
            paint_core::Color {
                r: 220,
                g: 50,
                b: 47,
            },
            paint_core::Color {
                r: 230,
                g: 145,
                b: 56,
            },
            paint_core::Color {
                r: 241,
                g: 196,
                b: 15,
            },
            paint_core::Color {
                r: 40,
                g: 167,
                b: 69,
            },
            paint_core::Color {
                r: 32,
                g: 201,
                b: 151,
            },
            paint_core::Color {
                r: 0,
                g: 123,
                b: 255,
            },
            paint_core::Color {
                r: 150,
                g: 68,
                b: 255,
            },
        ];
        let c = PALETTE[idx];
        self.engine.brush_mut().color = c;
        println!("颜色 #{:02X}{:02X}{:02X}", c.r, c.g, c.b);
    }

    fn presets_path() -> Option<std::path::PathBuf> {
        let home = std::env::var("HOME").ok()?;
        let mut p = std::path::PathBuf::from(home);
        p.push(".paintengine_presets.txt");
        Some(p)
    }

    /// 文字对象字体（系统 CJK 回退链）。
    fn init_text_font(&mut self) {
        if let Some(f) = load_system_font() {
            self.engine.set_text_font(f);
        } else {
            eprintln!("未找到系统字体：文字对象将不可渲染");
        }
    }

    /// 编辑计数检查点式自动保存（每 30 次编辑或退出时落盘）。
    fn autosave_checkpoint(&mut self) {
        if self.engine.edit_count() == self.saved_edit_count {
            return;
        }
        if let Some(bytes) = self.engine.save_ora() {
            if std::fs::write(autosave_path(), &bytes).is_ok() {
                self.saved_edit_count = self.engine.edit_count();
            }
        }
    }

    fn try_restore_autosave(&mut self) {
        if let Ok(bytes) = std::fs::read(autosave_path()) {
            if self.engine.load_ora(&bytes) {
                self.saved_edit_count = self.engine.edit_count();
                println!("已恢复上次自动保存（{} KB）", bytes.len() / 1024);
            }
        }
    }

    /// 帧率监控：1s 窗口计数差值（引擎无平台时钟，桌面用 Instant 采样）。
    fn maybe_print_fps(&mut self) {
        if !self.engine.fps_monitor_enabled() {
            return;
        }
        let now = std::time::Instant::now();
        let due = self
            .last_fps_print
            .is_none_or(|t| now.duration_since(t).as_secs() >= 1);
        if due {
            if let Some((t0, c0)) = self.last_fps_sample {
                let dt = now.duration_since(t0).as_secs_f32();
                let rate = (self.engine.render_count() - c0) as f32 / dt;
                log::info!("[fps] {rate:.1}");
            }
            self.last_fps_sample = Some((now, self.engine.render_count()));
            self.last_fps_print = Some(now);
        }
    }

    fn load_presets_file(&mut self) {
        let Some(path) = Self::presets_path() else {
            return;
        };
        if let Ok(text) = std::fs::read_to_string(&path) {
            let n = self.engine.import_presets(&text);
            if n > 0 {
                println!("已载入 {n} 个笔刷预设（{}）", path.display());
            }
        }
    }

    fn save_presets_file(&self) {
        let Some(path) = Self::presets_path() else {
            return;
        };
        let _ = std::fs::write(&path, self.engine.export_presets());
    }

    fn after_preset_change(&self) {
        self.save_presets_file();
    }

    /// 复制：内部剪贴板 + 尝试写入系统剪贴板（PNG）。
    fn copy_to_clipboard(&mut self) {
        if !self.engine.copy_selection() {
            println!("没有可复制的内容");
            return;
        }
        println!("已复制（内部）");
        if let Some(png) = self.engine.copy_selection_png() {
            if let Some(img) = image_from_png(&png) {
                if let Ok(mut cb) = arboard::Clipboard::new() {
                    let _ = cb.set_image(img);
                    println!("已写入系统剪贴板");
                }
            }
        }
    }

    /// 粘贴：优先系统剪贴板图像，其次内部剪贴板。
    fn paste_from_clipboard(&mut self) {
        if self.engine.transforming() {
            println!("请先提交或取消当前变换");
            return;
        }
        if let Ok(mut cb) = arboard::Clipboard::new() {
            if let Ok(img) = cb.get_image() {
                let (w, h) = (img.width as u32, img.height as u32);
                if w > 0 && h > 0 && self.engine.paste_rgba_float(&img.bytes, w, h) {
                    println!("已粘贴系统图像 {w}×{h}（拖拽定位，Enter 提交）");
                    return;
                }
            }
        }
        if self.engine.paste_float() {
            println!("已粘贴（内部，Enter 提交）");
        } else {
            println!("剪贴板没有可粘贴内容");
        }
    }

    /// 新建文档标签（当前文档入池）。
    fn doc_new(&mut self) {
        let cur = self.engine.document_take();
        self.docs.push((format!("画布 {}", self.doc_counter), cur));
        self.doc_counter += 1;
        self.engine.new_document();
        println!("新建文档（共 {} 个）", self.docs.len() + 1);
    }

    /// 切换到池中文档。
    #[allow(dead_code)]
    fn doc_switch(&mut self, idx: usize) {
        if idx >= self.docs.len() {
            return;
        }
        let cur = self.engine.document_take();
        let (name, doc) = self.docs.remove(idx);
        self.docs.push(("当前".into(), cur));
        // 换入
        let old = self.engine.swap_document(doc);
        // old 是 document_take 创建的空文档——丢弃，恢复真实当前名
        drop(old);
        // 名字放最前（栈顶为当前标签位——简化：docs 尾部即当前前的位置）
        let n = self.docs.len();
        self.docs[n - 1].0 = name.clone();
        println!("切换到 {name}");
    }

    /// 关闭当前文档（丢弃，回到池中最后一个；池空则新建）。
    fn doc_close(&mut self) {
        if let Some((name, doc)) = self.docs.pop() {
            let old = self.engine.swap_document(doc);
            drop(old);
            println!("关闭文档，切回 {name}");
        } else {
            self.engine.new_document();
            println!("关闭最后一个文档，已新建");
        }
    }

    /// 全部标签名（当前文档为最后一项）。
    fn doc_names(&self) -> Vec<String> {
        self.docs
            .iter()
            .map(|(n, _)| n.clone())
            .chain(std::iter::once("当前".to_string()))
            .collect()
    }

    fn panel_x0(&self) -> i32 {
        self.window
            .as_ref()
            .map_or(i32::MAX, |w| w.inner_size().width as i32)
    }

    fn cycle_active_layer(&mut self, dir: i32) {
        let infos = self.engine.layer_infos();
        let active = self.engine.active_layer_id().unwrap_or(u64::MAX);
        let n = infos.len() as i32;
        if n == 0 {
            return;
        }
        let cur = infos
            .iter()
            .position(|l| l.id == active)
            .map(|i| i as i32)
            .unwrap_or(0);
        let next = (cur + dir + n) % n;
        self.engine.select_layer_by_id(infos[next as usize].id);
        println!("活动层: {} ({}/{})", infos[next as usize].name, next + 1, n);
    }

    fn dragging_stroke_modifier(&self) -> bool {
        false
    }

    fn toggle_eraser(&mut self) {
        let next = if self.engine.tool() == paint_core::Tool::Eraser {
            paint_core::Tool::Brush
        } else {
            paint_core::Tool::Eraser
        };
        self.engine.set_tool(next);
    }

    /// 提交文字输入：新增/更新文字对象（非破坏可重编辑）。
    fn commit_typing(&mut self) {
        let Some((anchor, text, editing)) = self.typing.take() else {
            return;
        };
        if text.is_empty() {
            println!("空文字，忽略");
            return;
        }
        let size = self
            .text_size_hint
            .take()
            .unwrap_or_else(|| (self.engine.brush().size * 4.0).clamp(16.0, 200.0));
        let ok = if editing {
            self.engine.update_text_object(&text, size, None)
        } else {
            self.engine.add_text_object(
                (anchor.0, anchor.1 + size as f64 * 0.8),
                &text,
                size,
                None,
            )
        };
        println!(
            "{}：{text}",
            if ok {
                if editing { "文字已更新" } else { "文字已插入（可点击重编辑/拖动）" }
            } else {
                "文字操作失败（字体不支持？）"
            }
        );
        self.needs_redraw = true;
    }

    fn save_png(&mut self) {
        let path = rfd::FileDialog::new()
            .set_file_name("painting.png")
            .add_filter("PNG 图像", &["png"])
            .save_file();
        let Some(path) = path else { return };
        match self.engine.export_png(None, 1.0, false) {
            Some(png) => match std::fs::write(&path, &png) {
                Ok(()) => println!("已导出 {}", path.display()),
                Err(e) => eprintln!("导出失败: {e}"),
            },
            None => eprintln!("画布为空，未导出"),
        }
    }

    fn save_ora(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_file_name("painting.ora")
            .add_filter("OpenRaster 工程", &["ora"])
            .save_file()
        else {
            return;
        };
        match self.engine.save_ora() {
            Some(bytes) => match std::fs::write(&path, &bytes) {
                Ok(()) => println!("已保存 {}", path.display()),
                Err(e) => eprintln!("保存失败: {e}"),
            },
            None => eprintln!("画布为空，未保存"),
        }
    }

    fn open_image(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("图像文件", &["png", "jpg", "jpeg", "webp", "svg", "ora"])
            .pick_file()
        else {
            return;
        };
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        match std::fs::read(&path) {
            Ok(bytes) => {
                let ok = match ext.as_str() {
                    "svg" => self.engine.import_svg(&bytes, 1.0).is_some(),
                    "ora" => self.engine.load_ora(&bytes),
                    _ => self.engine.import_image(&bytes).is_some(),
                };
                if ok {
                    println!("已导入 {}", path.display());
                } else {
                    eprintln!("导入失败: {}", path.display());
                }
            }
            Err(e) => eprintln!("读取失败: {e}"),
        }
    }

    fn open_ora(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("OpenRaster 工程", &["ora"])
            .pick_file()
        else {
            return;
        };
        match std::fs::read(&path) {
            Ok(bytes) => {
                if self.engine.load_ora(&bytes) {
                    println!("已打开 {}", path.display());
                } else {
                    eprintln!("无法解析 {}", path.display());
                }
            }
            Err(e) => eprintln!("读取失败: {e}"),
        }
    }

    #[allow(dead_code)]
    fn legacy_save_png_auto(&mut self) {
        match self.engine.export_png(None, 1.0, false) {
            Some(png) => match std::fs::write("painting.png", &png) {
                Ok(()) => println!("已导出 painting.png ({} KB)", png.len() / 1024),
                Err(e) => eprintln!("导出失败: {e}"),
            },
            None => eprintln!("画布为空，未导出"),
        }
    }

    fn draw(&mut self) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        let size = window.inner_size();
        let (win_w, win_h) = (size.width, size.height);
        let engine_size = self.engine.frame_size();

        // 帧率监控：每秒输出一次平滑读数（须在 surface 借用前）
        self.maybe_print_fps();

        // 1) 引擎渲染画布帧（借 sb 段 1）
        {
            let Some(sb) = self.surface.as_mut() else {
                return;
            };
            let mut target = SoftbufferTarget {
                sb,
                w: engine_size.0,
                h: engine_size.1,
            };
            self.engine.render(&mut target);
        }

        // 2) 拼 UI 帧（纯 self 读写，无 sb）
        let present_frame: Vec<u8> = if win_w > engine_size.0 {
            let names = self.doc_names();
            let active = names.len() - 1;
            let mut full = vec![38u8; (win_w * win_h * 4) as usize];
            if let Some(ef) = self.engine.frame_mut() {
                for y in 0..engine_size.1.min(win_h) {
                    let src = (y * engine_size.0 * 4) as usize;
                    let dst = (y * win_w * 4) as usize;
                    let len = (engine_size.0 * 4) as usize;
                    full[dst..dst + len].copy_from_slice(&ef[src..src + len]);
                }
            }
            self.layer_panel
                .draw_tabs(&mut full, win_w, win_h, &names, active);
            self.layer_panel.draw(&self.engine, &mut full, win_w, win_h);
            full
        } else {
            Vec::new()
        };

        // 3) 呈现（借 sb 段 2）
        {
            let Some(sb) = self.surface.as_mut() else {
                return;
            };
            if !present_frame.is_empty() {
                if let Ok(mut buffer) = sb.buffer_mut() {
                    for (dst, src) in buffer.iter_mut().zip(present_frame.as_chunks::<4>().0) {
                        *dst = u32::from_le_bytes(*src);
                    }
                    let _ = buffer.present();
                }
            } else {
                // 窄窗：引擎帧已由段 1 呈现
            }
        }
        self.needs_redraw = false;
    }
}

/// softbuffer 适配器：RGBA8 帧转 u32 像素（内存布局同为 R,G,B,A 字节序，
/// 小端下直读 LE u32 即得 0xAABBGGRR）。softbuffer 每帧需要完整缓冲，
/// dirty 区域仅省去引擎侧合成。
struct SoftbufferTarget<'a> {
    sb: &'a mut softbuffer::Surface<SharedWindow, SharedWindow>,
    w: u32,
    h: u32,
}

impl Surface for SoftbufferTarget<'_> {
    fn present_cpu(&mut self, rgba: &[u8], size: (u32, u32), _dirty: Option<Rect>) {
        if size != (self.w, self.h) || rgba.len() != (self.w as usize) * (self.h as usize) * 4 {
            return;
        }
        let Ok(mut buffer) = self.sb.buffer_mut() else {
            return;
        };
        for (dst, src) in buffer.iter_mut().zip(rgba.as_chunks::<4>().0) {
            *dst = u32::from_le_bytes(*src);
        }
        let _ = buffer.present();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes().with_title(
            "paintEngine — 左键画/右键擦 · 空格/中键平移 · 滚轮缩放 · Ctrl+0 适应 · G 网格 · T 稳定器 · Y 倾斜笔 · R 旋转 · H 翻转 · Alt+点取色 · Ctrl+T 变换 · 1-9 色板 · Ctrl+S 存工程",
        );
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("创建窗口失败: {e}");
                event_loop.exit();
                return;
            }
        };
        let (w, h) = (window.inner_size().width, window.inner_size().height);
        let scale = window.scale_factor() as f32;
        let shared = SharedWindow(window.clone());
        let context = softbuffer::Context::new(shared.clone()).ok();
        let sb = context
            .as_ref()
            .and_then(|c| softbuffer::Surface::new(c, shared).ok());
        self.window = Some(window);
        self.context = context;
        self.surface = sb;
        self.on_resize(w, h, scale);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        // 所有可见变化都由事件驱动：置位重绘，空闲时不消耗 CPU
        if !matches!(event, WindowEvent::RedrawRequested) {
            self.needs_redraw = true;
        }
        match event {
            WindowEvent::CloseRequested => {
                // 退出前自动保存（编辑计数脏检查）
                self.autosave_checkpoint();
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                self.on_resize(
                    size.width,
                    size.height,
                    self.window.as_ref().map_or(1.0, |w| w.scale_factor()) as f32,
                );
            }
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x, position.y);
                if let Drag::Pan { last } = self.drag {
                    let (dx, dy) = (self.cursor.0 - last.0, self.cursor.1 - last.1);
                    self.engine.document_mut().viewport_mut().pan_by(dx, dy);
                    self.drag = Drag::Pan { last: self.cursor };
                } else if let Drag::Select { start, op } = self.drag {
                    let (x0, y0) = start;
                    let (x1, y1) = self.cursor;
                    let rect = paint_core::Rect::new(
                        x0.min(x1).floor() as i32,
                        y0.min(y1).floor() as i32,
                        (x1 - x0).abs().ceil().max(1.0) as u32,
                        (y1 - y0).abs().ceil().max(1.0) as u32,
                    );
                    // 屏幕矩形 → 画布矩形
                    let vp = self.engine.document().viewport();
                    let (cx0, cy0) = vp.screen_to_canvas(rect.x as f64, rect.y as f64);
                    let (cx1, cy1) = vp.screen_to_canvas(
                        (rect.x + rect.w as i32) as f64,
                        (rect.y + rect.h as i32) as f64,
                    );
                    let canvas_rect = paint_core::Rect::new(
                        cx0.floor() as i32,
                        cy0.floor() as i32,
                        (cx1.ceil() - cx0.floor()) as u32,
                        (cy1.ceil() - cy0.floor()) as u32,
                    );
                    self.engine.select_rect(canvas_rect, op);
                } else if let Drag::Float { last } = self.drag {
                    // 屏幕位移 → 画布位移（含视口旋转/缩放）
                    let vp = self.engine.document().viewport().clone();
                    let (dx, dy) = (self.cursor.0 - last.0, self.cursor.1 - last.1);
                    let inv_zoom = 1.0 / vp.zoom();
                    let (c, sn) = (vp.rotation().cos(), vp.rotation().sin());
                    let rx = c * dx + sn * dy;
                    let ry = -sn * dx + c * dy;
                    let fx = if vp.flip_x() { -rx } else { rx };
                    self.engine
                        .transform_translate(fx * inv_zoom, ry * inv_zoom);
                    self.drag = Drag::Float { last: self.cursor };
                } else if self.drag == Drag::Stroke {
                    let sample = self.pointer_sample();
                    self.engine.handle_event(PlatformEvent::Pointer {
                        phase: PointerPhase::Move,
                        sample,
                    });
                }
            }
            WindowEvent::MouseInput { state, button, .. } => match (state, button) {
                (ElementState::Pressed, MouseButton::Left) => {
                    if self.modifiers.control_key()
                        && !self.modifiers.shift_key()
                        && !self.modifiers.alt_key()
                        && !self.space_down
                        && self.dragging_stroke_modifier()
                    {
                        // 留给笔画（Ctrl+Z 等组合不受影响）——Ctrl 纯按住拖拽才选区
                    }
                    if self.engine.transforming() {
                        self.drag = Drag::Float { last: self.cursor };
                        return;
                    }
                    // Alt+点击 = 吸管取色
                    if self.modifiers.alt_key() && !self.space_down {
                        let (x, y) = (self.cursor.0 as u32, self.cursor.1 as u32);
                        if let Some(c) = self.engine.pick_color(x, y) {
                            self.engine.brush_mut().color = c;
                            log::info!("[color] 取色 #{:02X}{:02X}{:02X}", c.r, c.g, c.b);
                        }
                        return;
                    }
                    let ctrl_sel = self.modifiers.control_key() && !self.space_down;
                    if ctrl_sel {
                        let op = if self.modifiers.alt_key() {
                            paint_core::SelectionOp::Subtract
                        } else if self.modifiers.shift_key() {
                            paint_core::SelectionOp::Add
                        } else {
                            paint_core::SelectionOp::Replace
                        };
                        self.drag = Drag::Select {
                            start: self.cursor,
                            op,
                        };
                    } else if self.space_down {
                        self.drag = Drag::Pan { last: self.cursor };
                    } else {
                        self.drag = Drag::Stroke;
                        let sample = self.pointer_sample();
                        self.engine.handle_event(PlatformEvent::Pointer {
                            phase: PointerPhase::Down,
                            sample,
                        });
                    }
                }
                (ElementState::Released, MouseButton::Left) => {
                    if let Drag::Select { start, .. } = self.drag {
                        let empty = (self.cursor.0 - start.0).abs() < 2.0
                            && (self.cursor.1 - start.1).abs() < 2.0;
                        if empty {
                            self.engine.clear_selection();
                        }
                        self.drag = Drag::None;
                    } else if let Drag::Float { last } = self.drag {
                        // 屏幕位移 → 画布位移（含视口旋转/缩放）
                        let vp = self.engine.document().viewport().clone();
                        let (dx, dy) = (self.cursor.0 - last.0, self.cursor.1 - last.1);
                        let inv_zoom = 1.0 / vp.zoom();
                        let (c, sn) = (vp.rotation().cos(), vp.rotation().sin());
                        let rx = c * dx + sn * dy;
                        let ry = -sn * dx + c * dy;
                        let fx = if vp.flip_x() { -rx } else { rx };
                        self.engine
                            .transform_translate(fx * inv_zoom, ry * inv_zoom);
                        self.drag = Drag::Float { last: self.cursor };
                    } else if self.drag == Drag::Stroke {
                        let sample = self.pointer_sample();
                        self.engine.handle_event(PlatformEvent::Pointer {
                            phase: PointerPhase::Up,
                            sample,
                        });
                        // 文字工具：抬手先查命中已有对象（编辑），再查新锚点
                        if let Some((text, size, _)) = self.engine.take_text_edit() {
                            self.typing = Some(((0.0, 0.0), text, true));
                            self.text_size_hint = Some(size);
                            println!("编辑文字（回车更新 · Del 删除 · Esc 取消）");
                        } else if let Some(a) = self.engine.take_text_anchor() {
                            self.typing = Some((a, String::new(), false));
                            println!("文字输入（Enter 插入 · Esc 取消）：");
                        }
                    }
                    self.drag = Drag::None;
                }
                // 右键拖动 = 临时橡皮（松开恢复原工具）
                (ElementState::Pressed, MouseButton::Right) => {
                    if self.drag == Drag::None {
                        self.tool_before_erase = Some(self.engine.tool());
                        self.engine.set_tool(paint_core::Tool::Eraser);
                        self.drag = Drag::Stroke;
                        let sample = self.pointer_sample();
                        self.engine.handle_event(PlatformEvent::Pointer {
                            phase: PointerPhase::Down,
                            sample,
                        });
                    }
                }
                (ElementState::Released, MouseButton::Right) => {
                    if self.drag == Drag::Stroke {
                        let sample = self.pointer_sample();
                        self.engine.handle_event(PlatformEvent::Pointer {
                            phase: PointerPhase::Up,
                            sample,
                        });
                    }
                    if let Some(t) = self.tool_before_erase.take() {
                        self.engine.set_tool(t);
                    }
                    self.drag = Drag::None;
                }
                (ElementState::Pressed, MouseButton::Middle) => {
                    self.drag = Drag::Pan { last: self.cursor };
                }
                (ElementState::Released, MouseButton::Middle) => {
                    if matches!(self.drag, Drag::Pan { .. }) {
                        self.drag = Drag::None;
                    }
                }
                _ => {}
            },
            WindowEvent::MouseWheel { delta, .. } => {
                let step = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y as f64,
                    MouseScrollDelta::PixelDelta(p) => p.y / 53.0,
                };
                if self.engine.transforming() {
                    if self.modifiers.shift_key() {
                        self.engine.transform_scale(1.1f64.powf(step));
                    } else {
                        self.engine.transform_rotate(5.0f64.to_radians() * step);
                    }
                } else if self.cursor.0 as i32 >= self.panel_x0() {
                    // 面板区域滚轮 = 活动层透明度
                    let infos = self.engine.layer_infos();
                    let active = self.engine.active_layer_id().unwrap_or(u64::MAX);
                    if let Some(i) = infos.iter().position(|l| l.id == active) {
                        let v = (infos[i].opacity + 0.05 * step as f32).clamp(0.0, 1.0);
                        self.engine.set_layer_opacity_by_index(i, v);
                        println!("{} 透明度: {:.0}%", infos[i].name, v * 100.0);
                    }
                } else {
                    let factor = 1.15f64.powf(step);
                    self.engine
                        .document_mut()
                        .viewport_mut()
                        .zoom_at(self.cursor, factor);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let KeyEvent {
                    logical_key, state, ..
                } = &event;
                match (logical_key, state) {
                    // ── 文字输入态优先拦截 ──
                    (Key::Named(NamedKey::Enter), ElementState::Pressed)
                        if self.typing.is_some() =>
                    {
                        self.commit_typing();
                    }
                    (Key::Named(NamedKey::Escape), ElementState::Pressed)
                        if self.typing.is_some() =>
                    {
                        self.typing = None;
                        println!("文字输入已取消");
                    }
                    (Key::Named(NamedKey::Backspace), ElementState::Pressed)
                        if self.typing.is_some() =>
                    {
                        if let Some((_, buf, _)) = self.typing.as_mut() {
                            buf.pop();
                            println!("输入：{buf}_");
                        }
                    }
                    (Key::Named(NamedKey::Delete), ElementState::Pressed)
                        if self.typing.as_ref().is_some_and(|(_, _, ed)| *ed) =>
                    {
                        self.typing = None;
                        if self.engine.delete_text_object() {
                            println!("文字对象已删除");
                        }
                    }
                    (Key::Named(NamedKey::Delete), ElementState::Pressed)
                        if self.engine.has_selection() && !self.engine.transforming() =>
                    {
                        self.engine.delete_selection();
                    }
                    (Key::Named(NamedKey::Backspace), ElementState::Pressed)
                        if self.engine.has_selection() && !self.engine.transforming() =>
                    {
                        self.engine.delete_selection();
                    }
                    (Key::Named(NamedKey::PageUp), ElementState::Pressed) => {
                        self.cycle_active_layer(1);
                    }
                    (Key::Named(NamedKey::PageDown), ElementState::Pressed) => {
                        self.cycle_active_layer(-1);
                    }
                    (Key::Named(NamedKey::Enter), ElementState::Pressed)
                        if self.engine.transforming() =>
                    {
                        self.engine.commit_transform();
                        log::info!("[transform] 已提交");
                    }
                    (Key::Named(NamedKey::Escape), ElementState::Pressed)
                        if self.engine.transforming() =>
                    {
                        self.engine.cancel_transform();
                        log::info!("[transform] 已取消");
                    }
                    (Key::Named(NamedKey::Space), ElementState::Pressed) => self.space_down = true,
                    (Key::Named(NamedKey::Space), ElementState::Released) => {
                        self.space_down = false
                    }
                    (Key::Character(c), ElementState::Pressed) => {
                        let c = c.as_str();
                        let ctrl = self.modifiers.control_key();
                        let shift = self.modifiers.shift_key();
                        // 文字输入态：吞掉所有可打印字符（不触发快捷键）
                        if self.typing.is_some() && !ctrl && !self.modifiers.alt_key() {
                            if let Some((_, buf, _)) = self.typing.as_mut() {
                                // logical_key 为完整字符（含中文输入法提交）
                                if c.chars().all(|ch| !ch.is_control()) {
                                    buf.push_str(c);
                                    println!("输入：{buf}_");
                                }
                            }
                            self.needs_redraw = true;
                            return;
                        }
                        match c {
                            "z" | "Z" if ctrl => {
                                if shift {
                                    self.engine.redo();
                                } else {
                                    self.engine.undo();
                                }
                            }
                            "y" | "Y" if ctrl => {
                                self.engine.redo();
                            }
                            "n" | "N" if ctrl && shift => {
                                self.engine.add_layer();
                            }
                            "e" | "E" if ctrl => {
                                self.engine.merge_down();
                            }
                            "f" | "F" if ctrl => {
                                self.engine.flatten();
                            }
                            "s" | "S" if ctrl && shift => {
                                self.save_png();
                            }
                            "s" | "S" if ctrl => {
                                self.save_ora();
                            }
                            "y" | "Y" => {
                                // tilt 笔刷灵敏度档位：0 → 50% → 100% → 0
                                let cur = self.engine.brush().tilt_sensitivity;
                                let next = if cur < 0.1 {
                                    0.5
                                } else if cur < 0.9 {
                                    1.0
                                } else {
                                    0.0
                                };
                                self.engine.brush_mut().tilt_sensitivity = next;
                                println!("笔倾斜灵敏度: {:.0}%", next * 100.0);
                            }
                            "1" if !ctrl => self.set_palette(0),
                            "2" => self.set_palette(1),
                            "3" => self.set_palette(2),
                            "4" => self.set_palette(3),
                            "5" => self.set_palette(4),
                            "6" => self.set_palette(5),
                            "7" => self.set_palette(6),
                            "8" => self.set_palette(7),
                            "9" => self.set_palette(8),
                            "," => {
                                if let Some(n) = self.engine.cycle_preset(false) {
                                    self.after_preset_change();
                                    println!("预设 ← {n}");
                                }
                            }
                            "." => {
                                if let Some(n) = self.engine.cycle_preset(true) {
                                    self.after_preset_change();
                                    println!("预设 → {n}");
                                }
                            }
                            "p" | "P" if ctrl => {
                                if let Some(n) = self.engine.current_preset_name() {
                                    if self.engine.delete_preset(&n) {
                                        self.save_presets_file();
                                        println!("已删除预设 {n}");
                                    }
                                } else {
                                    println!("没有活动预设可删除");
                                }
                            }
                            "p" | "P" => {
                                let name = self
                                    .engine
                                    .current_preset_name()
                                    .unwrap_or_else(|| "自定义笔".into());
                                if self.engine.save_preset(&name) {
                                    self.save_presets_file();
                                    println!("已保存预设 {name}");
                                }
                            }
                            "c" | "C" if ctrl => {
                                self.copy_to_clipboard();
                            }
                            "x" | "X" if ctrl => {
                                if self.engine.cut_selection() {
                                    self.copy_to_clipboard();
                                    println!("已剪切");
                                }
                            }
                            "v" | "V" if ctrl => {
                                self.paste_from_clipboard();
                            }
                            "n" | "N" if ctrl && !shift => {
                                self.doc_new();
                            }
                            "w" | "W" if ctrl => {
                                self.doc_close();
                            }
                            "t" | "T" if ctrl => {
                                if self.engine.transforming() {
                                    self.engine.commit_transform();
                                    log::info!("[transform] 已提交");
                                } else if self.engine.begin_transform() {
                                    println!(
                                        "内容变换：拖拽移动 · 滚轮旋转 · Shift+滚轮缩放 · Enter 提交 · Esc 取消"
                                    );
                                } else {
                                    println!("没有可变换的内容");
                                }
                            }
                            "d" | "D" if ctrl => {
                                self.engine.clear_selection();
                            }
                            "a" | "A" if ctrl => {
                                self.engine.select_all();
                            }
                            "i" | "I" if ctrl => {
                                self.open_image();
                            }
                            "o" | "O" if ctrl => {
                                self.open_ora();
                            }
                            "0" if ctrl => {
                                self.engine.fit_to_content(48.0);
                            }
                            "1" if ctrl => {
                                self.engine.zoom_100();
                            }
                            "r" | "R" if ctrl => {
                                self.engine.document_mut().viewport_mut().reset_transform();
                            }
                            "r" | "R" => {
                                let delta: f64 = if self.modifiers.shift_key() {
                                    -15.0
                                } else {
                                    15.0
                                };
                                let anchor = self.cursor;
                                self.engine
                                    .document_mut()
                                    .viewport_mut()
                                    .rotate_by(anchor, delta.to_radians());
                            }
                            "m" | "M" if shift => {
                                let on = self.engine.toggle_layer_mask();
                                println!("图层蒙版: {}", if on { "开" } else { "关" });
                            }
                            "m" | "M" => {
                                let mask_tool = self.engine.tool() == paint_core::Tool::Mask;
                                self.engine.set_tool(if mask_tool {
                                    paint_core::Tool::Brush
                                } else {
                                    paint_core::Tool::Mask
                                });
                            }
                            "l" | "L" => {
                                let on = self.engine.toggle_layer_clip();
                                println!("剪贴层: {}", if on { "开" } else { "关" });
                            }
                            "h" | "H" => {
                                let anchor = self.cursor;
                                self.engine.document_mut().viewport_mut().flip_x_at(anchor);
                            }
                            "t" | "T" => {
                                // 稳定器档位循环：0 → 50% → 85% → 0
                                let cur = self.engine.brush().stabilizer;
                                let next = if cur < 0.1 {
                                    0.5
                                } else if cur < 0.7 {
                                    0.85
                                } else {
                                    0.0
                                };
                                self.engine.brush_mut().stabilizer = next;
                                println!("稳定器: {next:.0}");
                            }
                            "c" | "C" if ctrl && shift => {
                                if self.engine.canvas_bounds().is_some() {
                                    self.engine.clear_canvas();
                                    println!("画布: 无限");
                                } else {
                                    self.engine.set_canvas(1920, 1080);
                                    println!("画布: 1920×1080");
                                }
                            }
                            "x" | "X" => {
                                let name = self.engine.cycle_symmetry();
                                println!("对称: {name}");
                            }
                            "v" | "V" => {
                                let infos = self.engine.layer_infos();
                                let active = self.engine.active_layer_id().unwrap_or(u64::MAX);
                                if let Some(i) = infos.iter().position(|l| l.id == active) {
                                    self.engine.set_layer_visible_by_index(i, !infos[i].visible);
                                    println!(
                                        "{} 可见性: {}",
                                        infos[i].name,
                                        if infos[i].visible { "隐藏" } else { "显示" }
                                    );
                                }
                            }
                            "j" | "J" => {
                                // 插件滤镜示例：通道偏移
                                let mut params = paint_core::plugin::PluginParams::default();
                                params.set("r", paint_core::plugin::ParamValue::Number(30.0));
                                params.set("b", paint_core::plugin::ParamValue::Number(-20.0));
                                if self.engine.apply_plugin_filter("channel_shift", &params) {
                                    println!("已应用插件滤镜: 通道偏移");
                                } else {
                                    println!("插件不可用");
                                }
                            }
                            "f" | "F" if !ctrl => {
                                // 简易滤镜：反色（最直观）
                                if self.engine.apply_filter(paint_core::filter::Filter::Invert) {
                                    println!("已应用滤镜: 反色");
                                }
                            }
                            "g" | "G" => {
                                let on = !self.engine.show_grid();
                                self.engine.set_show_grid(on);
                            }
                            "e" | "E" if !ctrl => {
                                // B/E 在画笔/橡皮间切换
                                self.toggle_eraser();
                            }
                            "b" | "B" => {
                                self.engine.set_tool(paint_core::Tool::Brush);
                            }
                            "s" | "S" if !ctrl => {
                                // 形状工具：S 循环 线→矩→椭圆；Shift+S 切描边/填充
                                use paint_core::ShapeKind;
                                let cur = self.engine.tool();
                                let (kind, fill) = match cur {
                                    paint_core::Tool::Shape { kind, fill } => (kind, fill),
                                    _ => (ShapeKind::Line, shift),
                                };
                                if shift {
                                    let new_fill = !fill;
                                    self.engine.set_tool(paint_core::Tool::Shape {
                                        kind,
                                        fill: new_fill,
                                    });
                                    println!("形状：{}", if new_fill { "填充" } else { "描边" });
                                } else {
                                    let next = match kind {
                                        ShapeKind::Line => ShapeKind::Rect,
                                        ShapeKind::Rect => ShapeKind::Ellipse,
                                        ShapeKind::Ellipse => ShapeKind::Line,
                                    };
                                    self.engine.set_tool(paint_core::Tool::Shape {
                                        kind: next,
                                        fill,
                                    });
                                    let n = match next {
                                        ShapeKind::Line => "直线",
                                        ShapeKind::Rect => "矩形",
                                        ShapeKind::Ellipse => "椭圆",
                                    };
                                    let m = if fill { "填充" } else { "描边" };
                                    println!("形状工具：{n}（{m}）——拖拽绘制");
                                }
                            }
                            "i" | "I" if !ctrl => {
                                self.engine.set_tool(paint_core::Tool::Text);
                                println!("文字工具：点击画布位置后直接输入，Enter 插入 / Esc 取消");
                            }
                            "a" | "A" if !ctrl => {
                                self.engine.set_tool(paint_core::Tool::Fill { tolerance: 32 });
                                println!("填充工具：点击色块区域填充当前笔刷色");
                            }
                            "0" => {
                                let on = !self.engine.fps_monitor_enabled();
                                self.engine.set_fps_monitor(on);
                                println!("帧率监控：{}", if on { "开" } else { "关" });
                            }
                            "[" => {
                                let b = self.engine.brush_mut();
                                b.size = (b.size / 1.25).max(1.0);
                            }
                            "]" => {
                                let b = self.engine.brush_mut();
                                b.size = (b.size * 1.25).min(512.0);
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            WindowEvent::Focused(f) => {
                self.engine.handle_event(PlatformEvent::Focus(f));
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // 检查点式自动保存（每 30 次编辑）
        if self.engine.edit_count().saturating_sub(self.saved_edit_count) >= 30 {
            self.autosave_checkpoint();
        }
        // 按需重绘：仅事件驱动，空闲时零合成零呈现
        if self.needs_redraw {
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }
    }
}
