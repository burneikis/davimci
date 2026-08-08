//! The `egui` rasteriser.
//!
//! This module is the *only* place in davimci that knows what a colour is or
//! what a font is. It places pixels for decisions made elsewhere: the
//! [`DrawList`] it draws was computed by [`crate::layout::paint`] from a
//! `ViewState`, and the video image was composited by `davimci-present`.
//! Nothing here may consult a `Timeline`, choose a layout, or interpret a
//! key beyond naming its token.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "egui measures in f32 pixels; window-sized values are far below the mantissa"
)]

use egui::{
    Align2, Color32, CornerRadius, FontId, Pos2, Rect as EguiRect, Sense, Stroke, StrokeKind, Ui,
    Vec2,
};

use crate::input::{Modifiers, RawKey};
use crate::layout::TEXT_PADDING;
use crate::paint::{DrawList, Fill, Paint, Rect, TextRole};
use davimci_app::Severity;

/// Colours for every [`Fill`]. One function, so a theme is one edit.
#[must_use]
#[allow(
    clippy::match_same_arms,
    reason = "a theme table lists one arm per variant so a colour can be changed alone"
)]
pub fn fill_color(fill: Fill) -> Color32 {
    match fill {
        Fill::Background => Color32::from_rgb(24, 24, 28),
        Fill::Ruler => Color32::from_rgb(34, 34, 40),
        Fill::TrackLane => Color32::from_rgb(30, 30, 36),
        Fill::TrackLaneFocused => Color32::from_rgb(40, 40, 52),
        Fill::TrackHeader => Color32::from_rgb(44, 44, 54),
        Fill::TrackHeaderFocused => Color32::from_rgb(62, 62, 78),
        Fill::Clip => Color32::from_rgb(70, 110, 160),
        Fill::ClipSelected => Color32::from_rgb(120, 170, 230),
        Fill::ClipOffline => Color32::from_rgb(150, 60, 60),
        Fill::ClipGrouped => Color32::from_rgb(56, 88, 128),
        Fill::Waveform => Color32::from_rgb(150, 200, 180),
        Fill::Selection => Color32::from_rgba_unmultiplied(120, 170, 230, 60),
        Fill::Playhead => Color32::from_rgb(240, 220, 90),
        // Brighter than the playhead it sits on, because it is the one column
        // of it that says where an edit lands.
        Fill::Cursor => Color32::from_rgb(255, 245, 170),
        Fill::TickMajor => Color32::from_rgb(150, 150, 160),
        Fill::TickMinor => Color32::from_rgb(80, 80, 90),
        Fill::StatusLine => Color32::from_rgb(18, 18, 22),
        Fill::CommandLine => Color32::from_rgb(28, 28, 34),
        Fill::Caret => Color32::from_rgb(240, 240, 250),
        Fill::Video => Color32::BLACK,
        // The picker sits over the timeline, so it is opaque rather than
        // tinted: a half-transparent file list is unreadable over clips.
        Fill::ModalBackground => Color32::from_rgb(38, 38, 46),
        Fill::ModalSelected => Color32::from_rgb(70, 110, 160),
    }
}

