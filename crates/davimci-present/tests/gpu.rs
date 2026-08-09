//! The shader conversion, asserted against the CPU one.
//!
//! Software decode and the CPU conversion are the reference; this test is
//! what makes the GPU path a reproduction of it rather than a second
//! opinion. It runs on any adapter `wgpu` can find, lavapipe included, and
//! skips - loudly - when there is none, because a machine with no GPU must
//! not fail a suite it cannot run.

#![cfg(feature = "gpu")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use davimci_backend::PlanarFrame;
use davimci_core::Frame;
use davimci_present::gpu::{CHROMA_UPSAMPLE_TOLERANCE, PlanarRenderer, SHADER_TOLERANCE};

const WIDTH: u32 = 64;
const HEIGHT: u32 = 32;

/// A picture with structure in all three planes, so a conversion that
/// ignored chroma - or swapped U and V - could not pass.
fn frame() -> PlanarFrame {
    let cw = WIDTH.div_ceil(2) as usize;
    let ch = HEIGHT.div_ceil(2) as usize;
    let mut y = Vec::with_capacity((WIDTH * HEIGHT) as usize);
    for row in 0..HEIGHT {
        for col in 0..WIDTH {
            y.push(u8::try_from(16 + (row * 4 + col * 3) % 220).unwrap_or(16));
        }
    }
    let mut u = Vec::with_capacity(cw * ch);
    let mut v = Vec::with_capacity(cw * ch);
    for row in 0..ch {
        for col in 0..cw {
            u.push(u8::try_from(40 + (col * 5) % 180).unwrap_or(128));
            v.push(u8::try_from(30 + (row * 7) % 200).unwrap_or(128));
        }
    }
    PlanarFrame {
        position: Frame(0),
        width: WIDTH,
        height: HEIGHT,
        y,
        u,
        v,
    }
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

fn gpu() -> Option<Gpu> {
    let instance = wgpu::Instance::default();
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    Some(Gpu { device, queue })
}

/// Render the frame at 1:1 and read the pixels back.
fn render(gpu: &Gpu, frame: &PlanarFrame) -> Vec<u8> {
    let mut renderer = PlanarRenderer::new(&gpu.device, wgpu::TextureFormat::Rgba8Unorm);
    renderer.upload(&gpu.device, &gpu.queue, frame);
    assert!(renderer.is_ready(), "the frame was not uploaded");

    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("davimci-test-target"),
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    // 256-byte row alignment is a copy requirement, not a picture property.
    let row = (WIDTH * 4).div_ceil(256) * 256;
    let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("davimci-test-readback"),
        size: u64::from(row * HEIGHT),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("davimci-test-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        renderer.draw(&mut pass);
    }
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(HEIGHT),
            },
        },
        wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit(Some(encoder.finish()));

    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    let mapped = slice.get_mapped_range();
    let mut out = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for y in 0..HEIGHT {
        let start = (y * row) as usize;
        out.extend_from_slice(&mapped[start..start + (WIDTH * 4) as usize]);
    }
    drop(mapped);
    readback.unmap();
    out
}

/// The worst per-channel difference between the shader and the CPU
/// conversion of the same frame, with alpha asserted exactly.
fn worst_difference(gpu: &Gpu, frame: &PlanarFrame) -> i32 {
    let want = frame.to_rgba();
    let got = render(gpu, frame);
    assert_eq!(got.len(), want.rgba.len());
    let mut worst = 0i32;
    for (i, (a, b)) in want.rgba.iter().zip(&got).enumerate() {
        // Both write opaque alpha, so a difference there is a bug rather
        // than a rounding step.
        if i % 4 == 3 {
            assert_eq!(*a, *b, "alpha differs at byte {i}");
            continue;
        }
        worst = worst.max((i32::from(*a) - i32::from(*b)).abs());
    }
    worst
}

/// With flat chroma there is nothing to interpolate, so what is left is the
/// matrix itself: the shader must reproduce the CPU conversion to within the
/// rounding of one f32 step.
#[test]
fn the_shader_matrix_reproduces_the_cpu_conversion() {
    let Some(gpu) = gpu() else {
        eprintln!("skipping: no wgpu adapter on this machine");
        return;
    };
    let mut flat = frame();
    flat.u.fill(90);
    flat.v.fill(200);
    let worst = worst_difference(&gpu, &flat);
    assert!(
        worst <= i32::from(SHADER_TOLERANCE),
        "the shader matrix is off the CPU conversion by {worst}, over the documented tolerance \
         of {SHADER_TOLERANCE}"
    );
}

/// With chroma that varies, the shader interpolates where the CPU reference
/// repeats. That difference is bounded and named; it is not licence for the
/// picture to be a different one.
#[test]
fn the_shader_only_differs_from_the_cpu_by_chroma_upsampling() {
    let Some(gpu) = gpu() else {
        eprintln!("skipping: no wgpu adapter on this machine");
        return;
    };
    let worst = worst_difference(&gpu, &frame());
    assert!(
        worst <= i32::from(CHROMA_UPSAMPLE_TOLERANCE),
        "the shader is off the CPU conversion by {worst}, more than chroma upsampling explains"
    );
    assert!(
        worst > i32::from(SHADER_TOLERANCE),
        "chroma is no longer being interpolated; the tolerance is measuring nothing"
    );
}

#[test]
fn a_malformed_frame_is_not_uploaded() {
    let Some(gpu) = gpu() else {
        eprintln!("skipping: no wgpu adapter on this machine");
        return;
    };
    let mut renderer = PlanarRenderer::new(&gpu.device, wgpu::TextureFormat::Rgba8Unorm);
    let mut broken = frame();
    broken.u.clear();
    renderer.upload(&gpu.device, &gpu.queue, &broken);
    assert!(
        !renderer.is_ready(),
        "a short plane must be refused, not copied past its end"
    );
}
