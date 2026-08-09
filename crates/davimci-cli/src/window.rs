//! The window.
//!
//! The eframe application lives in the binary because it is the one place
//! that holds a frontend *and* a render backend at once, and no frontend may
//! reference MLT. It does four things per frame: hand egui's
//! input to the `Gui` frontend, pump the resulting events through the `App`,
//! run one presentation tick, and draw what `davimci-gui` computed.
//!
//! It decides nothing. Layout, painting, key meaning, and what the status
//! line says were all settled before this file runs.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "winit and egui measure in f32 pixels; window-sized values convert exactly"
)]

use davimci_app::{App, Event, Frontend, Host, Surface};
use davimci_core::Resolution;
use davimci_gui::egui_shell;
use davimci_gui::{Chrome, Gui, GuiEvent, VideoHeight, VideoQuad};

use crate::editor::Editor;
use crate::setting::PreviewHeight;

/// `:set previewheight` in the window's terms. Unset gives the pane the
/// share a fresh window opens with; `0` hands the whole window to the
/// timeline.
fn video_height(setting: Option<PreviewHeight>) -> VideoHeight {
    match setting.unwrap_or(PreviewHeight::Auto) {
        PreviewHeight::Off => VideoHeight::Off,
        PreviewHeight::Rows(rows) => VideoHeight::Rows(rows),
        PreviewHeight::Percent(pc) => VideoHeight::Percent(pc),
        PreviewHeight::Auto => VideoHeight::Auto,
    }
}

/// Window title, kept in one place so `:e` can extend it later.
const TITLE: &str = "davimci";

/// The running editor plus its window state.
pub struct Window {
    app: App,
    editor: Editor,
    gui: Gui,
    texture: Option<egui::TextureHandle>,
    /// `pixels_id` of the composition already on the GPU. A held frame is
    /// handed back with the same id every tick, and re-uploading it at
    /// refresh rate is what a repeated frame must not cost.
    uploaded: Option<u64>,
    /// Size of the video pane last frame, so the presenter is resized only
    /// when it actually changed.
    video_size: Resolution,
    /// One GPU texture per clip thumbnail, kept across frames.
    thumbnails: egui_shell::ThumbnailTextures,
    /// Whether this window can convert planar frames on the card. False
    /// without a `wgpu` render state, where the composited RGBA texture is
    /// the only path.
    planar: bool,
    /// Set once the close has been asked for, so background work is
    /// cancelled once rather than on every frame the viewport takes to go.
    closing: bool,
}

impl std::fmt::Debug for Window {
    /// `egui::TextureHandle` is not `Debug`, and a window in a log line
    /// should be a summary anyway.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Window")
            .field("video_size", &self.video_size)
            .field("has_texture", &self.texture.is_some())
            .finish_non_exhaustive()
    }
}

impl Window {
    #[must_use]
    pub fn new(app: App, editor: Editor) -> Self {
        Self {
            app,
            editor,
            gui: Gui::new(1280, 720),
            texture: None,
            uploaded: None,
            video_size: Resolution {
                width: 0,
                height: 0,
            },
            thumbnails: egui_shell::ThumbnailTextures::default(),
            planar: false,
            closing: false,
        }
    }

    /// Open the window and run until the editor quits.
    pub fn run(self) -> Result<(), eframe::Error> {
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title(TITLE)
                .with_inner_size([1280.0, 720.0])
                .with_min_inner_size([480.0, 320.0]),
            ..Default::default()
        };
        eframe::run_native(
            TITLE,
            options,
            Box::new(move |cc| {
                let mut window = self;
                // Planar preview needs a device to convert on. Asked for
                // once, here, because a window that cannot do it must stay
                // on the composited texture rather than show nothing.
                window.planar = crate::planar_video::install(cc.wgpu_render_state.as_ref());
                window.editor.set_planar_preview(window.planar);
                Ok(Box::new(window))
            }),
        )
    }

    /// Keep the presenter's surface the size of the video pane, so
    /// `davimci-present` letterboxes into exactly the space we will draw.
    fn sync_video_surface(&mut self, width: u32, height: u32) {
        let want = Resolution { width, height };
        if want != self.video_size && width > 0 && height > 0 {
            self.editor.presenter_mut().resize(want);
            self.video_size = want;
            // The frame on screen was composed for the old surface, so it is
            // now the wrong size; recompose it before it is drawn.
            self.editor.refresh_preview(self.app.session());
        }
    }

    /// Upload the composited frame, reusing the texture when the size is
    /// unchanged - a fresh allocation every frame would churn VRAM at 60fps.
    fn upload_video(&mut self, ctx: &egui::Context) {
        let Some(p) = self.editor.presentation() else {
            return;
        };
        // A planar frame is uploaded by the render callback, in its own
        // three single-channel textures. There is no composited buffer to
        // put on the card, and there must not be one.
        if p.video.is_some() {
            return;
        }
        if p.pixels.is_empty() || self.uploaded == Some(p.pixels_id) {
            return;
        }
        let id = p.pixels_id;
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [p.surface.width as usize, p.surface.height as usize],
            &p.pixels,
        );
        match &mut self.texture {
            Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
            None => {
                self.texture =
                    Some(ctx.load_texture("davimci-video", image, egui::TextureOptions::LINEAR));
            }
        }
        self.uploaded = Some(id);
    }
}

