//! 桌面图层面板：在引擎帧缓冲上直接绘制（呈现前拦截）。
//!
//! 布局：右侧 PANEL_W 像素列。自顶向下：标题栏 → 图层行
//! （自顶向下=顶层优先）→ 底部提示。点击/滚轮由 main.rs 的
//! 事件拦截驱动（panel_x0 判定），这里只负责绘制与命中测试。

use paint_core::Engine;

pub const PANEL_W: u32 = 200;
pub const TITLE_H: u32 = 32;
pub const ROW_H: u32 = 30;
pub const HINT_H: u32 = 44;
pub const TAB_H: u32 = 28;
pub const TAB_W: u32 = 110;

/// 面板命中区域。
#[allow(dead_code)]
pub enum Hit {
    Title,
    /// 图层行（栈索引）
    Row(usize),
    /// 眼睛切换按钮区域（栈索引）
    Eye(usize),
    Hint,
    Outside,
}

pub struct Panel {
    /// 系统字体（Helvetica.ttc，懒加载）
    font: Option<Vec<u8>>,
    /// 小号字号
    font_small: f32,
    font_title: f32,
}

impl Panel {
    pub fn new() -> Self {
        Self {
            font: std::fs::read("/System/Library/Fonts/Helvetica.ttc").ok(),
            font_small: 11.0,
            font_title: 13.0,
        }
    }

    pub fn panel_x0(frame_w: u32) -> u32 {
        frame_w.saturating_sub(PANEL_W)
    }

    /// 命中测试：屏幕坐标 → 面板区域。
    #[allow(dead_code)]
    pub fn hit_test(&self, x: i32, y: i32, layer_count: usize, frame_w: u32) -> Hit {
        let px0 = Self::panel_x0(frame_w) as i32;
        if x < px0 {
            return Hit::Outside;
        }
        if y < TITLE_H as i32 {
            return Hit::Title;
        }
        // 图层行区域（行 0 = 顶层，显示倒序）
        let row_area_h = layer_count as i32 * ROW_H as i32;
        if y < TITLE_H as i32 + row_area_h {
            let row_in_list = ((y - TITLE_H as i32) / ROW_H as i32) as usize;
            // 眼睛按钮：行左端 20px
            let rel_x = (x - px0) as usize;
            if rel_x < 24 {
                return Hit::Eye(layer_count - 1 - row_in_list);
            } else {
                return Hit::Row(layer_count - 1 - row_in_list);
            }
        }
        // 底部提示区
        if y >= TITLE_H as i32 + row_area_h {
            return Hit::Hint;
        }
        Hit::Outside
    }

    /// 绘制标签栏（画布区域顶部）。返回标签栏高度（0 = 不画）。
    pub fn draw_tabs(
        &self,
        frame: &mut [u8],
        frame_w: u32,
        frame_h: u32,
        names: &[String],
        active: usize,
    ) {
        if frame_w < 200 || frame_h < TAB_H + 50 || names.is_empty() {
            return;
        }
        // 背景
        fill_rect(frame, frame_w, 0, 0, frame_w, TAB_H, [34, 34, 38, 255]);
        // 标签
        for (i, name) in names.iter().enumerate() {
            let x = i as u32 * TAB_W;
            if x + TAB_W > frame_w {
                break;
            }
            let is_active = i == active;
            let bg = if is_active {
                [55, 90, 160, 255]
            } else {
                [42, 42, 46, 255]
            };
            fill_rect(frame, frame_w, x + 2, 3, TAB_W - 4, TAB_H - 4, bg);
            let col = if is_active {
                [255, 255, 255, 255]
            } else {
                [160, 160, 168, 255]
            };
            // 截断长名
            let display: String = name.chars().take(8).collect();
            self.draw_text(frame, frame_w, x + 10, 7, &display, col, 11.0);
            // 关闭 ×
            if names.len() > 1 {
                self.draw_text(
                    frame,
                    frame_w,
                    x + TAB_W - 18,
                    7,
                    "×",
                    [200, 200, 200, 255],
                    11.0,
                );
            }
        }
        // + 新建（最右）
        let plus_x = (names.len() as u32) * TAB_W;
        if plus_x + 30 < frame_w {
            self.draw_text(
                frame,
                frame_w,
                plus_x + 8,
                7,
                "+",
                [160, 160, 168, 255],
                12.0,
            );
        }
        // 分隔线
        fill_rect(frame, frame_w, 0, TAB_H - 1, frame_w, 1, [60, 60, 66, 255]);
    }

