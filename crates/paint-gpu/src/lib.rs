//! paint-gpu：[`Renderer`] 的 wgpu 实现——**CPU 盖章 + GPU 合成**混合形态。
//!
//! - `stamp_dabs` / `merge_layers` 委托 paint-render 软件路径（GPU 盖章在 M4）
//! - `composite` 走 GPU：
//!   1. 瓦片纹理缓存——按 `Arc` 指针身份比对，只重传变更瓦片
//!   2. 背景网格 → 逐图层 ping-pong 累积（片元内做混合公式，支持全部
//!      12 种模式与图层不透明度）→ 回读进引擎 CPU 帧缓冲
//!   3. 累积纹理跨帧保留，脏区内增量渲染（与 CPU 路径同语义）
//! - 引擎与全部壳层零改动：桌面端经 feature "gpu" 换用本渲染器即可。

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;

use paint_core::blend::composite_pixel;
use paint_core::color::Color;
use paint_core::document::Document;
use paint_core::geometry::Rect;
use paint_core::history::StrokeRecorder;
use paint_core::layer::{BlendMode, Layer, LayerId};
use paint_core::render::Renderer;
use paint_core::stroke::Dab;
use paint_core::tile::{TileId, TILE};
use paint_render::{merge_layers as cpu_merge, stamp_dabs as cpu_stamp};
use wgpu::util::DeviceExt;

