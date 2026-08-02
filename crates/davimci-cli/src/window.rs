//! The window (plan.md Phase 9c's shell).
//!
//! The eframe application lives in the binary because it is the one place
//! that holds a frontend *and* a render backend at once, and no frontend may
//! reference MLT (spec §10.1). It does four things per frame: hand egui's
//! input to the `Gui` frontend, pump the resulting events through the `App`,
//! run one presentation tick, and draw what `davimci-gui` computed.
//!
//! It decides nothing. Layout, painting, key meaning, and what the status
//! line says were all settled before this file runs.

use davimci_app::{App, Event, Frontend, Host, Surface};
use davimci_core::Resolution;
use davimci_gui::egui_shell;
use davimci_gui::{Chrome, Gui, GuiEvent, VideoQuad};

use crate::editor::Editor;

/// Window title, kept in one place so `:e` can extend it later.
const TITLE: &str = "davimci";

/// The running editor plus its window state.
pub struct Window {
    app: App,
    editor: Editor,
    gui: Gui,
    texture: Option<egui::TextureHandle>,
    /// Size of the video pane last frame, so the presenter is resized only
    /// when it actually changed.
    video_size: Resolution,
    /// Where the video landed inside the pane, decided in `logic` and drawn
    /// in `ui`.
    quad: Option<VideoQuad>,
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
            video_size: Resolution {
                width: 0,
                height: 0,
            },
            quad: None,
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
        eframe::run_native(TITLE, options, Box::new(move |_cc| Ok(Box::new(self))))
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
        if p.pixels.is_empty() {
            return;
        }
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
    }
}

impl eframe::App for Window {
    /// Everything that is not painting. eframe calls this before `ui`, and
    /// forbids drawing here - which suits the split this codebase already
    /// has: decisions first, pixels second.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let screen = ctx.input(|i| i.viewport_rect());
        self.gui.push(GuiEvent::Resized {
            width: screen.width() as u32,
            height: screen.height() as u32,
        });

        // Input: egui events in, davimci key tokens out. The `Gui` routes
        // modals; the grammar never sees a keystroke a modal owns.
        let events = ctx.input(|i| i.events.clone());
        for (raw, mods) in egui_shell::translate_events(&events) {
            self.gui.push(GuiEvent::Key(raw, mods));
        }
        if ctx.input(|i| i.viewport().close_requested()) {
            self.gui.push(GuiEvent::CloseRequested);
        }

        for event in self.gui.poll() {
            self.app.event(event, &mut self.editor);
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
        });

        self.quad = quad;

        let view = self.app.view();
        if let Err(e) = self.gui.render(&view) {
            self.app.notify(davimci_app::Message::error(e.to_string()));
        }

        if self.app.wants_quit() || self.editor.wants_quit() {
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
            egui_shell::draw(list, ui, screen.min);
        }
        let layout = self.gui.layout();
        if let (Some(tex), Some(q)) = (&self.texture, self.quad) {
            let rect = egui::Rect::from_min_size(
                screen.min
                    + egui::Vec2::new(
                        layout.video.x as f32 + q.x as f32,
                        layout.video.y as f32 + q.y as f32,
                    ),
                egui::Vec2::new(q.width as f32, q.height as f32),
            );
            egui_shell::draw_video(ui, rect, tex);
        }
    }
}

/// The surface a fresh window reports before its first resize.
#[must_use]
pub fn initial_surface() -> Surface {
    Surface {
        columns: 1200,
        rows: 6,
    }
}