/// Text colour and size for every [`TextRole`].
#[must_use]
#[allow(
    clippy::match_same_arms,
    reason = "a theme table lists one arm per variant so a colour can be changed alone"
)]
pub fn text_style(role: TextRole) -> (Color32, f32) {
    match role {
        TextRole::TrackName => (Color32::from_rgb(200, 200, 210), 12.0),
        TextRole::ClipLabel => (Color32::from_rgb(235, 235, 245), 11.0),
        TextRole::Status => (Color32::from_rgb(210, 210, 220), 13.0),
        TextRole::Command => (Color32::from_rgb(240, 240, 250), 13.0),
        TextRole::Completion => (Color32::from_rgb(150, 190, 240), 12.0),
        TextRole::Timecode => (Color32::from_rgb(240, 220, 90), 12.0),
        TextRole::RulerNumber => (Color32::from_rgb(120, 120, 135), 10.0),
        TextRole::Message(Severity::Info) => (Color32::from_rgb(180, 210, 180), 13.0),
        TextRole::Message(Severity::Warning) => (Color32::from_rgb(230, 200, 120), 13.0),
        TextRole::Message(Severity::Error) => (Color32::from_rgb(240, 140, 140), 13.0),
        TextRole::ModalTitle => (Color32::from_rgb(240, 220, 90), 14.0),
        TextRole::ModalQuery => (Color32::from_rgb(240, 240, 250), 13.0),
        TextRole::ModalEntry => (Color32::from_rgb(210, 210, 220), 13.0),
        TextRole::ModalEntryDir => (Color32::from_rgb(150, 190, 240), 13.0),
        TextRole::ModalEntrySelected => (Color32::from_rgb(255, 255, 255), 13.0),
    }
}

fn to_egui(rect: Rect) -> EguiRect {
    EguiRect::from_min_size(
        Pos2::new(rect.x as f32, rect.y as f32),
        Vec2::new(rect.width as f32, rect.height as f32),
    )
}

/// One uploaded texture per clip thumbnail.
///
/// A draw list is rebuilt every frame, so uploading its thumbnails every
/// frame would spend the GPU on pictures that did not change. The cache is
/// keyed by clip *and* source frame, because a clip's filmstrip is several
/// different pictures of it.
#[derive(Default)]
pub struct ThumbnailTextures {
    textures:
        std::collections::HashMap<(davimci_core::ClipId, davimci_core::Frame), egui::TextureHandle>,
}

impl std::fmt::Debug for ThumbnailTextures {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThumbnailTextures")
            .field("len", &self.textures.len())
            .finish()
    }
}

impl ThumbnailTextures {
    fn texture(
        &mut self,
        ctx: &egui::Context,
        clip: davimci_core::ClipId,
        thumb: &davimci_app::Thumbnail,
    ) -> egui::TextureHandle {
        if let Some(tex) = self.textures.get(&(clip, thumb.source)) {
            return tex.clone();
        }
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [thumb.width as usize, thumb.height as usize],
            &thumb.rgba,
        );
        let tex = ctx.load_texture(
            format!("davimci-thumb-{}-{}", clip.get(), thumb.source.get()),
            image,
            egui::TextureOptions::LINEAR,
        );
        self.textures.insert((clip, thumb.source), tex.clone());
        tex
    }

    /// Forget textures for pictures the list no longer draws.
    pub fn retain(&mut self, list: &DrawList) {
        let live: Vec<(davimci_core::ClipId, davimci_core::Frame)> = list
            .images()
            .into_iter()
            .map(|(_, clip, thumb)| (clip, thumb.source))
            .collect();
        self.textures.retain(|key, _| live.contains(key));
    }
}

/// Draw a whole [`DrawList`] into `ui`, offset by the panel's origin.
///
/// `Fill::Video` is skipped: the video pane is a texture, drawn by
/// [`draw_video`], and painting a black rectangle over it would hide it.
pub fn draw(list: &DrawList, ui: &Ui, origin: Pos2, thumbs: &mut ThumbnailTextures) {
    draw_ops(list, ui, origin, false, thumbs);
}

/// Draw only the modal overlay. The shell calls this *after* the video
/// texture, so a picker is never hidden behind the picture.
pub fn draw_modal(list: &DrawList, ui: &Ui, origin: Pos2) {
    draw_ops(list, ui, origin, true, &mut ThumbnailTextures::default());
}

