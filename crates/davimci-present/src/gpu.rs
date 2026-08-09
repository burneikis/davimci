//! Planar YUV upload, converted in a shader.
//!
//! The CPU path in [`crate::presenter`] stays the reference: it is what the
//! parity and snapshot tests assert, and nothing here may redefine those
//! pixels. This module is the host-side addition the GPU plan asks for - a
//! host that owns a `wgpu` device uploads the three planes as they came out
//! of the decoder and converts them on the card, which is three eighths of
//! the bytes of an RGBA8 upload and no CPU colour conversion at all.
//!
//! Correctness is settled against [`PlanarFrame::to_rgba`]: the shader
//! implements the same BT.709 limited-range matrix, and the test renders a
//! frame and compares it to the CPU conversion under a tolerance that is
//! documented here and applied nowhere else.

use davimci_backend::PlanarFrame;

/// How far a shader-converted channel may sit from the CPU conversion of the
/// same pixel, where the two are doing the same arithmetic.
///
/// Integers scaled by 1024 on the CPU, f32 on the card: the same matrix,
/// rounded differently in the last step. This tolerance belongs to the GPU
/// path alone and must never be applied to a CPU-path comparison.
pub const SHADER_TOLERANCE: u8 = 2;

/// The extra difference chroma upsampling accounts for.
///
/// The CPU reference repeats each chroma sample over its 2x2 luma block; the
/// shader samples the half-resolution planes with linear filtering, which
/// interpolates across the block boundary. That is a deliberate difference -
/// interpolated chroma is the better picture, and the CPU path stays simple
/// - so it is measured and named rather than folded into
///   [`SHADER_TOLERANCE`]. It applies only where chroma actually varies.
pub const CHROMA_UPSAMPLE_TOLERANCE: u8 = 12;

const SHADER: &str = r"
struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// One oversized triangle covering the viewport: no vertex buffer, and no
// seam down the middle the way two triangles have.
@vertex
fn vs(@builtin(vertex_index) index: u32) -> VertexOut {
    var out: VertexOut;
    let x = f32((index << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(index & 2u) * 2.0 - 1.0;
    out.position = vec4<f32>(x, -y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (y + 1.0) * 0.5);
    return out;
}

@group(0) @binding(0) var luma: texture_2d<f32>;
@group(0) @binding(1) var chroma_b: texture_2d<f32>;
@group(0) @binding(2) var chroma_r: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;

// BT.709, limited range: the same matrix as `PlanarFrame::to_rgba`, which is
// what this is asserted against.
@fragment
fn fs(in: VertexOut) -> @location(0) vec4<f32> {
    let y = (textureSample(luma, samp, in.uv).r * 255.0 - 16.0) * 1.164383;
    let u = textureSample(chroma_b, samp, in.uv).r * 255.0 - 128.0;
    let v = textureSample(chroma_r, samp, in.uv).r * 255.0 - 128.0;
    let rgb = vec3<f32>(
        y + 1.792741 * v,
        y - 0.213249 * u - 0.532909 * v,
        y + 2.112402 * u,
    ) / 255.0;
    return vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
";

/// The three planes on the card, plus the pipeline that converts them.
///
/// One per host, reused across frames: the textures are reallocated only
/// when the picture changes size, because a per-frame allocation at refresh
/// rate is the cost this path exists to avoid.
#[derive(Debug)]
pub struct PlanarRenderer {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    planes: Option<Planes>,
}

#[derive(Debug)]
struct Planes {
    width: u32,
    height: u32,
    y: wgpu::Texture,
    u: wgpu::Texture,
    v: wgpu::Texture,
    bind_group: wgpu::BindGroup,
}

impl PlanarRenderer {
    /// Build the pipeline against the host's own device, so the video is
    /// drawn in the host's render pass rather than in a second one.
    #[must_use]
    pub fn new(device: &wgpu::Device, target: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("davimci-planar"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let plane = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("davimci-planar"),
            entries: &[
                plane(0),
                plane(1),
                plane(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("davimci-planar"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("davimci-planar"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        // Linear between texels, clamped at the edges: the picture is scaled
        // to a pane, and clamping is what keeps the last row of chroma from
        // wrapping onto the first.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("davimci-planar"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            pipeline,
            layout,
            sampler,
            planes: None,
        }
    }

    /// Whether a frame has been uploaded and can be drawn.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.planes.is_some()
    }

    /// Put one decoded frame on the card.
    ///
    /// A frame whose planes do not match its dimensions is dropped rather
    /// than uploaded: a short plane would be read past its end by the copy.
    pub fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, frame: &PlanarFrame) {
        if !frame.is_well_formed() || frame.width == 0 || frame.height == 0 {
            return;
        }
        let needs_alloc = self
            .planes
            .as_ref()
            .is_none_or(|p| p.width != frame.width || p.height != frame.height);
        if needs_alloc {
            self.planes = Some(self.allocate(device, frame));
        }
        let Some(planes) = &self.planes else { return };
        write(queue, &planes.y, frame.width, frame.height, &frame.y);
        let (cw, ch) = (frame.chroma_width(), frame.chroma_height());
        write(queue, &planes.u, cw, ch, &frame.u);
        write(queue, &planes.v, cw, ch, &frame.v);
    }

    /// Draw the uploaded frame over the pass's current viewport, which is
    /// where the host has already put the letterboxed quad.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        let Some(planes) = &self.planes else { return };
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &planes.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    fn allocate(&self, device: &wgpu::Device, frame: &PlanarFrame) -> Planes {
        let plane = |label, width: u32, height: u32| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        };
        let y = plane("davimci-plane-y", frame.width, frame.height);
        let u = plane(
            "davimci-plane-u",
            frame.chroma_width(),
            frame.chroma_height(),
        );
        let v = plane(
            "davimci-plane-v",
            frame.chroma_width(),
            frame.chroma_height(),
        );
        let view = |t: &wgpu::Texture| t.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("davimci-planar"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view(&y)),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view(&u)),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&view(&v)),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        Planes {
            width: frame.width,
            height: frame.height,
            y,
            u,
            v,
            bind_group,
        }
    }
}

fn write(queue: &wgpu::Queue, texture: &wgpu::Texture, width: u32, height: u32, bytes: &[u8]) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
}
