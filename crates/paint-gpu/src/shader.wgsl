// M2.5 GPU 合成着色器。与 paint-render/src/composite.rs 的 CPU 路径逐像素对齐：
// - 背景网格：解析式点阵（最近网格交点 ±0.5px 命中）
// - 瓦片合成：composite_pixel 的 WGSL 移植（12 种混合模式，预乘域）
// 全屏三角形顶点，一切几何在片元着色器内按 uniform 计算。

struct Vp {
    pan_x: f32,
    pan_y: f32,
    zoom: f32,
    inv_zoom: f32,
    screen_w: f32,
    screen_h: f32,
    grid_spacing: f32,
    grid_on: f32,
    bg: vec4<f32>,      // rgb + alpha（透明导出为 0）
    dot_rgb: vec4<f32>, // 网格点颜色
    rot_c: f32,
    rot_s: f32,
    flip: f32,
    _pad: f32,
};

struct Vout {
    @builtin(position) pos: vec4<f32>,
};

@vertex
fn vs_fullscreen(@builtin(vertex_index) vi: u32) -> Vout {
    var positions = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    var out: Vout;
    out.pos = vec4<f32>(positions[vi], 0.0, 1.0);
    return out;
}

// 屏幕像素坐标（左上原点）→ 画布坐标（含旋转/翻转的通用逆变换）
fn canvas_coord(px: f32, py_top: f32, vp: Vp) -> vec2<f32> {
    let d = vec2<f32>(px + 0.5, py_top + 0.5) - vec2<f32>(vp.pan_x, vp.pan_y);
    let rx = vp.rot_c * d.x + vp.rot_s * d.y;
    let ry = -vp.rot_s * d.x + vp.rot_c * d.y;
    let fx = select(rx, -rx, vp.flip > 0.5);
    return vec2<f32>(fx, ry) * vp.inv_zoom;
}

// ── 背景网格通道 ──
@group(0) @binding(0) var<uniform> BG_VP: Vp;

@fragment
fn fs_background(in: Vout) -> @location(0) vec4<f32> {
    let px = floor(in.pos.x);
    let py_top = floor(in.pos.y);
    var col = vec4<f32>(BG_VP.bg.rgb * BG_VP.bg.a, BG_VP.bg.a);

    if (BG_VP.grid_on > 0.5) {
        let canvas = canvas_coord(px, py_top, BG_VP);
        let s = BG_VP.grid_spacing;
        // 最近网格交点（画布空间锚定）
        let g = floor(canvas / s + vec2<f32>(0.5)) * s;
        let gs = g * BG_VP.zoom + vec2<f32>(BG_VP.pan_x, BG_VP.pan_y);
        let d = abs(gs - (vec2<f32>(px, py_top) + vec2<f32>(0.5)));
        if (d.x >= -0.5 && d.x < 0.5 && d.y >= -0.5 && d.y < 0.5) {
            col = vec4<f32>(BG_VP.dot_rgb.rgb, 1.0);
        }
    }
    return col;
}

// ── 累积复制通道（accum → 另一累积 / 输出）──
@group(0) @binding(0) var COPY_TEX: texture_2d<f32>;
@group(0) @binding(1) var COPY_SAMP: sampler;

@fragment
fn fs_copy(in: Vout) -> @location(0) vec4<f32> {
    return textureLoad(COPY_TEX, vec2<i32>(floor(in.pos.xy)), 0);
}

// ── 瓦片合成通道 ──
@group(0) @binding(0) var<uniform> T_VP: Vp;

struct TileU {
    origin: vec2<f32>,
    opacity: f32,
    mode: u32,
    has_mask: f32,
    has_parent: f32,
    f2l_a: f32,
    f2l_b: f32,
    f2l_c: f32,
    f2l_d: f32,
    f2l_e: f32,
    f2l_f: f32,
    pad: vec2<f32>,
    pad2: vec2<f32>,
};
@group(1) @binding(0) var TILE_TEX: texture_2d<f32>;
@group(1) @binding(1) var TILE_SAMP: sampler;
@group(1) @binding(2) var ACCUM_TEX: texture_2d<f32>;
@group(1) @binding(3) var ACCUM_SAMP: sampler;
@group(1) @binding(4) var<uniform> TILE_U: TileU;
@group(1) @binding(5) var MASK_TEX: texture_2d<f32>;
@group(1) @binding(6) var PARENT_TEX: texture_2d<f32>;

