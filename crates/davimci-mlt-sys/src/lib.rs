//! Raw FFI declarations for `libmlt` (MLT 7).
//!
//! Hand-written rather than bindgen-generated: davimci uses a small, stable
//! slice of the MLT C API, and a hand-written surface is reviewable, has no
//! build-time codegen dependency, and documents exactly which functions the
//! wrapper is allowed to touch.
//!
//! Everything here is `unsafe` and unowned. The refcount and lifetime rules
//! live one layer up in `davimci-mlt`; nothing else in the workspace may depend
//! on this crate.
//!
//! `mlt_position` is `int32_t` unless libmlt was built with
//! `DOUBLE_MLT_POSITION`, which no distribution build enables. The build
//! script pins the major version so a silently different ABI cannot be linked.

#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_double, c_int, c_void};

pub type mlt_position = i32;

macro_rules! opaque {
    ($($name:ident),* $(,)?) => {$(
        #[repr(C)]
        #[derive(Debug)]
        pub struct $name {
            _private: [u8; 0],
        }
    )*};
}

opaque!(
    mlt_repository_s,
    mlt_properties_s,
    mlt_service_s,
    mlt_producer_s,
    mlt_playlist_s,
    mlt_tractor_s,
    mlt_multitrack_s,
    mlt_field_s,
    mlt_transition_s,
    mlt_filter_s,
    mlt_consumer_s,
    mlt_frame_s,
    mlt_event_s,
);

pub type mlt_repository = *mut mlt_repository_s;
pub type mlt_properties = *mut mlt_properties_s;
pub type mlt_service = *mut mlt_service_s;
pub type mlt_producer = *mut mlt_producer_s;
pub type mlt_playlist = *mut mlt_playlist_s;
pub type mlt_tractor = *mut mlt_tractor_s;
pub type mlt_multitrack = *mut mlt_multitrack_s;
pub type mlt_field = *mut mlt_field_s;
pub type mlt_transition = *mut mlt_transition_s;
pub type mlt_filter = *mut mlt_filter_s;
pub type mlt_consumer = *mut mlt_consumer_s;
pub type mlt_frame = *mut mlt_frame_s;
pub type mlt_event = *mut mlt_event_s;

/// `mlt_event_data` from `framework/mlt_events.h`: a one-word union passed by
/// value. Only the frame case is used, via `mlt_event_data_to_frame`.
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(missing_debug_implementations)] // a union has nothing safe to print
pub union mlt_event_data_u {
    pub i: c_int,
    pub p: *mut c_void,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct mlt_event_data {
    pub u: mlt_event_data_u,
}

impl std::fmt::Debug for mlt_event_data {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("mlt_event_data")
    }
}

pub type mlt_listener = Option<
    unsafe extern "C" fn(owner: mlt_properties, listener_data: *mut c_void, data: mlt_event_data),
>;

/// `mlt_profile_s` from `framework/mlt_profile.h`. Fields are public in MLT
/// and set directly; there is no setter API.
#[repr(C)]
#[derive(Debug)]
pub struct mlt_profile_s {
    pub description: *mut c_char,
    pub frame_rate_num: c_int,
    pub frame_rate_den: c_int,
    pub width: c_int,
    pub height: c_int,
    pub progressive: c_int,
    pub sample_aspect_num: c_int,
    pub sample_aspect_den: c_int,
    pub display_aspect_num: c_int,
    pub display_aspect_den: c_int,
    pub colorspace: c_int,
    pub is_explicit: c_int,
}

pub type mlt_profile = *mut mlt_profile_s;

/// `mlt_image_rgba` from `mlt_types.h`, third in the enum. RGBA is the
/// presenter's format, so this is the only one davimci ever asks for.
pub const MLT_IMAGE_RGBA: c_int = 2;

/// `mlt_image_yuv420p` from `mlt_types.h`, fifth in the enum. Planar 8-bit
/// YUV 4:2:0: the format a GPU host uploads as three textures and converts
/// in a shader.
pub const MLT_IMAGE_YUV420P: c_int = 4;

