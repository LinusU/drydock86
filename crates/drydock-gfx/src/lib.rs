//! Drydock graphics: a headless-capable wgpu renderer implementing legacy fixed-function
//! rasterisation semantics (transforms, lighting, texture stages, alpha test, fog,
//! colour keys) for the emulated Direct3D layer.
//!
//! The renderer knows nothing about Windows. Callers identify textures and render targets
//! with their own `u32` keys, upload RGBA8 contents, record draws, and read results back.
//! The same renderer serves windowed and headless sessions; a window only consumes the
//! presentation copies it is handed.

use bytemuck::{Pod, Zeroable};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub use wgpu;

#[derive(Debug, Clone, serde::Serialize)]
pub struct AdapterReport {
    pub name: String,
    pub backend: String,
    pub device_type: String,
    pub driver: String,
    pub driver_info: String,
    pub vendor: u32,
    pub device: u32,
}

#[derive(Debug, Clone, Default)]
pub struct GpuOptions {
    /// Ask for the backend's fallback (software) adapter.
    pub software: bool,
    /// Restrict backends ("vulkan", "metal", "gl", "dx12"); empty = platform default.
    pub backends: Vec<String>,
}

pub struct Gpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub report: AdapterReport,
    /// Uncaptured device errors (validation failures), oldest first.
    pub errors: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Gpu {
    pub fn take_errors(&self) -> Vec<String> {
        std::mem::take(&mut *self.errors.lock().unwrap())
    }
}

impl Gpu {
    pub fn new(opts: &GpuOptions) -> Result<Arc<Gpu>, String> {
        let mut backends = wgpu::Backends::empty();
        for b in &opts.backends {
            backends |= match b.to_lowercase().as_str() {
                "vulkan" => wgpu::Backends::VULKAN,
                "metal" => wgpu::Backends::METAL,
                "gl" | "gles" => wgpu::Backends::GL,
                "dx12" => wgpu::Backends::DX12,
                other => return Err(format!("unknown backend {other}")),
            };
        }
        if backends.is_empty() {
            backends = wgpu::Backends::PRIMARY;
        }
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = backends;
        let instance = wgpu::Instance::new(desc);
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: opts.software,
            compatible_surface: None,
        }))
        .map_err(|e| {
            format!(
                "no graphics adapter available (backends {backends:?}, software={}): {e}. \
                 On Linux without a GPU install Mesa's Vulkan software driver (lavapipe).",
                opts.software
            )
        })?;
        let info = adapter.get_info();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("drydock"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            ..Default::default()
        }))
        .map_err(|e| format!("device creation failed on {}: {e}", info.name))?;
        // Validation errors are emulator bugs: record them instead of aborting.
        let errors: Arc<std::sync::Mutex<Vec<String>>> = Default::default();
        let e2 = errors.clone();
        device.on_uncaptured_error(Arc::new(move |e: wgpu::Error| {
            log::error!("wgpu: {e}");
            e2.lock().unwrap().push(e.to_string());
        }));
        let report = AdapterReport {
            name: info.name.clone(),
            backend: format!("{:?}", info.backend),
            device_type: format!("{:?}", info.device_type),
            driver: info.driver.clone(),
            driver_info: info.driver_info.clone(),
            vendor: info.vendor,
            device: info.device,
        };
        Ok(Arc::new(Gpu {
            instance,
            adapter,
            device,
            queue,
            report,
            errors,
        }))
    }
}

