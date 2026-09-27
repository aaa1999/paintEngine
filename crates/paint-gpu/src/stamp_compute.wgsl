// GPU 盖章 compute shader：dab 参数 → 瓦片像素写入。
// 每个 workgroup (8,8,1) 处理一个 dab 的 256×256 瓦片区域。
// 大笔刷（半径 > GPU_STAMP_THRESHOLD）走此路径。

struct DabGpu {
    // 画布坐标
    x: f32,
    y: f32,
    radius: f32,
    hardness: f32,
    // 颜色（预乘）
    cr: f32,
    cg: f32,
    cb: f32,
    ca: f32, // = alpha（预乘前）
    alpha: f32,
    // 模式：0=buildup, 1=wash, 2=erase
    mode: u32,
    // 各向异性
    aspect: f32,
    angle: f32,
    // 散布
    scatter: f32,
    // 瓦片原点（画布）
    tile_ox: f32,
    tile_oy: f32,
};

@group(0) @binding(0) var<storage, read> dabs: array<DabGpu>;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var dst: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(3) var samp: sampler;
@group(0) @binding(4) var<uniform> n_dabs: u32;

fn falloff(t: f32, hardness: f32) -> f32 {
    if (hardness >= 0.999) {
        if (t < 1.0) { return 1.0; }
        return 0.0;
    }
    if (t <= hardness) { return 1.0; }
    return (1.0 - t) / (1.0 - hardness);
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let px = vec2<i32>(gid.xy);
    if (px.x >= 256 || px.y >= 256) { return; }

    var out_c = textureLoad(src, px, 0);

    // 遍历全部 dabs（单 z 层）
    for (var di: u32 = 0u; di < n_dabs; di = di + 1u) {
        let d = dabs[di];

    // 画布坐标
    let canvas_x = d.tile_ox + f32(px.x) + 0.5;
    let canvas_y = d.tile_oy + f32(px.y) + 0.5;

    // 各向异性距离
    let dx0 = canvas_x - d.x;
    let dy0 = canvas_y - d.y;
    let aspect = clamp(d.aspect, 0.05, 1.0);
    let ca = cos(d.angle);
    let sa = sin(d.angle);
    let dx = ca * dx0 + sa * dy0;   // 长轴
    let dy = -sa * dx0 + ca * dy0;  // 短轴
    let inv_ry = 1.0 / (d.radius * aspect);
    let t2 = (dx / d.radius) * (dx / d.radius) + (dy * inv_ry) * (dy * inv_ry);
    if (t2 >= 1.0) { return; }

    let t = sqrt(t2);
    var a = falloff(t, d.hardness) * d.alpha;
    if (a <= 0.004) { return; }

        if (d.mode == 2u) {
            // erase
            let k = 1.0 - a;
            out_c = vec4<f32>(out_c.rgb * k, out_c.a * k);
        } else if (d.mode == 1u) {
            // wash：取最大覆盖
            if (a > out_c.a) {
                out_c = vec4<f32>(d.cr * a, d.cg * a, d.cb * a, a);
            }
        } else {
            // buildup（source-over 预乘）
            let oa = a + out_c.a * (1.0 - a);
            if (oa > 0.0) {
                out_c = vec4<f32>(
                    d.cr * a + out_c.r * (1.0 - a),
                    d.cg * a + out_c.g * (1.0 - a),
                    d.cb * a + out_c.b * (1.0 - a),
                    oa,
                );
            }
        }
    }
    textureStore(dst, px, out_c);
}