unsafe extern "C" {
    // -- factory ---------------------------------------------------------
    pub fn mlt_factory_init(directory: *const c_char) -> mlt_repository;
    pub fn mlt_factory_close();
    pub fn mlt_factory_producer(
        profile: mlt_profile,
        service: *const c_char,
        resource: *const c_char,
    ) -> mlt_producer;
    pub fn mlt_factory_consumer(
        profile: mlt_profile,
        service: *const c_char,
        resource: *const c_char,
    ) -> mlt_consumer;
    pub fn mlt_factory_transition(
        profile: mlt_profile,
        service: *const c_char,
        resource: *const c_char,
    ) -> mlt_transition;
    pub fn mlt_factory_filter(
        profile: mlt_profile,
        service: *const c_char,
        resource: *const c_char,
    ) -> mlt_filter;

    // -- profile ---------------------------------------------------------
    pub fn mlt_profile_init(name: *const c_char) -> mlt_profile;
    pub fn mlt_profile_close(profile: mlt_profile);

    // -- properties ------------------------------------------------------
    pub fn mlt_properties_set(
        self_: mlt_properties,
        name: *const c_char,
        value: *const c_char,
    ) -> c_int;
    pub fn mlt_properties_get(self_: mlt_properties, name: *const c_char) -> *mut c_char;
    pub fn mlt_properties_set_int(self_: mlt_properties, name: *const c_char, v: c_int) -> c_int;
    pub fn mlt_properties_get_int(self_: mlt_properties, name: *const c_char) -> c_int;
    pub fn mlt_properties_set_double(
        self_: mlt_properties,
        name: *const c_char,
        v: c_double,
    ) -> c_int;
    pub fn mlt_properties_get_double(self_: mlt_properties, name: *const c_char) -> c_double;
    pub fn mlt_properties_set_position(
        self_: mlt_properties,
        name: *const c_char,
        v: mlt_position,
    ) -> c_int;
    pub fn mlt_properties_inc_ref(self_: mlt_properties) -> c_int;
    pub fn mlt_properties_ref_count(self_: mlt_properties) -> c_int;

    // -- service ---------------------------------------------------------
    pub fn mlt_service_properties(self_: mlt_service) -> mlt_properties;
    pub fn mlt_service_get_frame(self_: mlt_service, frame: *mut mlt_frame, index: c_int) -> c_int;
    pub fn mlt_service_attach(self_: mlt_service, filter: mlt_filter) -> c_int;

    // -- producer --------------------------------------------------------
    pub fn mlt_producer_service(self_: mlt_producer) -> mlt_service;
    pub fn mlt_producer_properties(self_: mlt_producer) -> mlt_properties;
    pub fn mlt_producer_seek(self_: mlt_producer, position: mlt_position) -> c_int;
    pub fn mlt_producer_position(self_: mlt_producer) -> mlt_position;
    pub fn mlt_producer_set_in_and_out(
        self_: mlt_producer,
        r#in: mlt_position,
        out: mlt_position,
    ) -> c_int;
    pub fn mlt_producer_get_in(self_: mlt_producer) -> mlt_position;
    pub fn mlt_producer_get_out(self_: mlt_producer) -> mlt_position;
    pub fn mlt_producer_get_length(self_: mlt_producer) -> mlt_position;
    pub fn mlt_producer_get_playtime(self_: mlt_producer) -> mlt_position;
    pub fn mlt_producer_set_speed(self_: mlt_producer, speed: c_double) -> c_int;
    pub fn mlt_producer_get_speed(self_: mlt_producer) -> c_double;
    pub fn mlt_producer_close(self_: mlt_producer);

    // -- playlist --------------------------------------------------------
    pub fn mlt_playlist_new(profile: mlt_profile) -> mlt_playlist;
    pub fn mlt_playlist_producer(self_: mlt_playlist) -> mlt_producer;
    pub fn mlt_playlist_properties(self_: mlt_playlist) -> mlt_properties;
    pub fn mlt_playlist_count(self_: mlt_playlist) -> c_int;
    pub fn mlt_playlist_clear(self_: mlt_playlist) -> c_int;
    pub fn mlt_playlist_append_io(
        self_: mlt_playlist,
        producer: mlt_producer,
        r#in: mlt_position,
        out: mlt_position,
    ) -> c_int;
    pub fn mlt_playlist_blank(self_: mlt_playlist, out: mlt_position) -> c_int;
    pub fn mlt_playlist_insert(
        self_: mlt_playlist,
        producer: mlt_producer,
        where_: c_int,
        r#in: mlt_position,
        out: mlt_position,
    ) -> c_int;
    pub fn mlt_playlist_insert_blank(self_: mlt_playlist, clip: c_int, out: c_int);
    pub fn mlt_playlist_remove(self_: mlt_playlist, where_: c_int) -> c_int;
    pub fn mlt_playlist_resize_clip(
        self_: mlt_playlist,
        clip: c_int,
        r#in: mlt_position,
        out: mlt_position,
    ) -> c_int;
    pub fn mlt_playlist_close(self_: mlt_playlist);

    // -- tractor / multitrack --------------------------------------------
    pub fn mlt_tractor_new() -> mlt_tractor;
    pub fn mlt_tractor_producer(self_: mlt_tractor) -> mlt_producer;
    pub fn mlt_tractor_properties(self_: mlt_tractor) -> mlt_properties;
    pub fn mlt_tractor_field(self_: mlt_tractor) -> mlt_field;
    pub fn mlt_tractor_multitrack(self_: mlt_tractor) -> mlt_multitrack;
    pub fn mlt_tractor_set_track(self_: mlt_tractor, producer: mlt_producer, index: c_int)
    -> c_int;
    pub fn mlt_tractor_get_track(self_: mlt_tractor, index: c_int) -> mlt_producer;
    pub fn mlt_tractor_refresh(self_: mlt_tractor);
    pub fn mlt_tractor_close(self_: mlt_tractor);
    pub fn mlt_multitrack_count(self_: mlt_multitrack) -> c_int;

    // -- field / transition ----------------------------------------------
    pub fn mlt_field_plant_transition(
        self_: mlt_field,
        transition: mlt_transition,
        a_track: c_int,
        b_track: c_int,
    ) -> c_int;
    pub fn mlt_transition_properties(self_: mlt_transition) -> mlt_properties;
    pub fn mlt_transition_close(self_: mlt_transition);
    pub fn mlt_filter_properties(self_: mlt_filter) -> mlt_properties;
    pub fn mlt_filter_close(self_: mlt_filter);

    // -- consumer --------------------------------------------------------
    pub fn mlt_consumer_properties(self_: mlt_consumer) -> mlt_properties;
    pub fn mlt_consumer_connect(self_: mlt_consumer, producer: mlt_service) -> c_int;
    pub fn mlt_consumer_start(self_: mlt_consumer) -> c_int;
    pub fn mlt_consumer_stop(self_: mlt_consumer) -> c_int;
    pub fn mlt_consumer_is_stopped(self_: mlt_consumer) -> c_int;
    pub fn mlt_consumer_position(self_: mlt_consumer) -> mlt_position;
    pub fn mlt_consumer_purge(self_: mlt_consumer);
    pub fn mlt_consumer_close(self_: mlt_consumer);

    // -- frame -----------------------------------------------------------
    pub fn mlt_frame_get_position(self_: mlt_frame) -> mlt_position;
    pub fn mlt_frame_properties(self_: mlt_frame) -> mlt_properties;
    pub fn mlt_frame_get_image(
        self_: mlt_frame,
        buffer: *mut *mut u8,
        format: *mut c_int,
        width: *mut c_int,
        height: *mut c_int,
        writable: c_int,
    ) -> c_int;
    pub fn mlt_frame_close(self_: mlt_frame);

    // -- events ----------------------------------------------------------
    pub fn mlt_events_listen(
        self_: mlt_properties,
        listener_data: *mut c_void,
        id: *const c_char,
        listener: mlt_listener,
    ) -> mlt_event;
    pub fn mlt_event_inc_ref(self_: mlt_event);
    pub fn mlt_event_close(self_: mlt_event);
    pub fn mlt_event_data_to_frame(data: mlt_event_data) -> mlt_frame;

    // -- misc ------------------------------------------------------------
    pub fn mlt_pool_release(release: *mut c_void);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// The struct layout is hand-transcribed from `mlt_profile.h`, so pin the
    /// field offsets: a wrong one would silently write the frame rate into
    /// the width. The trailing padding after eleven `int`s is why this checks
    /// offsets rather than the total size.
    #[test]
    fn profile_layout_matches_the_c_struct() {
        use std::mem::offset_of;
        let ptr = size_of::<*mut c_char>();
        let int = size_of::<c_int>();
        assert_eq!(offset_of!(mlt_profile_s, description), 0);
        assert_eq!(offset_of!(mlt_profile_s, frame_rate_num), ptr);
        assert_eq!(offset_of!(mlt_profile_s, frame_rate_den), ptr + int);
        assert_eq!(offset_of!(mlt_profile_s, width), ptr + 2 * int);
        assert_eq!(offset_of!(mlt_profile_s, height), ptr + 3 * int);
        assert_eq!(offset_of!(mlt_profile_s, is_explicit), ptr + 10 * int);
    }

    /// Links against the real library, proving the symbols resolve.
    #[test]
    fn profile_init_and_close_links() {
        unsafe {
            let p = mlt_profile_init(std::ptr::null());
            assert!(!p.is_null());
            assert!((*p).frame_rate_num > 0);
            mlt_profile_close(p);
        }
    }
}