fn draw_ops(list: &DrawList, ui: &Ui, origin: Pos2, modal: bool, thumbs: &mut ThumbnailTextures) {
    let painter = ui.painter();
    for op in list.ops().iter().filter(|op| op.is_modal() == modal) {
        match op {
            Paint::Rect {
                fill: Fill::Video, ..
            } => {}
            Paint::Rect { rect, fill } => {
                painter.rect_filled(
                    to_egui(*rect).translate(origin.to_vec2()),
                    CornerRadius::ZERO,
                    fill_color(*fill),
                );
            }
            Paint::Image {
                rect,
                clip,
                image,
                tile,
            } => {
                let tex = thumbs.texture(ui.ctx(), *clip, image);
                let r = to_egui(*rect).translate(origin.to_vec2());
                // A tile cut off at the clip's edge shows less of the
                // picture; it is never squashed into the space that is left.
                let u = (rect.width as f32 / (*tile).max(1) as f32).clamp(0.0, 1.0);
                painter.with_clip_rect(r).image(
                    tex.id(),
                    r,
                    EguiRect::from_min_max(Pos2::ZERO, Pos2::new(u, 1.0)),
                    Color32::WHITE,
                );
            }
            Paint::Text { rect, role, text } => {
                let (color, size) = text_style(*role);
                let r = to_egui(*rect).translate(origin.to_vec2());
                // Clip labels and track names are drawn inside their own
                // rectangle; status and command lines are left-aligned in
                // theirs. Either way the text never escapes its box.
                painter.with_clip_rect(r).text(
                    r.left_center()
                        + Vec2::new(f32::from(u16::try_from(TEXT_PADDING).unwrap_or(4)), 0.0),
                    Align2::LEFT_CENTER,
                    text,
                    FontId::monospace(size),
                    color,
                );
            }
        }
    }
}

/// Draw the composited video image into `rect`.
pub fn draw_video(ui: &Ui, rect: EguiRect, texture: &egui::TextureHandle) {
    ui.painter().image(
        texture.id(),
        rect,
        EguiRect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
        Color32::WHITE,
    );
}

/// Outline the focused region, so the window shows where keys will go.
pub fn draw_focus_hint(ui: &Ui, rect: EguiRect) {
    ui.painter().rect_stroke(
        rect,
        CornerRadius::ZERO,
        Stroke::new(1.0, Color32::from_rgb(70, 70, 90)),
        StrokeKind::Inside,
    );
}

/// Claim the whole panel so egui does not steal keystrokes for a widget.
pub fn claim_input(ui: &mut Ui, rect: EguiRect) {
    let _ = ui.allocate_rect(rect, Sense::click_and_drag());
}