/// Canonical vertex consumed by the fixed-function shader.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, Pod, Zeroable)]
pub struct Vertex {
    pub pos: [f32; 4],
    pub normal: [f32; 3],
    /// RGBA8
    pub diffuse: [u8; 4],
    pub specular: [u8; 4],
    pub uv0: [f32; 2],
    pub uv1: [f32; 2],
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, Pod, Zeroable)]
pub struct GpuLight {
    pub kind: [u32; 4],
    pub diffuse: [f32; 4],
    pub specular: [f32; 4],
    pub ambient: [f32; 4],
    pub position: [f32; 4],
    pub direction: [f32; 4],
    pub params: [f32; 4],
    pub params2: [f32; 4],
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, Pod, Zeroable)]
pub struct GpuStage {
    pub ops: [u32; 4],
    pub ops2: [u32; 4],
    /// Texture transform: element count (0 = disabled), projected flag.
    pub tt: [u32; 4],
    pub texmat: [f32; 16],
}

/// Uniform block of the fixed-function shader (layout mirrors `ff.wgsl`).
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct Uniforms {
    pub world: [f32; 16],
    pub view: [f32; 16],
    pub proj: [f32; 16],
    pub normal_mat: [f32; 16],
    pub rt: [f32; 4],
    pub flags: [u32; 4],
    pub alpha_fog: [f32; 4],
    pub fog_color: [f32; 4],
    pub ambient: [f32; 4],
    pub tfactor: [f32; 4],
    pub mat_diffuse: [f32; 4],
    pub mat_ambient: [f32; 4],
    pub mat_specular: [f32; 4],
    pub mat_emissive: [f32; 4],
    pub mat_power: [f32; 4],
    pub color_src: [u32; 4],
    pub lights: [GpuLight; 8],
    pub stages: [GpuStage; 2],
}

impl Default for Uniforms {
    fn default() -> Self {
        Zeroable::zeroed()
    }
}

pub const F_PRETRANSFORMED: u32 = 1;
pub const F_LIGHTING: u32 = 2;
pub const F_HAS_NORMAL: u32 = 4;
pub const F_HAS_DIFFUSE: u32 = 8;
pub const F_HAS_SPECULAR: u32 = 16;
pub const F_SPECULAR_ENABLE: u32 = 32;
pub const F_LOCAL_VIEWER: u32 = 64;
pub const F_NORMALIZE: u32 = 128;
pub const F_COLORKEY: u32 = 256;
pub const F_ALPHATEST: u32 = 512;
pub const F_FOG: u32 = 1024;
pub const F_COLOR_VERTEX: u32 = 2048;
pub const F_CLEAR: u32 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Topology {
    Points,
    Lines,
    LineStrip,
    Triangles,
}