    /// 绘制面板到帧缓冲（RGBA8 预乘行主序，帧宽 = frame_w）。
    pub fn draw(&self, engine: &Engine, frame: &mut [u8], frame_w: u32, frame_h: u32) {
        if frame_w < PANEL_W + 100 || frame_h < 200 {
            return;
        }
        let px0 = Self::panel_x0(frame_w);
        let infos = engine.layer_infos();
        let active = engine.active_layer_id().unwrap_or(u64::MAX);

        // ── 背景 ──
        fill_rect(frame, frame_w, px0, 0, PANEL_W, frame_h, [38, 38, 42, 255]);

        // ── 标题栏 ──
        fill_rect(frame, frame_w, px0, 0, PANEL_W, TITLE_H, [30, 30, 34, 255]);
        self.draw_text(
            frame,
            frame_w,
            px0 + 10,
            9,
            "图层",
            [220, 220, 225, 255],
            self.font_title,
        );
        let count_text = format!("({})", infos.len());
        self.draw_text(
            frame,
            frame_w,
            px0 + 48,
            11,
            &count_text,
            [140, 140, 148, 255],
            self.font_small,
        );

        // ── 图层行（自顶向下=倒序遍历）──
        for (row, li) in infos.iter().enumerate().rev() {
            let y_top = TITLE_H + (infos.len() - 1 - row) as u32 * ROW_H;
            let is_active = li.id == active;

            // 行背景（活动层高亮）
            let bg = if is_active {
                [45, 90, 165, 255]
            } else {
                [42, 42, 46, 255]
            };
            fill_rect(frame, frame_w, px0, y_top, PANEL_W, ROW_H, bg);

            // 眼睛图标（简化：可见=●，隐藏=○）
            let eye_char = if li.visible { "●" } else { "○" };
            let eye_col = if li.visible {
                [120, 200, 120, 255]
            } else {
                [100, 100, 108, 255]
            };
            self.draw_cjk_glyph(frame, frame_w, px0 + 6, y_top + 7, eye_char, eye_col, 14.0);

            // 层名（含标记）
            let mut name = li.name.clone();
            if li.has_mask {
                name.push_str(" ◐");
            }
            if li.clipped {
                name.push_str(" ⧉");
            }
            let name_col = if li.visible {
                [230, 230, 235, 255]
            } else {
                [120, 120, 128, 255]
            };
            self.draw_text_cjk(
                frame,
                frame_w,
                px0 + 28,
                y_top + 8,
                &name,
                name_col,
                self.font_small,
            );

            // 不透明度条
            let bar_w = 60u32;
            let bar_x = px0 + PANEL_W - bar_w - 10;
            let bar_y = y_top + ROW_H - 7;
            fill_rect(frame, frame_w, bar_x, bar_y, bar_w, 3, [60, 60, 66, 255]);
            let fill_w = (bar_w as f32 * li.opacity) as u32;
            fill_rect(frame, frame_w, bar_x, bar_y, fill_w, 3, [90, 170, 250, 255]);
        }

        // ── 底部提示 ──
        let hint_y = frame_h.saturating_sub(HINT_H);
        fill_rect(
            frame,
            frame_w,
            px0,
            hint_y,
            PANEL_W,
            HINT_H,
            [30, 30, 34, 255],
        );
        let hints = [
            "PageUp/Dn 选层 · V 可见性",
            "滚轮 透明度 · +新建 · Del 删",
            "Ctrl+E 合并 · Ctrl+F 压平",
        ];
        for (i, h) in hints.iter().enumerate() {
            self.draw_text_cjk(
                frame,
                frame_w,
                px0 + 8,
                hint_y + 6 + i as u32 * 13,
                h,
                [130, 130, 138, 255],
                10.0,
            );
        }
    }