const SHADER: &str = include_str!("shader.wgsl");
const STAMP_SHADER: &str = include_str!("stamp_compute.wgsl");
/// 半径 > 此值走 GPU 盖章（基准：CPU ≤30px 0.5-1.1ms 无瓶颈）。
pub const GPU_STAMP_THRESHOLD: f32 = 20.0;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VpUniform {
    pan_x: f32,
    pan_y: f32,
    zoom: f32,
    inv_zoom: f32,
    screen_w: f32,
    screen_h: f32,
    grid_spacing: f32,
    grid_on: f32,
    bg: [f32; 4],
    dot_rgb: [f32; 4],
    rot_c: f32,
    rot_s: f32,
    flip: f32,
    _pad: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DabGpu {
    x: f32,
    y: f32,
    radius: f32,
    hardness: f32,
    cr: f32,
    cg: f32,
    cb: f32,
    ca: f32,
    alpha: f32,
    mode: u32,
    aspect: f32,
    angle: f32,
    scatter: f32,
    tile_ox: f32,
    tile_oy: f32,
    _pad: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TileUniform {
    origin: [f32; 2],
    opacity: f32,
    mode: u32,
    has_mask: f32,
    has_parent: f32,
    /// 画布→瓦片局部仿射（6 分量；普通瓦片 = 平移 -origin，
    /// 浮动瓦片 = 视口外再复合浮动逆仿射）
    f2l: [f32; 6],
    // 非破坏性调整
    adj_brightness: f32,
    adj_contrast: f32,
    adj_saturation: f32,
    adj_hue: f32,
    adj_strength: f32,
    has_adj: f32,
    pad: f32,
    _pad_reserved: f32,
}

struct CachedTile {
    texture: wgpu::Texture,
    /// 上传时的数据指针（Arc 身份），不同即重传
    ptr: usize,
}

/// GPU 渲染器。创建时初始化 wgpu 设备（离屏，无需窗口 surface）。
pub struct WgpuRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    bg_pipeline: wgpu::RenderPipeline,
    copy_pipeline: wgpu::RenderPipeline,
    tile_pipeline: wgpu::RenderPipeline,
    vp_layout: wgpu::BindGroupLayout,
    copy_layout: wgpu::BindGroupLayout,
    tile_layout: wgpu::BindGroupLayout,
    nearest: wgpu::Sampler,
    linear: wgpu::Sampler,
    // 累积双缓冲 + 输出纹理（尺寸随目标变化重建）
    accum: [Option<wgpu::Texture>; 2],
    out: Option<wgpu::Texture>,
    size: (u32, u32),
    accum_cur: usize,
    readback: Option<wgpu::Buffer>,
    readback_bpr: u32,
    // 瓦片缓存：(layer_raw, 通道, tile) → 纹理（通道 0=像素 1=蒙版）
    tiles: HashMap<(u64, u8, TileId), CachedTile>,
    // GPU 盖章
    stamp_pipeline: Option<wgpu::ComputePipeline>,
    stamp_layout: Option<wgpu::BindGroupLayout>,
    // 蒙版缺省（全显）/ 父层缺省（全隐）占位纹理
    white_tex: Option<wgpu::Texture>,
    black_tex: Option<wgpu::Texture>,
}

impl WgpuRenderer {
    /// 创建 GPU 渲染器（wgpu 设备初始化）。
    /// 返回 None = 无可用 GPU（宿主应回退 SoftwareRenderer）。
    pub fn new() -> Option<Self> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .ok()?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;

        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("paint-composite"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let vp_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vp"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let copy_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("copy"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let tile_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tile"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        let full = wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_fullscreen"),
            buffers: &[],
            compilation_options: Default::default(),
        };
        let mk = |frag: &str, layout: &[&wgpu::BindGroupLayout]| -> wgpu::RenderPipeline {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(frag),
                layout: Some(
                    &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some("layout"),
                        bind_group_layouts: layout,
                        push_constant_ranges: &[],
                    }),
                ),
                vertex: full.clone(),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(frag),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        blend: None, // 混合在片元内完成（读 dst 纹理）
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview: None,
                cache: None,
            })
        };
        let bg_pipeline = mk("fs_background", &[&vp_layout]);
        let copy_pipeline = mk("fs_copy", &[&copy_layout]);
        let tile_pipeline = mk("fs_tile", &[&vp_layout, &tile_layout]);

        let nearest = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("nearest"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let linear = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("linear"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        Some(Self {
            device,
            queue,
            bg_pipeline,
            copy_pipeline,
            tile_pipeline,
            vp_layout,
            copy_layout,
            tile_layout,
            nearest,
            linear,
            accum: [None, None],
            out: None,
            size: (0, 0),
            accum_cur: 0,
            readback: None,
            readback_bpr: 0,
            tiles: HashMap::new(),
            white_tex: None,
            black_tex: None,
            stamp_pipeline: None,
            stamp_layout: None,
        })
    }

    /// GPU compute 盖章：上传 dab 参数 + 逐瓦片 compute + 回读同步 CPU。
    /// 选区裁剪在 CPU 预处理（合并到 alpha）；撤销快照照常 CPU 采集。
    fn stamp_dabs_gpu(
        &mut self,
        grid: &mut paint_core::tile::TileGrid,
        dabs: &[Dab],
        clip: Option<&paint_core::tile::TileGrid>,
        recorder: &mut StrokeRecorder,
    ) {
        self.ensure_stamp_pipeline();
        let Some(pipeline) = self.stamp_pipeline.clone() else {
            return;
        };
        let Some(layout) = self.stamp_layout.clone() else {
            return;
        };

        // 收集受影响的瓦片 id（dab bbox 覆盖的瓦片）
        let mut tile_ids: std::collections::HashSet<TileId> = Default::default();
        for dab in dabs {
            let r = (dab.radius + 1.0) as i64;
            let (x0, y0) = ((dab.x as i64 - r), (dab.y as i64 - r));
            let (x1, y1) = ((dab.x as i64 + r), (dab.y as i64 + r));
            for ty in (y0 >> 8)..=(y1 >> 8) {
                for tx in (x0 >> 8)..=(x1 >> 8) {
                    tile_ids.insert(TileId {
                        x: tx as i32,
                        y: ty as i32,
                    });
                }
            }
        }
        let tile_ids: Vec<TileId> = tile_ids.into_iter().collect();
        if tile_ids.is_empty() {
            return;
        }

        // 1) 撤销采集 + 瓦片数据快照（compute 前的旧值）
        for tid in &tile_ids {
            recorder.capture(grid, *tid);
        }

        // 2) 上传 dab 参数到 storage buffer
        let mut gpu_dabs: Vec<DabGpu> = Vec::with_capacity(dabs.len() * tile_ids.len());
        for tid in &tile_ids {
            let (ox, oy) = tid.origin();
            for d in dabs {
                // 裁剪：选区内有效 alpha 比例（简化——逐 dab 中心采样）
                let clip_a = match clip {
                    None => 1.0f32,
                    Some(g) => g
                        .get(TileId::at(d.x as i64, d.y as i64))
                        .map(|t| {
                            let (tox, toy) = TileId::at(d.x as i64, d.y as i64).origin();
                            let lx = (d.x as i64 - tox).clamp(0, 255) as usize;
                            let ly = (d.y as i64 - toy).clamp(0, 255) as usize;
                            t.pixels()[(ly * 256 + lx) * 4] as f32 / 255.0
                        })
                        .unwrap_or(0.0),
                };
                let alpha = d.alpha * clip_a;
                if alpha <= 0.001 {
                    continue;
                }
                gpu_dabs.push(DabGpu {
                    x: d.x as f32,
                    y: d.y as f32,
                    radius: d.radius,
                    hardness: d.hardness,
                    cr: d.color.r as f32 / 255.0,
                    cg: d.color.g as f32 / 255.0,
                    cb: d.color.b as f32 / 255.0,
                    ca: 1.0,
                    alpha,
                    mode: if d.erase {
                        2
                    } else if d.mode == paint_core::DabMode::Wash {
                        1
                    } else {
                        0
                    },
                    aspect: d.aspect,
                    angle: d.angle,
                    scatter: d.scatter,
                    tile_ox: ox as f32,
                    tile_oy: oy as f32,
                    _pad: 0.0,
                });
            }
        }
        if gpu_dabs.is_empty() {
            return;
        }
        let dab_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("dab-params"),
                contents: bytemuck::cast_slice(&gpu_dabs),
                usage: wgpu::BufferUsages::STORAGE,
            });

        // 3) 逐瓦片 compute：上传 → dispatch → 回读
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("stamp"),
            });
        let mut readbacks: Vec<(TileId, wgpu::Buffer)> = Vec::new();

        for tid in &tile_ids {
            let (_ox, _oy) = tid.origin();
            // 确保瓦片存在（get_or_create 语义：GPU 端空纹理 = 全透明）
            let tile = grid.get_or_create_mut(*tid);
            let tile_data = tile.clone(); // Arc 共享

            // GPU 纹理（写入目标）——不复用合成缓存（格式不同：需 STORAGE 绑定）
            let src_tex = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("stamp-src"),
                size: wgpu::Extent3d {
                    width: TILE,
                    height: TILE,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &src_tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                tile_data.pixels(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(TILE * 4),
                    rows_per_image: Some(TILE),
                },
                wgpu::Extent3d {
                    width: TILE,
                    height: TILE,
                    depth_or_array_layers: 1,
                },
            );
            let dst_tex = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("stamp-dst"),
                size: wgpu::Extent3d {
                    width: TILE,
                    height: TILE,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });

            // 该瓦片涉及的 dab 数（z 维度）
            let n_buf = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("n-dabs"),
                    contents: bytemuck::bytes_of(&(gpu_dabs.len() as u32)),
                    usage: wgpu::BufferUsages::UNIFORM,
                });

            let src_view = src_tex.create_view(&Default::default());
            let dst_view = dst_tex.create_view(&Default::default());
            let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                layout: &layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: dab_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&src_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&dst_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&self.nearest),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: n_buf.as_entire_binding(),
                    },
                ],
                label: Some("stamp-bind"),
            });

            let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("stamp-cp"),
                timestamp_writes: None,
            });
            cpass.set_pipeline(&pipeline);
            cpass.set_bind_group(0, &bind, &[]);
            cpass.dispatch_workgroups(32, 32, 1);
            drop(cpass);

            // 回读缓冲
            let rb = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("stamp-rb"),
                size: (TILE * TILE * 4) as u64,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &dst_tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &rb,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(TILE * 4),
                        rows_per_image: None,
                    },
                },
                wgpu::Extent3d {
                    width: TILE,
                    height: TILE,
                    depth_or_array_layers: 1,
                },
            );
            readbacks.push((*tid, rb));
        }

        self.queue.submit([encoder.finish()]);

        // 4) 回读：映射 → 写回 CPU 瓦片（替换 Arc 为新 TileData）
        for (tid, rb) in &readbacks {
            let slice = rb.slice(..);
            let (tx, rx) = std::sync::mpsc::channel();
            slice.map_async(wgpu::MapMode::Read, move |r| {
                let _ = tx.send(r);
            });
            self.device.poll(wgpu::PollType::wait_indefinitely()).ok();
            if rx.recv().is_err() {
                continue;
            }
            if let Ok(Ok(())) = rx.try_recv() {}

            let data = slice.get_mapped_range();
            let mut new_tile = paint_core::TileData::transparent();
            new_tile.pixels_mut().copy_from_slice(&data);
            drop(data);
            rb.unmap();

            grid.set(*tid, std::sync::Arc::new(new_tile));
        }
    }

    fn ensure_stamp_pipeline(&mut self) {
        if self.stamp_pipeline.is_some() {
            return;
        }
        let module = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("stamp-compute"),
                source: wgpu::ShaderSource::Wgsl(STAMP_SHADER.into()),
            });
        let layout = self
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("stamp"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: false },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::StorageTexture {
                            access: wgpu::StorageTextureAccess::WriteOnly,
                            format: wgpu::TextureFormat::Rgba8Unorm,
                            view_dimension: wgpu::TextureViewDimension::D2,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });
        let pipeline = self
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("stamp-compute"),
                layout: Some(&self.device.create_pipeline_layout(
                    &wgpu::PipelineLayoutDescriptor {
                        label: Some("stamp-layout"),
                        bind_group_layouts: &[&layout],
                        push_constant_ranges: &[],
                    },
                )),
                module: &module,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                cache: None,
            });
        self.stamp_layout = Some(layout);
        self.stamp_pipeline = Some(pipeline);
    }

    fn placeholder(&mut self, white: bool) -> wgpu::Texture {
        let slot = if white {
            &mut self.white_tex
        } else {
            &mut self.black_tex
        };
        if slot.is_none() {
            let v = 255u8 * white as u8;
            let data = vec![v; (TILE * TILE * 4) as usize];
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(if white { "white" } else { "black" }),
                size: wgpu::Extent3d {
                    width: TILE,
                    height: TILE,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(TILE * 4),
                    rows_per_image: Some(TILE),
                },
                wgpu::Extent3d {
                    width: TILE,
                    height: TILE,
                    depth_or_array_layers: 1,
                },
            );
            *slot = Some(texture);
        }
        slot.as_ref().unwrap().clone()
    }

    fn ensure_size(&mut self, w: u32, h: u32) {
        if self.size == (w, h) {
            return;
        }
        let usage = wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC;
        let mk = |device: &wgpu::Device| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("accum"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage,
                view_formats: &[],
            })
        };
        self.accum = [Some(mk(&self.device)), Some(mk(&self.device))];
        self.out = Some(self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("out"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        }));
        self.size = (w, h);
        self.accum_cur = 0;
        self.tiles.clear();
    }

    fn vp_bind(&self, vp_uniform: &VpUniform) -> wgpu::BindGroup {
        let buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vp-u"),
                contents: bytemuck::bytes_of(vp_uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: &self.vp_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buf.as_entire_binding(),
            }],
            label: Some("vp-bg"),
        })
    }

    fn tile_texture(
        &mut self,
        layer: u64,
        chan: u8,
        id: TileId,
        tile: &Arc<paint_core::tile::TileData>,
    ) -> &wgpu::Texture {
        let ptr = Arc::as_ptr(tile) as usize;
        let need_upload = match self.tiles.get(&(layer, chan, id)) {
            Some(c) => c.ptr != ptr,
            None => true,
        };
        if need_upload {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("tile"),
                size: wgpu::Extent3d {
                    width: TILE,
                    height: TILE,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                tile.pixels(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(TILE * 4),
                    rows_per_image: Some(TILE),
                },
                wgpu::Extent3d {
                    width: TILE,
                    height: TILE,
                    depth_or_array_layers: 1,
                },
            );
            self.tiles
                .insert((layer, chan, id), CachedTile { texture, ptr });
        }
        &self.tiles.get(&(layer, chan, id)).unwrap().texture
    }

    fn copy_bind(&self, tex: &wgpu::Texture) -> wgpu::BindGroup {
        let view = tex.create_view(&Default::default());
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: &self.copy_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.nearest),
                },
            ],
            label: Some("copy"),
        })
    }
}