fn hard_light(cb: f32, cs: f32) -> f32 {
    if (cs <= 0.5) {
        return 2.0 * cb * cs;
    }
    return 1.0 - 2.0 * (1.0 - cb) * (1.0 - cs);
}

fn soft_light(cb: f32, cs: f32) -> f32 {
    if (cs <= 0.5) {
        return cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb);
    }
    var d = sqrt(cb);
    if (cb <= 0.25) {
        d = ((16.0 * cb - 12.0) * cb + 4.0) * cb;
    }
    return cb + (2.0 * cs - 1.0) * (d - cb);
}

// 与 paint-core blend.rs 的 blend_channel 逐条对应（W3C 公式）
fn blend_channel(mode: u32, cb: f32, cs: f32) -> f32 {
    switch (mode) {
        case 0u: { return cs; }                        // Normal
        case 1u: { return cb * cs; }                   // Multiply
        case 2u: { return cb + cs - cb * cs; }         // Screen
        case 3u: { return hard_light(cs, cb); }        // Overlay
        case 4u: { return min(cb, cs); }               // Darken
        case 5u: { return max(cb, cs); }               // Lighten
        case 6u: {                                    // ColorDodge
            if (cb <= 0.0) { return 0.0; }
            if (cs >= 1.0) { return 1.0; }
            return min(cb / (1.0 - cs), 1.0);
        }
        case 7u: {                                    // ColorBurn
            if (cb >= 1.0) { return 1.0; }
            if (cs <= 0.0) { return 0.0; }
            return 1.0 - min((1.0 - cb) / cs, 1.0);
        }
        case 8u: { return hard_light(cb, cs); }        // HardLight
        case 9u: { return soft_light(cb, cs); }        // SoftLight
        case 10u: { return abs(cb - cs); }             // Difference
        case 11u: { return cb + cs - 2.0 * cb * cs; }  // Exclusion
        default: { return cs; }
    }
}

@fragment
fn fs_tile(in: Vout) -> @location(0) vec4<f32> {
    let px = floor(in.pos.x);
    let py_top = floor(in.pos.y);
    let canvas = canvas_coord(px, py_top, T_VP);
    // 画布 → 瓦片局部（仿射；普通瓦片为平移 -origin，浮动瓦片为复合逆仿射）
    let local = vec2<f32>(
        TILE_U.f2l_a * canvas.x + TILE_U.f2l_b * canvas.y + TILE_U.f2l_e,
        TILE_U.f2l_c * canvas.x + TILE_U.f2l_d * canvas.y + TILE_U.f2l_f,
    );
    // 瓦片边界外由 scissor 裁剪；此处兜底
    if (local.x < 0.0 || local.y < 0.0 || local.x >= 256.0 || local.y >= 256.0) {
        discard;
    }

    let src = textureSampleLevel(TILE_TEX, TILE_SAMP, local / 256.0, 0.0);
    let as_raw = src.a;
    if (as_raw <= 0.0) {
        discard;
    }
    let opacity = clamp(TILE_U.opacity, 0.0, 1.0);
    // 蒙版（R 通道，1:1 最近邻）与剪贴父层（alpha）约束
    let li = vec2<i32>(floor(local));
    var as_eff = as_raw * opacity;
    if (TILE_U.has_mask > 0.5) {
        as_eff = as_eff * textureLoad(MASK_TEX, li, 0).r;
    }
    if (TILE_U.has_parent > 0.5) {
        as_eff = as_eff * textureLoad(PARENT_TEX, li, 0).a;
    }
    if (as_eff <= 0.0) {
        discard;
    }

    let dst = textureLoad(ACCUM_TEX, vec2<i32>(floor(in.pos.xy)), 0);
    let ab = dst.a;
    let ao = as_eff + ab * (1.0 - as_eff);
    if (ao <= 0.0) {
        return vec4<f32>(0.0);
    }

    let cs = src.rgb / max(as_raw, 1e-6);
    var cb = vec3<f32>(0.0);
    if (ab > 0.0) {
        cb = dst.rgb / max(ab, 1e-6);
    }
    let blended = vec3<f32>(
        blend_channel(TILE_U.mode, cb.r, cs.r),
        blend_channel(TILE_U.mode, cb.g, cs.g),
        blend_channel(TILE_U.mode, cb.b, cs.b),
    );
    let co = as_eff * ((1.0 - ab) * cs + ab * blended) + (1.0 - as_eff) * cb;
    return vec4<f32>(clamp(co, vec3<f32>(0.0), vec3<f32>(1.0)), ao);
}
