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

fn main() {
    let event_loop = winit::event_loop::EventLoop::new().unwrap();
    let mut app = App::new();
    event_loop.run_app(&mut app).unwrap();
}

#[derive(PartialEq)]
enum Drag {
    None,
    Stroke,
    Pan { last: (f64, f64) },
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
    /// 按需重绘：任何输入/焦点/尺寸事件置位，画完即清。
    needs_redraw: bool,
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
            needs_redraw: true,
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
        self.engine
            .handle_event(PlatformEvent::Resize { w, h, scale });
    }

    fn toggle_eraser(&mut self) {
        let next = if self.engine.tool() == paint_core::Tool::Eraser {
            paint_core::Tool::Brush
        } else {
            paint_core::Tool::Eraser
        };
        self.engine.set_tool(next);
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
        let Some(sb) = self.surface.as_mut() else {
            return;
        };
        let size = window.inner_size();
        let mut target = SoftbufferTarget {
            sb,
            w: size.width,
            h: size.height,
        };
        self.engine.render(&mut target);
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
            "paintEngine — 左键画/右键擦 · 空格/中键平移 · 滚轮缩放 · Ctrl+0 适应 · G 网格 · T 稳定器 · Ctrl+S 存工程 · Ctrl+Z 撤销",
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
            WindowEvent::CloseRequested => event_loop.exit(),
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
                    if self.space_down {
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
                    if self.drag == Drag::Stroke {
                        let sample = self.pointer_sample();
                        self.engine.handle_event(PlatformEvent::Pointer {
                            phase: PointerPhase::Up,
                            sample,
                        });
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
                let factor = match delta {
                    MouseScrollDelta::LineDelta(_, y) => 1.15f64.powf(y as f64),
                    MouseScrollDelta::PixelDelta(p) => (p.y / 400.0).exp(),
                };
                self.engine
                    .document_mut()
                    .viewport_mut()
                    .zoom_at(self.cursor, factor);
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let KeyEvent {
                    logical_key, state, ..
                } = &event;
                match (logical_key, state) {
                    (Key::Named(NamedKey::Space), ElementState::Pressed) => self.space_down = true,
                    (Key::Named(NamedKey::Space), ElementState::Released) => {
                        self.space_down = false
                    }
                    (Key::Character(c), ElementState::Pressed) => {
                        let c = c.as_str();
                        let ctrl = self.modifiers.control_key();
                        let shift = self.modifiers.shift_key();
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
                            "o" | "O" if ctrl => {
                                self.open_ora();
                            }
                            "0" if ctrl => {
                                self.engine.fit_to_content(48.0);
                            }
                            "1" if ctrl => {
                                self.engine.zoom_100();
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
        // 按需重绘：仅事件驱动，空闲时零合成零呈现
        if self.needs_redraw {
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }
    }
}