impl eframe::App for Window {
    /// Everything that is not painting. eframe calls this before `ui`, and
    /// forbids drawing here - which suits the split this codebase already
    /// has: decisions first, pixels second.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let screen = ctx.input(egui::InputState::viewport_rect);
        self.gui.push(GuiEvent::Resized {
            width: screen.width() as u32,
            height: screen.height() as u32,
        });

        // Input: egui events in, davimci key tokens out. The `Gui` routes
        // modals; the grammar never sees a keystroke a modal owns.
        let events = ctx.input(|i| i.events.clone());
        for (raw, mods) in egui_shell::translate_events(&events, self.gui.takes_text()) {
            self.gui.push(GuiEvent::Key(raw, mods));
        }
        // A press, not a release: clicking the timeline seeks there
        // immediately, the way scrubbing feels in every editor.
        if let Some(pos) = ctx.input(|i| {
            i.pointer
                .press_origin()
                .filter(|_| i.pointer.primary_pressed())
        }) {
            self.gui.push(GuiEvent::Click {
                x: (pos.x - screen.min.x) as i32,
                y: (pos.y - screen.min.y) as i32,
            });
        }
        if ctx.input(|i| i.viewport().close_requested()) {
            self.gui.push(GuiEvent::CloseRequested);
        }

        // One frame of input is one batch: a held key repeats faster than a
        // frame can be decoded, so the whole burst costs a single seek.
        let events = self.gui.poll();
        for response in self.app.drain(events, &mut self.editor) {
            // `i`/`a`/`r` ask for a picker; the frontend is what has one.
            self.gui.apply_response(&response);
        }
        // One presentation tick per frame: playback, shuttle, pacing.
        self.app.event(Event::Tick, &mut self.editor);

        // The editor may have swapped the timeline under us (`:e`, `:bn`).
        if let Some(session) = self.editor.take_session_swap() {
            self.app.replace_session(session);
        }
        for notice in self.editor.take_notices() {
            self.app.notify(notice);
        }

        // Read every frame, so `:set previewheight` lands on the next one.
        self.gui
            .set_preview_height(video_height(self.editor.preview_height()));
        let layout = self.gui.layout();
        self.sync_video_surface(layout.video.width, layout.video.height);
        self.upload_video(ctx);

        // Tell the painter where the video landed, then draw.
        let quad = self.editor.presentation().map(|p| VideoQuad {
            x: p.quad.x,
            y: p.quad.y,
            width: p.quad.width,
            height: p.quad.height,
            timecode: None,
        });
        self.gui.set_chrome(Chrome {
            video: quad,
            command_cursor: 0,
            // The shell owns the picker and folds it in when it paints.
            picker: None,
            // Read every frame, so `:set numbers` lands on the next one.
            numbers: self.editor.numbers(),
        });

        let view = self.app.view();
        if let Err(e) = self.gui.render(&view) {
            self.app.notify(davimci_app::Message::error(e.to_string()));
        }
        // Textures for clips that are no longer drawn are pixels on the GPU
        // nobody will ever look at again.
        if let Some(list) = self.gui.last_draw() {
            self.thumbnails.retain(list);
        }

        if self.app.wants_quit() || self.editor.wants_quit() {
            // Cancel first, close second: the editor is dropped after the
            // event loop returns, and a drop that joins a running transcode
            // is a window that stays on screen after the user closed it.
            if !self.closing {
                self.closing = true;
                self.editor.shutdown();
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        // Playback and shuttle advance off the clock, so the window must
        // keep repainting even with no input.
        ctx.request_repaint();
    }

    /// Painting only. Every rectangle here was decided by
    /// `davimci_gui::layout::paint`.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let screen = ui.max_rect();
        egui_shell::claim_input(ui, screen);
        if let Some(list) = self.gui.last_draw() {
            egui_shell::draw(list, ui, screen.min, &mut self.thumbnails);
        }
        let layout = self.gui.layout();
        // The texture is the whole composited surface - letterbox bars
        // included - so it is drawn over the whole video pane. Drawing it
        // into the quad would letterbox a second time and squash the
        // picture into the middle of its own bars.
        // Planar first: when the presenter handed out a decoder frame, the
        // picture is drawn into the quad it letterboxed rather than over the
        // whole pane, because the shader draws the video and nothing else.
        if let Some(p) = self.editor.presentation()
            && let Some(video) = p.video.clone()
            && layout.video.height > 0
        {
            let quad = egui::Rect::from_min_size(
                screen.min
                    + egui::Vec2::new(
                        layout.video.x as f32 + p.quad.x as f32,
                        layout.video.y as f32 + p.quad.y as f32,
                    ),
                egui::Vec2::new(p.quad.width as f32, p.quad.height as f32),
            );
            crate::planar_video::draw(ui, quad, &video);
        } else if let (Some(tex), Some(p)) = (&self.texture, self.editor.presentation())
            && layout.video.height > 0
        {
            let rect = egui::Rect::from_min_size(
                screen.min + egui::Vec2::new(layout.video.x as f32, layout.video.y as f32),
                egui::Vec2::new(p.surface.width as f32, p.surface.height as f32),
            );
            egui_shell::draw_video(ui, rect, tex);
        }
        // Modals go over the video, or the picker is painted and then
        // covered by the picture.
        if let Some(list) = self.gui.last_draw() {
            egui_shell::draw_modal(list, ui, screen.min, &mut self.thumbnails);
        }
    }
}

/// The surface a fresh window reports before its first resize.
#[must_use]
pub fn initial_surface() -> Surface {
    Surface {
        columns: 1200,
        rows: 6,
        thumbnail_columns: 0,
        cell_columns: 150,
        cell_rows: 12,
    }
}