impl Renderer for WgpuRenderer {
    fn stamp_dabs(
        &mut self,
        grid: &mut paint_core::tile::TileGrid,
        dabs: &[Dab],
        clip: Option<&paint_core::tile::TileGrid>,
        recorder: &mut StrokeRecorder,
    ) {
        // 大笔刷走 GPU compute；小笔刷 CPU（基准：≤20px 半径 CPU <1ms）
        let max_r = dabs.iter().map(|d| d.radius).fold(0.0f32, f32::max);
        if max_r > GPU_STAMP_THRESHOLD && dabs.len() > 4 {
            self.stamp_dabs_gpu(grid, dabs, clip, recorder);
        } else {
            cpu_stamp(grid, dabs, clip, recorder);
        }
    }

    fn merge_layers(&mut self, dst: &mut Layer, src: &Layer, recorder: &mut StrokeRecorder) {
        cpu_merge(dst, src, recorder);
    }

    fn composite(
        &mut self,
        doc: &Document,
        target: &mut [u8],
        width: u32,
        dirty: Rect,
        background: Option<Color>,
    ) {
        let h = if width == 0 {
            0
        } else {
            (target.len() / (width as usize * 4)) as u32
        };
        if h == 0 {
            return;
        }
        self.ensure_size(width, h);
        let full = Rect::new(0, 0, width, h);
        let Some(region) = dirty.intersect(&full) else {
            return;
        };

        let vp = doc.viewport();
        let zoom = vp.zoom();
        let (pan_x, pan_y) = vp.pan();

        // 网格参数与 CPU 路径同逻辑
        let mut grid_spacing = 256.0f64;
        while grid_spacing * zoom < 32.0 {
            grid_spacing *= 2.0;
        }
        let grid_on = background.is_some() && doc.show_grid();
        let bg = background.unwrap_or(Color::WHITE);
        let lum = 0.299 * bg.r as f32 + 0.587 * bg.g as f32 + 0.114 * bg.b as f32;
        let mixc = |c: u8| -> f32 {
            if lum >= 128.0 {
                (c as f32 * 205.0) / 255.0
            } else {
                c as f32 + (255.0 - c as f32) * 45.0 / 255.0
            }
        };
        let vp_uniform = VpUniform {
            pan_x: pan_x as f32,
            pan_y: pan_y as f32,
            zoom: zoom as f32,
            inv_zoom: (1.0 / zoom) as f32,
            screen_w: width as f32,
            screen_h: h as f32,
            grid_spacing: grid_spacing as f32,
            grid_on: if grid_on { 1.0 } else { 0.0 },
            bg: [
                bg.r as f32 / 255.0,
                bg.g as f32 / 255.0,
                bg.b as f32 / 255.0,
                if background.is_some() { 1.0 } else { 0.0 },
            ],
            dot_rgb: [mixc(bg.r), mixc(bg.g), mixc(bg.b), 1.0],
            rot_c: vp.rotation().cos() as f32,
            rot_s: vp.rotation().sin() as f32,
            flip: if vp.flip_x() { 1.0 } else { 0.0 },
            _pad: 0.0,
        };

        // 脏区覆盖的瓦片范围：region 四角逆变换到画布取 AABB（旋转安全）
        let corners = [
            vp.screen_to_canvas(region.x as f64, region.y as f64),
            vp.screen_to_canvas(region.x2() as f64, region.y as f64),
            vp.screen_to_canvas(region.x as f64, region.y2() as f64),
            vp.screen_to_canvas(region.x2() as f64, region.y2() as f64),
        ];
        let cx0 = corners.iter().map(|c| c.0).fold(f64::MAX, f64::min);
        let cy0 = corners.iter().map(|c| c.1).fold(f64::MAX, f64::min);
        let cx1 = corners.iter().map(|c| c.0).fold(f64::MIN, f64::max);
        let cy1 = corners.iter().map(|c| c.1).fold(f64::MIN, f64::max);
        let tx0 = (cx0.floor() as i64) >> 8;
        let ty0 = (cy0.floor() as i64) >> 8;
        let tx1 = (cx1.ceil() as i64) >> 8;
        let ty1 = (cy1.ceil() as i64) >> 8;

        // 预上传瓦片纹理（借用冲突：先收集再上传）
        let mut uploads: Vec<(u64, TileId, Arc<paint_core::tile::TileData>)> = Vec::new();
        for (lid, layer) in doc.layers().iter_with_id() {
            if !layer.visible || layer.opacity <= 0.0 {
                continue;
            }
            for ty in ty0..=ty1 {
                for tx in tx0..=tx1 {
                    let id = TileId {
                        x: tx as i32,
                        y: ty as i32,
                    };
                    if let Some(t) = layer.content().get(id) {
                        uploads.push((lid.to_raw(), id, t.clone()));
                    }
                }
            }
        }
        for (l, id, t) in &uploads {
            self.tile_texture(*l, 0, *id, t);
        }
        // 蒙版瓦片上传（通道 1）
        let mut mask_uploads: Vec<(u64, TileId, Arc<paint_core::tile::TileData>)> = Vec::new();
        for (lid, layer) in doc.layers().iter_with_id() {
            if !layer.visible || layer.opacity <= 0.0 {
                continue;
            }
            let Some(mask) = &layer.mask else {
                continue;
            };
            for ty in ty0..=ty1 {
                for tx in tx0..=tx1 {
                    let id = TileId {
                        x: tx as i32,
                        y: ty as i32,
                    };
                    if let Some(t) = mask.get(id) {
                        mask_uploads.push((lid.to_raw(), id, t.clone()));
                    }
                }
            }
        }
        for (l, id, t) in &mask_uploads {
            self.tile_texture(*l, 1, *id, t);
        }
        // 浮动瓦片上传（通道 2）
        let mut float_uploads: Vec<(u64, TileId, Arc<paint_core::tile::TileData>)> = Vec::new();
        if let Some(fl) = doc.floating() {
            for (id, t) in fl.tiles.iter_entries() {
                float_uploads.push((u64::MAX, id, t.clone()));
            }
        }
        for (l, id, t) in &float_uploads {
            self.tile_texture(*l, 2, *id, t);
        }

        // 与 CPU 同规则：旋转/翻转下走最近邻（双线性邻域跨瓦片有接缝）
        let bilinear = zoom > 1.0 && vp.transform_ident();
        let sampler = if bilinear {
            self.linear.clone()
        } else {
            self.nearest.clone()
        };

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("composite"),
            });
        let scissor = |pass: &mut wgpu::RenderPass, r: &Rect| {
            pass.set_scissor_rect(r.x as u32, r.y as u32, r.w, r.h);
        };

        // 1) 背景网格 → accum[cur]（Load 保持脏区外历史内容）
        {
            let dst = self.accum[self.accum_cur].as_ref().unwrap();
            let view = dst.create_view(&Default::default());
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("bg"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.bg_pipeline);
            pass.set_bind_group(0, &self.vp_bind(&vp_uniform), &[]);
            scissor(&mut pass, &region);
            pass.draw(0..3, 0..1);
        }

        // 2) 逐图层：复制 accum[cur]→accum[1-cur]（脏区内），再盖瓦片
        let layers_vec: Vec<(paint_core::LayerId, &paint_core::layer::Layer)> =
            doc.layers().iter_with_id().collect();
        // 单独显示：solo 层存在时只画该层（与 CPU 合成同规则）
        let solo = doc
            .solo()
            .filter(|s| layers_vec.iter().any(|(id, _)| *id == *s));
        for (li, (lid, layer)) in layers_vec.iter().enumerate() {
            if !layer.visible || layer.opacity <= 0.0 {
                continue;
            }
            if solo.is_some() && Some(*lid) != solo {
                continue;
            }
            let adj = layer.adjustment.unwrap_or_default();
            let has_adj = layer.adjustment.map(|a| !a.is_identity()).unwrap_or(false);
            let parent: Option<(paint_core::LayerId, &paint_core::layer::Layer)> = if layer.clipped
            {
                layers_vec[..li]
                    .iter()
                    .rev()
                    .copied()
                    .find(|(_, l)| !l.clipped && l.visible)
            } else {
                None
            };
            let parent_raw = parent.map(|(pid, _)| pid.to_raw());
            let next = 1 - self.accum_cur;
            let src_tex = self.accum[self.accum_cur].as_ref().unwrap().clone();
            let dst_tex = self.accum[next].as_ref().unwrap().clone();
            {
                let view = dst_tex.create_view(&Default::default());
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("copy"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                pass.set_pipeline(&self.copy_pipeline);
                pass.set_bind_group(0, &self.copy_bind(&src_tex), &[]);
                scissor(&mut pass, &region);
                pass.draw(0..3, 0..1);
            }
            // 瓦片 pass
            let mode_idx = BlendMode::ALL
                .iter()
                .position(|m| *m == layer.blend_mode)
                .unwrap_or(0) as u32;
            let origin = dst_tex.create_view(&Default::default());
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("tiles"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &origin,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.tile_pipeline);
            let vp_bg = self.vp_bind(&vp_uniform);
            pass.set_bind_group(0, &vp_bg, &[]);
            for ty in ty0..=ty1 {
                for tx in tx0..=tx1 {
                    let id = TileId {
                        x: tx as i32,
                        y: ty as i32,
                    };
                    let Some(_) = layer.tiles.get(id) else {
                        continue;
                    };
                    let Some(cached) = self.tiles.get(&(lid.to_raw(), 0, id)) else {
                        continue;
                    };
                    let tile_tex = cached.texture.clone();
                    // 蒙版/父层纹理：缺省占位
                    let mask_view = match (&layer.mask, self.tiles.get(&(lid.to_raw(), 1, id))) {
                        (Some(_), Some(mc)) => mc.texture.create_view(&Default::default()),
                        _ => {
                            let t = self.placeholder(true);
                            t.create_view(&Default::default())
                        }
                    };
                    let parent_view = match parent_raw {
                        Some(praw) => match self.tiles.get(&(praw, 0, id)) {
                            Some(pc) => pc.texture.create_view(&Default::default()),
                            None => {
                                let t = self.placeholder(false);
                                t.create_view(&Default::default())
                            }
                        },
                        None => {
                            let t = self.placeholder(true);
                            t.create_view(&Default::default())
                        }
                    };
                    let tile_view = tile_tex.create_view(&Default::default());
                    let accum_view = src_tex.create_view(&Default::default());
                    let ubuf = self
                        .device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("tile-u"),
                            contents: bytemuck::bytes_of(&TileUniform {
                                origin: [(tx << 8) as f32, (ty << 8) as f32],
                                opacity: layer.opacity,
                                mode: mode_idx,
                                has_mask: if layer.mask.is_some() { 1.0 } else { 0.0 },
                                has_parent: if parent.is_some() { 1.0 } else { 0.0 },
                                f2l: [1.0, 0.0, 0.0, 1.0, -(tx << 8) as f32, -(ty << 8) as f32],
                                adj_brightness: adj.brightness,
                                adj_contrast: adj.contrast,
                                adj_saturation: adj.saturation,
                                adj_hue: adj.hue,
                                adj_strength: adj.strength,
                                has_adj: if has_adj { 1.0 } else { 0.0 },
                                pad: 0.0,
                                _pad_reserved: 0.0,
                            }),
                            usage: wgpu::BufferUsages::UNIFORM,
                        });
                    let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        layout: &self.tile_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(&tile_view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::Sampler(&sampler),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: wgpu::BindingResource::TextureView(&accum_view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 3,
                                resource: wgpu::BindingResource::Sampler(&self.nearest),
                            },
                            wgpu::BindGroupEntry {
                                binding: 4,
                                resource: ubuf.as_entire_binding(),
                            },
                            wgpu::BindGroupEntry {
                                binding: 5,
                                resource: wgpu::BindingResource::TextureView(&mask_view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 6,
                                resource: wgpu::BindingResource::TextureView(&parent_view),
                            },
                        ],
                        label: Some("tile"),
                    });
                    // 瓦片屏幕包围盒（4 角变换 AABB，旋转安全）∩ 脏区
                    let corners = [
                        vp.canvas_to_screen((tx << 8) as f64, (ty << 8) as f64),
                        vp.canvas_to_screen(((tx + 1) << 8) as f64, (ty << 8) as f64),
                        vp.canvas_to_screen((tx << 8) as f64, ((ty + 1) << 8) as f64),
                        vp.canvas_to_screen(((tx + 1) << 8) as f64, ((ty + 1) << 8) as f64),
                    ];
                    let x0 = corners.iter().map(|c| c.0).fold(f64::MAX, f64::min);
                    let y0 = corners.iter().map(|c| c.1).fold(f64::MAX, f64::min);
                    let x1 = corners.iter().map(|c| c.0).fold(f64::MIN, f64::max);
                    let y1 = corners.iter().map(|c| c.1).fold(f64::MIN, f64::max);
                    let tile_rect = Rect::new(
                        x0.floor() as i32,
                        y0.floor() as i32,
                        (x1.ceil() as i64 - x0.floor() as i64).max(1) as u32,
                        (y1.ceil() as i64 - y0.floor() as i64).max(1) as u32,
                    );
                    let Some(sc) = tile_rect.intersect(&region) else {
                        continue;
                    };
                    pass.set_bind_group(1, &bind, &[]);
                    scissor(&mut pass, &sc);
                    pass.draw(0..3, 0..1);
                }
            }
            self.accum_cur = next;
        }

        // 2.5) 浮动层（内容级变换预览）：Normal/不透明/最近邻
        if let Some(fl) = doc.floating() {
            let next = 1 - self.accum_cur;
            let dst_tex = self.accum[next].as_ref().unwrap().clone();
            let src_tex = self.accum[self.accum_cur].as_ref().unwrap().clone();
            {
                let view = dst_tex.create_view(&Default::default());
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("float-copy"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                pass.set_pipeline(&self.copy_pipeline);
                pass.set_bind_group(0, &self.copy_bind(&src_tex), &[]);
                scissor(&mut pass, &region);
                pass.draw(0..3, 0..1);
            }
            let inv = fl.affine.invert();
            let view = dst_tex.create_view(&Default::default());
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("float"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.tile_pipeline);
            let vp_bg = self.vp_bind(&vp_uniform);
            pass.set_bind_group(0, &vp_bg, &[]);
            for (fid, _ftile) in fl.tiles.iter_entries() {
                let Some(cached) = self.tiles.get(&(u64::MAX, 2, fid)) else {
                    continue;
                };
                let tile_tex = cached.texture.clone();
                let tile_view = tile_tex.create_view(&Default::default());
                let accum_view = src_tex.create_view(&Default::default());
                let white1 = self.placeholder(true);
                let white2 = self.placeholder(true);
                let w1v = white1.create_view(&Default::default());
                let w2v = white2.create_view(&Default::default());
                let ubuf = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("float-u"),
                        contents: bytemuck::bytes_of(&TileUniform {
                            origin: [0.0, 0.0],
                            opacity: 1.0,
                            mode: 0, // Normal
                            has_mask: 0.0,
                            has_parent: 0.0,
                            f2l: [
                                inv.a as f32,
                                inv.b as f32,
                                inv.c as f32,
                                inv.d as f32,
                                inv.e as f32,
                                inv.f as f32,
                            ],
                            adj_brightness: 0.0,
                            adj_contrast: 0.0,
                            adj_saturation: 0.0,
                            adj_hue: 0.0,
                            adj_strength: 1.0,
                            has_adj: 0.0,
                            pad: 0.0,
                            _pad_reserved: 0.0,
                        }),
                        usage: wgpu::BufferUsages::UNIFORM,
                    });
                let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    layout: &self.tile_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&tile_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&self.nearest),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::TextureView(&accum_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::Sampler(&self.nearest),
                        },
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: ubuf.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 5,
                            resource: wgpu::BindingResource::TextureView(&w1v),
                        },
                        wgpu::BindGroupEntry {
                            binding: 6,
                            resource: wgpu::BindingResource::TextureView(&w2v),
                        },
                    ],
                    label: Some("float"),
                });
                // 瓦片 4 角（float 源）→ 正仿射 → 画布 → 屏幕 AABB
                let (fox, foy) = fid.origin();
                let cs = [
                    fl.affine.apply(fox as f64, foy as f64),
                    fl.affine.apply(fox as f64 + 256.0, foy as f64),
                    fl.affine.apply(fox as f64, foy as f64 + 256.0),
                    fl.affine.apply(fox as f64 + 256.0, foy as f64 + 256.0),
                ];
                let ss: Vec<_> = cs.iter().map(|p| vp.canvas_to_screen(p.0, p.1)).collect();
                let x0 = ss.iter().map(|p| p.0).fold(f64::MAX, f64::min).floor() as i32;
                let y0 = ss.iter().map(|p| p.1).fold(f64::MAX, f64::min).floor() as i32;
                let x1 = ss.iter().map(|p| p.0).fold(f64::MIN, f64::max).ceil() as i64;
                let y1 = ss.iter().map(|p| p.1).fold(f64::MIN, f64::max).ceil() as i64;
                let rect = Rect::new(
                    x0,
                    y0,
                    (x1 - x0 as i64).max(1) as u32,
                    (y1 - y0 as i64).max(1) as u32,
                );
                let Some(sc) = rect.intersect(&region) else {
                    continue;
                };
                pass.set_bind_group(1, &bind, &[]);
                scissor(&mut pass, &sc);
                pass.draw(0..3, 0..1);
            }
            self.accum_cur = next;
        }

        // 3) 最终复制 accum → out（仅脏区）
        {
            let src_tex = self.accum[self.accum_cur].as_ref().unwrap();
            let out = self.out.as_ref().unwrap();
            let view = out.create_view(&Default::default());
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("final"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.copy_pipeline);
            pass.set_bind_group(0, &self.copy_bind(src_tex), &[]);
            scissor(&mut pass, &region);
            pass.draw(0..3, 0..1);
        }

        // 4) 回读脏区 → CPU
        let bpr_aligned = (region.w * 4).div_ceil(256) * 256;
        let buf_size = (bpr_aligned * region.h) as u64;
        if self.readback.as_ref().is_none_or(|b| b.size() != buf_size) {
            self.readback = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("readback"),
                size: buf_size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }));
            self.readback_bpr = bpr_aligned;
        }
        let out = self.out.as_ref().unwrap();
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: out,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: region.x as u32,
                    y: region.y as u32,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: self.readback.as_ref().unwrap(),
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.readback_bpr),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: region.w,
                height: region.h,
                depth_or_array_layers: 1,
            },
        );

        self.queue.submit([encoder.finish()]);
        let slice = self.readback.as_ref().unwrap().slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("wgpu device poll");
        rx.recv().expect("map callback").expect("map read");

        let data = slice.get_mapped_range();
        for row in 0..region.h as usize {
            let src_row = &data[row * self.readback_bpr as usize..][..region.w as usize * 4];
            let dst_off = (((region.y as usize + row) * width as usize) + region.x as usize) * 4;
            target[dst_off..dst_off + region.w as usize * 4].copy_from_slice(src_row);
        }
        drop(data);
        self.readback.as_ref().unwrap().unmap();
    }
}

impl Default for WgpuRenderer {
    fn default() -> Self {
        Self::new().expect("wgpu 设备初始化失败")
    }
}

// 占位使用 composite_pixel（保持与 CPU 路径一致的语义参考）
#[allow(dead_code)]
fn _blend_ref(dst: &mut [u8], src: &[u8], op: f32, mode: BlendMode) {
    composite_pixel(dst, src, op, mode);
}

#[allow(dead_code)]
fn _nz(v: u32) -> NonZeroU32 {
    NonZeroU32::new(v).unwrap()
}

#[allow(dead_code)]
fn _lid_ok(v: LayerId) -> bool {
    v.to_raw() != u64::MAX
}