    /// 画英文/数字文字（swash 光栅化）。
    #[allow(clippy::too_many_arguments)]
    fn draw_text(
        &self,
        frame: &mut [u8],
        w: u32,
        x: u32,
        y: u32,
        text: &str,
        color: [u8; 4],
        size: f32,
    ) {
        self.rasterize(frame, w, x, y, text, color, size, false)
    }

    /// 画含中文的文字。
    #[allow(clippy::too_many_arguments)]
    fn draw_text_cjk(
        &self,
        frame: &mut [u8],
        w: u32,
        x: u32,
        y: u32,
        text: &str,
        color: [u8; 4],
        size: f32,
    ) {
        self.rasterize(frame, w, x, y, text, color, size, true)
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_cjk_glyph(
        &self,
        frame: &mut [u8],
        w: u32,
        x: u32,
        y: u32,
        ch: &str,
        color: [u8; 4],
        size: f32,
    ) {
        self.rasterize(frame, w, x, y, ch, color, size, true)
    }

    /// swash 光栅化写帧（直接 alpha 混合）。
    #[allow(clippy::too_many_arguments)]
    fn rasterize(
        &self,
        frame: &mut [u8],
        fw: u32,
        x: u32,
        y: u32,
        text: &str,
        color: [u8; 4],
        size: f32,
        _cjk: bool,
    ) {
        use swash::scale::{Render, ScaleContext, Source};
        use swash::shape::ShapeContext;
        use swash::FontRef;

        let Some(font_bytes) = &self.font else { return };
        // Helvetica.ttc 是集合——取第 0 个
        let Some(font) = FontRef::from_index(font_bytes, 0) else {
            return;
        };

        let mut shaper_ctx = ShapeContext::new();
        let mut scale_ctx = ScaleContext::new();

        let mut run = shaper_ctx.builder(font).size(size).build();
        run.add_str(text);
        let mut pen_x = 0f32;
        let mut glyphs = Vec::new();
        run.shape_with(|cluster| {
            for g in cluster.glyphs {
                glyphs.push((g.id, pen_x + g.x, g.y));
                pen_x += g.advance;
            }
        });

        let sources = [
            Source::Outline,
            Source::Bitmap(swash::scale::StrikeWith::BestFit),
        ];
        let render = Render::new(&sources);
        let mut scaler = scale_ctx.builder(font).size(size).build();

        for (gid, gx, gy) in glyphs {
            let Some(image) = render.render(&mut scaler, gid) else {
                continue;
            };
            let ox = x as i64 + gx.round() as i64 + image.placement.left as i64;
            let oy = y as i64 + gy.round() as i64 - image.placement.top as i64;
            let (iw, ih) = (image.placement.width as i64, image.placement.height as i64);
            for row in 0..ih {
                for col in 0..iw {
                    let a = image.data[(row * iw + col) as usize];
                    if a == 0 {
                        continue;
                    }
                    let (px, py) = (ox + col, oy + row);
                    if px < 0 || py < 0 || px >= fw as i64 {
                        continue;
                    }
                    let idx = (py as usize * fw as usize + px as usize) * 4;
                    if idx + 3 >= frame.len() {
                        continue;
                    }
                    let sa = a as f32 / 255.0;
                    let dr = frame[idx] as f32;
                    let dg = frame[idx + 1] as f32;
                    let db = frame[idx + 2] as f32;
                    frame[idx] = (color[0] as f32 * sa + dr * (1.0 - sa)) as u8;
                    frame[idx + 1] = (color[1] as f32 * sa + dg * (1.0 - sa)) as u8;
                    frame[idx + 2] = (color[2] as f32 * sa + db * (1.0 - sa)) as u8;
                    frame[idx + 3] = 255;
                }
            }
        }
    }
}

fn fill_rect(frame: &mut [u8], fw: u32, x: u32, y: u32, w: u32, h: u32, color: [u8; 4]) {
    let fh = (frame.len() / (fw as usize * 4)) as u32;
    for row in y..(y + h).min(fh) {
        for col in x..(x + w).min(fw) {
            let idx = (row as usize * fw as usize + col as usize) * 4;
            if idx + 3 < frame.len() {
                frame[idx..idx + 4].copy_from_slice(&color);
            }
        }
    }
}