/// Translate one frame of `egui` input into davimci key presses.
///
/// Printable characters arrive as `Event::Text`, already shifted by the
/// platform layout, so they are taken from there; named keys and Control
/// chords arrive as `Event::Key`, because `Text` is not emitted for either.
/// Whitespace text is dropped so `Space` is not counted twice.
///
/// `taking_text` says whether a modal is spelling out a line, which is the
/// only place a paste means anything: the timeline grammar has no paste of
/// its own, and `p` reads the yank register rather than the system
/// clipboard.
#[must_use]
pub fn translate_events(events: &[egui::Event], taking_text: bool) -> Vec<(RawKey, Modifiers)> {
    let mut out = Vec::new();
    for event in events {
        match event {
            egui::Event::Paste(text) if taking_text => {
                for c in text.chars().filter(|c| !c.is_whitespace()) {
                    out.push((RawKey::Char(c), Modifiers::default()));
                }
            }
            egui::Event::Text(text) => {
                for c in text.chars().filter(|c| !c.is_whitespace()) {
                    out.push((RawKey::Char(c), Modifiers::default()));
                }
            }
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => {
                let mods = Modifiers {
                    ctrl: modifiers.ctrl,
                    alt: modifiers.alt,
                    shift: modifiers.shift,
                    logo: modifiers.command && !modifiers.ctrl,
                };
                if mods.ctrl {
                    if let Some(c) = key.name().chars().next().filter(char::is_ascii_alphabetic) {
                        out.push((RawKey::Char(c.to_ascii_lowercase()), mods));
                    }
                    continue;
                }
                let raw = match key {
                    egui::Key::Escape => RawKey::Escape,
                    egui::Key::Enter => RawKey::Enter,
                    egui::Key::Backspace => RawKey::Backspace,
                    egui::Key::Tab => RawKey::Tab,
                    egui::Key::ArrowLeft => RawKey::Left,
                    egui::Key::ArrowRight => RawKey::Right,
                    egui::Key::ArrowUp => RawKey::Up,
                    egui::Key::ArrowDown => RawKey::Down,
                    egui::Key::Space => RawKey::Space,
                    _ => continue,
                };
                out.push((raw, mods));
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_events_become_characters_and_whitespace_is_dropped() {
        let events = vec![
            egui::Event::Text("d".into()),
            egui::Event::Text(" ".into()),
            egui::Event::Text("w".into()),
        ];
        let keys = translate_events(&events, false);
        assert_eq!(
            keys.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>(),
            vec![RawKey::Char('d'), RawKey::Char('w')]
        );
    }

    #[test]
    fn space_arrives_once_from_the_key_event() {
        let events = vec![
            egui::Event::Text(" ".into()),
            egui::Event::Key {
                key: egui::Key::Space,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
        ];
        let keys = translate_events(&events, false);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].0, RawKey::Space);
    }

    #[test]
    fn ctrl_chords_become_ctrl_keys() {
        let events = vec![egui::Event::Key {
            key: egui::Key::R,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::CTRL,
        }];
        let keys = translate_events(&events, false);
        assert_eq!(keys[0].0, RawKey::Char('r'));
        assert!(keys[0].1.ctrl);
    }

    /// A paste with no line being typed has nowhere to go, and must not be
    /// mistaken for the keys that spell its contents.
    #[test]
    fn a_paste_outside_a_text_modal_is_dropped() {
        assert!(translate_events(&[egui::Event::Paste("clip.mp4".into())], false).is_empty());
    }

    #[test]
    fn a_paste_into_a_text_modal_is_the_text() {
        let keys = translate_events(&[egui::Event::Paste("a b".into())], true);
        let typed: String = keys
            .iter()
            .filter_map(|(k, _)| match k {
                RawKey::Char(c) => Some(*c),
                _ => None,
            })
            .collect();
        assert_eq!(typed, "ab");
        assert!(keys.iter().all(|(_, m)| !m.ctrl));
    }

    #[test]
    fn key_releases_are_ignored() {
        let events = vec![egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }];
        assert!(translate_events(&events, false).is_empty());
    }

    #[test]
    fn every_fill_and_role_has_a_colour() {
        for fill in [
            Fill::Background,
            Fill::Clip,
            Fill::ClipSelected,
            Fill::ClipGrouped,
            Fill::Playhead,
            Fill::Video,
        ] {
            let _ = fill_color(fill);
        }
        // Grouping shades a clip rather than recolouring it, so the grouped
        // fill has to stay a darker version of the plain one.
        let (plain, grouped) = (fill_color(Fill::Clip), fill_color(Fill::ClipGrouped));
        assert_ne!(plain, grouped);
        // Darker on every channel, but not so dark it reads as a hole in the
        // lane: between half and four-fifths of the plain clip's brightness.
        for (g, p) in [
            (grouped.r(), plain.r()),
            (grouped.g(), plain.g()),
            (grouped.b(), plain.b()),
        ] {
            let (g, p) = (u32::from(g), u32::from(p));
            assert!(g * 10 <= p * 9, "grouping must read as darker");
            assert!(g * 2 >= p, "grouping must not read as a hole");
        }

        for role in [
            TextRole::Status,
            TextRole::ClipLabel,
            TextRole::Message(Severity::Error),
        ] {
            assert!(text_style(role).1 > 0.0);
        }
    }
}