/// Blend factors in legacy numbering (1 = ZERO … 13 = BOTHINVSRCALPHA).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlendState {
    pub src: u32,
    pub dst: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DepthState {
    /// Legacy compare function (1 = NEVER … 8 = ALWAYS); 8 when testing is disabled.
    pub func: u32,
    pub write: bool,
    pub bias: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PipeKey {
    pub topology: Topology,
    pub blend: Option<BlendState>,
    pub depth: Option<DepthState>,
    /// 1 = none, 2 = cull clockwise, 3 = cull counter-clockwise (screen space).
    pub cull: u32,
    pub color_write: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SamplerKey {
    /// 1 = point, 2 = linear
    pub mag: u32,
    pub min: u32,
    /// 1 = none, 2 = point, 3 = linear
    pub mip: u32,
    /// 1 = wrap, 2 = mirror, 3 = clamp
    pub addr_u: u32,
    pub addr_v: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct Viewport {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub min_z: f32,
    pub max_z: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TexFormat {
    Color,
    Depth,
}

struct Tex {
    tex: wgpu::Texture,
    view: wgpu::TextureView,
    w: u32,
    h: u32,
    levels: u32,
    format: TexFormat,
}

struct DrawOp {
    pipe: PipeKey,
    uniform_offset: u32,
    first_vertex: u32,
    vertex_count: u32,
    first_index: u32,
    index_count: u32,
    textures: [Option<u32>; 2],
    samplers: [SamplerKey; 2],
    viewport: Viewport,
    scissor: [u32; 4],
}

struct PendingPass {
    target: u32,
    depth: Option<u32>,
    ops: Vec<DrawOp>,
    vertices: Vec<Vertex>,
    indices: Vec<u32>,
    uniforms: Vec<u8>,
    referenced: HashSet<u32>,
}

pub struct Renderer {
    pub gpu: Arc<Gpu>,
    textures: HashMap<u32, Tex>,
    pipelines: HashMap<PipeKey, wgpu::RenderPipeline>,
    samplers: HashMap<SamplerKey, wgpu::Sampler>,
    shader: wgpu::ShaderModule,
    layout: wgpu::PipelineLayout,
    ubo_layout: wgpu::BindGroupLayout,
    tex_layout: wgpu::BindGroupLayout,
    white: wgpu::TextureView,
    pending: Option<PendingPass>,
    uniform_align: u32,
    pub stats: RenderStats,
}

#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct RenderStats {
    pub draws: u64,
    pub vertices: u64,
    pub passes: u64,
    pub uploads: u64,
    pub upload_bytes: u64,
    pub readbacks: u64,
    pub readback_bytes: u64,
}

const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

fn blend_factor(f: u32, alpha: bool) -> wgpu::BlendFactor {
    use wgpu::BlendFactor as B;
    match f {
        1 => B::Zero,
        2 => B::One,
        3 => if alpha { B::SrcAlpha } else { B::Src },
        4 => if alpha { B::OneMinusSrcAlpha } else { B::OneMinusSrc },
        5 => B::SrcAlpha,
        6 => B::OneMinusSrcAlpha,
        7 => B::DstAlpha,
        8 => B::OneMinusDstAlpha,
        9 => if alpha { B::DstAlpha } else { B::Dst },
        10 => if alpha { B::OneMinusDstAlpha } else { B::OneMinusDst },
        11 => B::SrcAlphaSaturated,
        _ => B::One,
    }
}

fn compare(f: u32) -> wgpu::CompareFunction {
    use wgpu::CompareFunction as C;
    match f {
        1 => C::Never,
        2 => C::Less,
        3 => C::Equal,
        4 => C::LessEqual,
        5 => C::Greater,
        6 => C::NotEqual,
        7 => C::GreaterEqual,
        _ => C::Always,
    }
}

fn address(a: u32) -> wgpu::AddressMode {
    match a {
        2 => wgpu::AddressMode::MirrorRepeat,
        3 | 4 => wgpu::AddressMode::ClampToEdge,
        _ => wgpu::AddressMode::Repeat,
    }
}

impl Renderer {
    pub fn new(gpu: Arc<Gpu>) -> Renderer {
        let d = &gpu.device;
        let shader = d.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fixed-function"),
            source: wgpu::ShaderSource::Wgsl(include_str!("ff.wgsl").into()),
        });
        let ubo_layout = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ubo"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<Uniforms>() as u64),
                },
                count: None,
            }],
        });
        let tex_entry = |b: u32| wgpu::BindGroupLayoutEntry {
            binding: b,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let samp_entry = |b: u32| wgpu::BindGroupLayoutEntry {
            binding: b,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let tex_layout = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("textures"),
            entries: &[tex_entry(0), samp_entry(1), tex_entry(2), samp_entry(3)],
        });
        let layout = d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ff"),
            bind_group_layouts: &[Some(&ubo_layout), Some(&tex_layout)],
            immediate_size: 0,
        });
        let white_tex = d.create_texture(&wgpu::TextureDescriptor {
            label: Some("white"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: COLOR_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &white_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[255, 255, 255, 255],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let white = white_tex.create_view(&Default::default());
        let uniform_align = d.limits().min_uniform_buffer_offset_alignment;
        Renderer {
            gpu,
            textures: HashMap::new(),
            pipelines: HashMap::new(),
            samplers: HashMap::new(),
            shader,
            layout,
            ubo_layout,
            tex_layout,
            white,
            pending: None,
            uniform_align,
            stats: RenderStats::default(),
        }
    }

    pub fn has_texture(&self, key: u32) -> bool {
        self.textures.contains_key(&key)
    }

    /// Create (or recreate with a new size) a texture/render target.
    pub fn ensure_texture(&mut self, key: u32, w: u32, h: u32, levels: u32, format: TexFormat) {
        if let Some(t) = self.textures.get(&key)
            && t.w == w
            && t.h == h
            && t.levels == levels
            && t.format == format
        {
            return;
        }
        self.flush_if_referenced(key);
        let (fmt, usage) = match format {
            TexFormat::Color => (
                COLOR_FORMAT,
                wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC,
            ),
            TexFormat::Depth => (
                DEPTH_FORMAT,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
            ),
        };
        let tex = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("surface"),
            size: wgpu::Extent3d {
                width: w.max(1),
                height: h.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: levels.max(1),
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: fmt,
            usage,
            view_formats: &[],
        });
        let view = tex.create_view(&Default::default());
        self.textures.insert(
            key,
            Tex {
                tex,
                view,
                w,
                h,
                levels: levels.max(1),
                format,
            },
        );
    }

    pub fn drop_texture(&mut self, key: u32) {
        self.flush_if_referenced(key);
        self.textures.remove(&key);
    }

    fn flush_if_referenced(&mut self, key: u32) {
        if self.pending.as_ref().is_some_and(|p| p.referenced.contains(&key) || p.target == key || p.depth == Some(key)) {
            self.flush();
        }
    }

    /// Upload RGBA8 data into one mip level.
    pub fn upload(&mut self, key: u32, level: u32, w: u32, h: u32, rgba: &[u8]) {
        self.flush_if_referenced(key);
        let Some(t) = self.textures.get(&key) else {
            return;
        };
        self.stats.uploads += 1;
        self.stats.upload_bytes += rgba.len() as u64;
        self.gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &t.tex,
                mip_level: level,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Clear a rectangle of a color target and/or its depth target (implemented as a
    /// draw so it honours partial rectangles).
    pub fn clear(&mut self, target: u32, depth: Option<u32>, rect: [u32; 4], color: Option<[u8; 4]>, z: Option<f32>) {
        let Some(t) = self.textures.get(&target) else {
            return;
        };
        let (tw, th) = (t.w as f32, t.h as f32);
        let mut u = Uniforms::default();
        u.rt = [tw, th, tw, th];
        u.flags[0] = F_PRETRANSFORMED | F_HAS_DIFFUSE | F_CLEAR;
        let [x, y, w, h] = rect;
        let zz = z.unwrap_or(0.0);
        let c = color.unwrap_or([0; 4]);
        let v = |px: f32, py: f32| Vertex {
            pos: [px - 0.5, py - 0.5, zz, 1.0],
            diffuse: c,
            ..Default::default()
        };
        let (x0, y0, x1, y1) = (x as f32, y as f32, (x + w) as f32, (y + h) as f32);
        let verts = [v(x0, y0), v(x1, y0), v(x0, y1), v(x1, y1)];
        let key = PipeKey {
            topology: Topology::Triangles,
            blend: None,
            depth: z.map(|_| DepthState { func: 8, write: true, bias: 0 }),
            cull: 1,
            color_write: color.is_some(),
        };
        let vp = Viewport { x: 0.0, y: 0.0, w: tw, h: th, min_z: 0.0, max_z: 1.0 };
        self.draw(target, depth, key, &u, &verts, &[0, 1, 2, 2, 1, 3], [None, None], [SamplerKey::default(); 2], vp, [x, y, w, h]);
    }

    /// Read back a color target (RGBA8) or depth target (f32 as bytes).
    pub fn readback(&mut self, key: u32) -> Option<(u32, u32, Vec<u8>)> {
        self.flush_if_referenced(key);
        let t = self.textures.get(&key)?;
        let (w, h) = (t.w, t.h);
        let bpr = (w * 4).div_ceil(256) * 256;
        let buf = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (bpr * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.gpu.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &t.tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: if t.format == TexFormat::Depth {
                    wgpu::TextureAspect::DepthOnly
                } else {
                    wgpu::TextureAspect::All
                },
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bpr),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.gpu.queue.submit([enc.finish()]);
        let slice = buf.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = self.gpu.device.poll(wgpu::PollType::wait_indefinitely());
        let data = slice.get_mapped_range();
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            let o = (y * bpr) as usize;
            out.extend_from_slice(&data[o..o + (w * 4) as usize]);
        }
        drop(data);
        buf.unmap();
        self.stats.readbacks += 1;
        self.stats.readback_bytes += out.len() as u64;
        Some((w, h, out))
    }

    /// GPU copy between two color textures of the same size (e.g. for presentation).
    pub fn copy(&mut self, src: u32, dst: u32) {
        self.flush_if_referenced(src);
        self.flush_if_referenced(dst);
        let (Some(s), Some(d)) = (self.textures.get(&src), self.textures.get(&dst)) else {
            return;
        };
        let mut enc = self.gpu.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_texture(
            s.tex.as_image_copy(),
            d.tex.as_image_copy(),
            wgpu::Extent3d {
                width: s.w.min(d.w),
                height: s.h.min(d.h),
                depth_or_array_layers: 1,
            },
        );
        self.gpu.queue.submit([enc.finish()]);
    }

    pub fn texture_handle(&self, key: u32) -> Option<wgpu::Texture> {
        self.textures.get(&key).map(|t| t.tex.clone())
    }

    fn pipeline(&mut self, k: PipeKey, has_depth: bool) -> wgpu::RenderPipeline {
        let mut kk = k;
        if !has_depth {
            kk.depth = None;
        }
        if let Some(p) = self.pipelines.get(&kk) {
            return p.clone();
        }
        let blend = kk.blend.map(|b| wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: if b.src == 12 { wgpu::BlendFactor::SrcAlpha } else if b.src == 13 { wgpu::BlendFactor::OneMinusSrcAlpha } else { blend_factor(b.src, false) },
                dst_factor: if b.src == 12 { wgpu::BlendFactor::OneMinusSrcAlpha } else if b.src == 13 { wgpu::BlendFactor::SrcAlpha } else { blend_factor(b.dst, false) },
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: if b.src == 12 { wgpu::BlendFactor::SrcAlpha } else if b.src == 13 { wgpu::BlendFactor::OneMinusSrcAlpha } else { blend_factor(b.src, true) },
                dst_factor: if b.src == 12 { wgpu::BlendFactor::OneMinusSrcAlpha } else if b.src == 13 { wgpu::BlendFactor::SrcAlpha } else { blend_factor(b.dst, true) },
                operation: wgpu::BlendOperation::Add,
            },
        });
        let attrs = wgpu::vertex_attr_array![
            0 => Float32x4, 1 => Float32x3, 2 => Unorm8x4, 3 => Unorm8x4, 4 => Float32x2, 5 => Float32x2
        ];
        let (topology, strip) = match kk.topology {
            Topology::Points => (wgpu::PrimitiveTopology::PointList, None),
            Topology::Lines => (wgpu::PrimitiveTopology::LineList, None),
            Topology::LineStrip => (wgpu::PrimitiveTopology::LineStrip, Some(wgpu::IndexFormat::Uint32)),
            Topology::Triangles => (wgpu::PrimitiveTopology::TriangleList, None),
        };
        // Front faces are counter-clockwise in wgpu's y-up clip space, which is
        // clockwise on the y-down screen D3D reasons about.
        let cull = match kk.cull {
            2 => Some(wgpu::Face::Back),
            3 => Some(wgpu::Face::Front),
            _ => None,
        };
        let depth_stencil = if has_depth {
            let d = kk.depth.unwrap_or(DepthState {
                func: 8,
                write: false,
                bias: 0,
            });
            Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(d.write),
                depth_compare: Some(compare(d.func)),
                stencil: Default::default(),
                bias: wgpu::DepthBiasState {
                    constant: -d.bias * 16,
                    slope_scale: 0.0,
                    clamp: 0.0,
                },
            })
        } else {
            None
        };
        let p = self.gpu.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ff"),
            layout: Some(&self.layout),
            vertex: wgpu::VertexState {
                module: &self.shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &attrs,
                }],
            },
            primitive: wgpu::PrimitiveState {
                topology,
                strip_index_format: strip,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: cull,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &self.shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: COLOR_FORMAT,
                    blend,
                    write_mask: if kk.color_write {
                        wgpu::ColorWrites::ALL
                    } else {
                        wgpu::ColorWrites::empty()
                    },
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        self.pipelines.insert(kk, p.clone());
        p
    }

    fn sampler(&mut self, k: SamplerKey) -> wgpu::Sampler {
        if let Some(s) = self.samplers.get(&k) {
            return s.clone();
        }
        let f = |v: u32| if v >= 2 { wgpu::FilterMode::Linear } else { wgpu::FilterMode::Nearest };
        let s = self.gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ff"),
            address_mode_u: address(k.addr_u),
            address_mode_v: address(k.addr_v),
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: f(k.mag),
            min_filter: f(k.min),
            mipmap_filter: if k.mip >= 3 {
                wgpu::MipmapFilterMode::Linear
            } else {
                wgpu::MipmapFilterMode::Nearest
            },
            lod_min_clamp: 0.0,
            lod_max_clamp: if k.mip <= 1 { 0.0 } else { 32.0 },
            ..Default::default()
        });
        self.samplers.insert(k, s.clone());
        s
    }

    /// Record a draw into the pass for (target, depth). Indices are local to `vertices`.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        target: u32,
        depth: Option<u32>,
        pipe: PipeKey,
        uniforms: &Uniforms,
        vertices: &[Vertex],
        indices: &[u32],
        textures: [Option<u32>; 2],
        samplers: [SamplerKey; 2],
        viewport: Viewport,
        scissor: [u32; 4],
    ) {
        if vertices.is_empty() || (!indices.is_empty() && indices.len() < 2) {
            return;
        }
        // A draw whose depth target is missing renders without depth.
        let depth = depth.filter(|d| self.textures.contains_key(d));
        if self.pending.as_ref().is_some_and(|p| p.target != target || p.depth != depth) {
            self.flush();
        }
        let p = self.pending.get_or_insert_with(|| PendingPass {
            target,
            depth,
            ops: Vec::new(),
            vertices: Vec::new(),
            indices: Vec::new(),
            uniforms: Vec::new(),
            referenced: HashSet::new(),
        });
        let align = self.uniform_align.max(256) as usize;
        let uo = p.uniforms.len().div_ceil(align) * align;
        p.uniforms.resize(uo, 0);
        p.uniforms.extend_from_slice(bytemuck::bytes_of(uniforms));
        let first_vertex = p.vertices.len() as u32;
        p.vertices.extend_from_slice(vertices);
        let first_index = p.indices.len() as u32;
        if indices.is_empty() {
            p.indices.extend(0..vertices.len() as u32);
        } else {
            p.indices.extend_from_slice(indices);
        }
        let index_count = p.indices.len() as u32 - first_index;
        for t in textures.iter().flatten() {
            p.referenced.insert(*t);
        }
        p.ops.push(DrawOp {
            pipe,
            uniform_offset: uo as u32,
            first_vertex,
            vertex_count: vertices.len() as u32,
            first_index,
            index_count,
            textures,
            samplers,
            viewport,
            scissor,
        });
        self.stats.draws += 1;
        self.stats.vertices += vertices.len() as u64;
    }

    /// Encode and submit all recorded work.
    pub fn flush(&mut self) {
        let Some(p) = self.pending.take() else {
            return;
        };
        if p.ops.is_empty() {
            return;
        }
        let d = self.gpu.device.clone();
        use wgpu::util::DeviceExt;
        let vb = d.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vertices"),
            contents: bytemuck::cast_slice(&p.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let ib = d.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("indices"),
            contents: bytemuck::cast_slice(&p.indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        let mut ubytes = p.uniforms.clone();
        ubytes.resize(ubytes.len() + std::mem::size_of::<Uniforms>(), 0);
        let ub = d.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("uniforms"),
            contents: &ubytes,
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let ubg = d.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ubo"),
            layout: &self.ubo_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &ub,
                    offset: 0,
                    size: wgpu::BufferSize::new(std::mem::size_of::<Uniforms>() as u64),
                }),
            }],
        });
        // Bind groups per distinct texture/sampler combination.
        let mut groups: HashMap<([Option<u32>; 2], [SamplerKey; 2]), wgpu::BindGroup> = HashMap::new();
        let mut pipes = Vec::with_capacity(p.ops.len());
        let has_depth = p.depth.is_some_and(|k| self.textures.contains_key(&k));
        for op in &p.ops {
            pipes.push(self.pipeline(op.pipe, has_depth));
            let key = (op.textures, op.samplers);
            if !groups.contains_key(&key) {
                let s0 = self.sampler(op.samplers[0]);
                let s1 = self.sampler(op.samplers[1]);
                let v0 = op.textures[0].and_then(|t| self.textures.get(&t)).map(|t| &t.view).unwrap_or(&self.white);
                let v1 = op.textures[1].and_then(|t| self.textures.get(&t)).map(|t| &t.view).unwrap_or(&self.white);
                let bg = d.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("tex"),
                    layout: &self.tex_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(v0),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&s0),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::TextureView(v1),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::Sampler(&s1),
                        },
                    ],
                });
                groups.insert(key, bg);
            }
        }
        let Some(target) = self.textures.get(&p.target) else {
            return;
        };
        let (tw, th) = (target.w, target.h);
        let depth_view = if has_depth {
            p.depth.and_then(|k| self.textures.get(&k)).map(|t| &t.view)
        } else {
            None
        };
        let mut enc = d.create_command_encoder(&Default::default());
        {
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ff"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: depth_view.map(|v| wgpu::RenderPassDepthStencilAttachment {
                    view: v,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            rp.set_vertex_buffer(0, vb.slice(..));
            rp.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
            for (op, pipe) in p.ops.iter().zip(&pipes) {
                let vp = op.viewport;
                let vx = vp.x.clamp(0.0, tw as f32);
                let vy = vp.y.clamp(0.0, th as f32);
                let vw = vp.w.min(tw as f32 - vx).max(1.0);
                let vh = vp.h.min(th as f32 - vy).max(1.0);
                rp.set_viewport(vx, vy, vw, vh, vp.min_z.clamp(0.0, 1.0), vp.max_z.clamp(0.0, 1.0).max(vp.min_z.clamp(0.0, 1.0)));
                let [sx, sy, sw, sh] = op.scissor;
                let sx = sx.min(tw);
                let sy = sy.min(th);
                let sw = sw.min(tw - sx);
                let sh = sh.min(th - sy);
                if sw == 0 || sh == 0 {
                    continue;
                }
                rp.set_scissor_rect(sx, sy, sw, sh);
                rp.set_pipeline(pipe);
                rp.set_bind_group(0, &ubg, &[op.uniform_offset]);
                rp.set_bind_group(1, &groups[&(op.textures, op.samplers)], &[]);
                let _ = op.vertex_count;
                rp.draw_indexed(op.first_index..op.first_index + op.index_count, op.first_vertex as i32, 0..1);
            }
        }
        self.gpu.queue.submit([enc.finish()]);
        self.stats.passes += 1;
    }
}
