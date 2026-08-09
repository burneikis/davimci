//! Drawing the preview from the decoder's own planes.
//!
//! The window is the one host with a GPU, so it is the one host that can
//! take a planar frame and convert it on the card. The conversion itself
//! lives in `davimci-present::gpu`, asserted there against the CPU
//! reference; this file is only the `egui` plumbing that gets a frame into a
//! render pass at the right rectangle.
//!
//! It decides nothing: where the picture goes is the quad the presenter
//! computed, exactly as the RGBA path uses it.

use std::sync::Arc;

use davimci_backend::PlanarFrame;
use davimci_present::gpu::PlanarRenderer;
use eframe::egui_wgpu::{self, CallbackTrait};
// The window's own wgpu, so the device it hands over is the one the
// renderer's types are built against.
use eframe::wgpu;

/// Install the renderer into the frame's resources, once per window.
///
/// Returns whether the host can draw planar frames at all: without a `wgpu`
/// render state there is no device to convert on, and the window stays on
/// the composited RGBA texture.
#[must_use]
pub fn install(state: Option<&egui_wgpu::RenderState>) -> bool {
    let Some(state) = state else { return false };
    let renderer = PlanarRenderer::new(&state.device, state.target_format);
    state
        .renderer
        .write()
        .callback_resources
        .insert(PlanarResources { renderer });
    true
}

/// The pipeline and its uploaded planes, owned by `egui`'s frame resources
/// so they outlive any one paint.
struct PlanarResources {
    renderer: PlanarRenderer,
}

/// One frame's worth of work: upload in `prepare`, draw in `paint`.
struct PlanarCallback {
    frame: Arc<PlanarFrame>,
}

impl CallbackTrait for PlanarCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(res) = resources.get_mut::<PlanarResources>() {
            res.renderer.upload(device, queue, &self.frame);
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        if let Some(res) = resources.get::<PlanarResources>() {
            res.renderer.draw(pass);
        }
    }
}

/// Paint `frame` into `rect`, which is the quad the presenter letterboxed
/// the picture into.
pub fn draw(ui: &egui::Ui, rect: egui::Rect, frame: &Arc<PlanarFrame>) {
    ui.painter().add(egui_wgpu::Callback::new_paint_callback(
        rect,
        PlanarCallback {
            frame: Arc::clone(frame),
        },
    ));
}
