//! Safe, RAII wrappers over the raw MLT handles.
//!
//! MLT is refcounted C with manual `*_close` calls; spec §10.1 accepts that
//! risk and asks for exactly this layer plus a test suite that exercises it.
//! The rules here are:
//!
//! - every wrapper owns exactly one reference and releases it on `Drop`;
//! - `clone_ref` is the *only* way to get a second handle, and it increments
//!   the refcount, so a double free is not expressible;
//! - no wrapper is `Clone`, so a copy cannot happen by accident;
//! - nothing hands out a raw pointer with a lifetime longer than the borrow.
//!
//! Wrappers are `!Send` by construction (they hold raw pointers) because MLT
//! services are not thread-safe without explicit locking.

use std::ffi::{CStr, CString, c_char, c_int};
use std::ptr;
use std::sync::OnceLock;

use davimci_mlt_sys as sys;

use crate::error::MltError;

/// Process-wide MLT initialisation.
///
/// `mlt_factory_init` is global and `mlt_factory_close` tears down modules
/// other live objects may still reference, so davimci initialises once and
/// never closes: process exit is the only safe teardown point.
pub fn init() -> Result<(), MltError> {
    static REPO: OnceLock<bool> = OnceLock::new();
    let ok = *REPO.get_or_init(|| {
        // SAFETY: called once, before any other MLT call, with a null
        // directory so MLT uses its configured module path.
        let repo = unsafe { sys::mlt_factory_init(ptr::null()) };
        !repo.is_null()
    });
    if ok { Ok(()) } else { Err(MltError::Init) }
}

fn cstr(s: &str) -> Result<CString, MltError> {
    CString::new(s).map_err(|_| MltError::BadString {
        value: s.to_string(),
    })
}

/// A borrowed MLT properties bag.
///
/// Borrowed, not owned: properties belong to the service that vended them, so
/// this wrapper never closes anything.
#[derive(Debug)]
pub struct Properties<'a> {
    raw: sys::mlt_properties,
    _owner: std::marker::PhantomData<&'a ()>,
}

impl<'a> Properties<'a> {
    /// # Safety
    /// `raw` must be non-null and outlive `'a`.
    unsafe fn from_raw(raw: sys::mlt_properties) -> Self {
        Self {
            raw,
            _owner: std::marker::PhantomData,
        }
    }

    pub fn set(&mut self, name: &str, value: &str) -> Result<(), MltError> {
        let (n, v) = (cstr(name)?, cstr(value)?);
        // SAFETY: both strings are NUL-terminated and live across the call.
        unsafe { sys::mlt_properties_set(self.raw, n.as_ptr(), v.as_ptr()) };
        Ok(())
    }

    pub fn set_int(&mut self, name: &str, value: i32) -> Result<(), MltError> {
        let n = cstr(name)?;
        // SAFETY: as above.
        unsafe { sys::mlt_properties_set_int(self.raw, n.as_ptr(), value as c_int) };
        Ok(())
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<String> {
        let n = cstr(name).ok()?;
        // SAFETY: MLT returns a pointer owned by the properties bag, valid
        // until the property is overwritten; it is copied immediately.
        let raw = unsafe { sys::mlt_properties_get(self.raw, n.as_ptr()) };
        if raw.is_null() {
            return None;
        }
        Some(
            unsafe { CStr::from_ptr(raw as *const c_char) }
                .to_string_lossy()
                .into_owned(),
        )
    }

    #[must_use]
    pub fn get_int(&self, name: &str) -> i32 {
        let Ok(n) = cstr(name) else { return 0 };
        // SAFETY: NUL-terminated name, valid bag.
        unsafe { sys::mlt_properties_get_int(self.raw, n.as_ptr()) }
    }

    /// MLT's own reference count. Used by the refcount tests.
    #[must_use]
    pub fn ref_count(&self) -> i32 {
        // SAFETY: valid bag.
        unsafe { sys::mlt_properties_ref_count(self.raw) }
    }

    fn inc_ref(&self) {
        // SAFETY: valid bag.
        unsafe { sys::mlt_properties_inc_ref(self.raw) };
    }
}

/// An MLT profile: the project's single framerate and resolution (spec §7.1).
#[derive(Debug)]
pub struct Profile {
    raw: sys::mlt_profile,
}

impl Profile {
    /// A profile with explicit geometry, so MLT never guesses from the first
    /// producer it sees.
    pub fn new(width: u32, height: u32, fps_num: u32, fps_den: u32) -> Result<Self, MltError> {
        init()?;
        // SAFETY: a null name asks MLT for its default profile, which is then
        // overwritten field by field.
        let raw = unsafe { sys::mlt_profile_init(ptr::null()) };
        if raw.is_null() {
            return Err(MltError::Init);
        }
        // SAFETY: `raw` is a valid, uniquely owned profile.
        unsafe {
            (*raw).width = width as c_int;
            (*raw).height = height as c_int;
            (*raw).frame_rate_num = fps_num as c_int;
            (*raw).frame_rate_den = fps_den as c_int;
            (*raw).progressive = 1;
            (*raw).sample_aspect_num = 1;
            (*raw).sample_aspect_den = 1;
            (*raw).display_aspect_num = width as c_int;
            (*raw).display_aspect_den = height as c_int;
            (*raw).colorspace = 709;
            (*raw).is_explicit = 1;
        }
        Ok(Self { raw })
    }

    pub(crate) fn as_raw(&self) -> sys::mlt_profile {
        self.raw
    }

    #[must_use]
    pub fn fps(&self) -> (i32, i32) {
        // SAFETY: valid profile.
        unsafe { ((*self.raw).frame_rate_num, (*self.raw).frame_rate_den) }
    }

    #[must_use]
    pub fn size(&self) -> (i32, i32) {
        // SAFETY: valid profile.
        unsafe { ((*self.raw).width, (*self.raw).height) }
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        // SAFETY: owned, closed exactly once.
        unsafe { sys::mlt_profile_close(self.raw) };
    }
}

/// An MLT producer: media, a generator, a playlist, or a tractor.
#[derive(Debug)]
pub struct Producer {
    raw: sys::mlt_producer,
}

impl Producer {
    /// Build a producer from a service name and a resource.
    pub fn new(profile: &Profile, service: &str, resource: &str) -> Result<Self, MltError> {
        init()?;
        let (s, r) = (cstr(service)?, cstr(resource)?);
        // SAFETY: valid profile, NUL-terminated strings.
        let raw = unsafe { sys::mlt_factory_producer(profile.as_raw(), s.as_ptr(), r.as_ptr()) };
        if raw.is_null() {
            return Err(MltError::NoProducer {
                service: service.into(),
                resource: resource.into(),
            });
        }
        Ok(Self { raw })
    }

    /// # Safety
    /// `raw` must be a producer this wrapper may take one reference of.
    pub(crate) unsafe fn from_raw(raw: sys::mlt_producer) -> Self {
        Self { raw }
    }

    pub(crate) fn as_raw(&self) -> sys::mlt_producer {
        self.raw
    }

    #[must_use]
    pub fn properties(&self) -> Properties<'_> {
        // SAFETY: the bag belongs to this producer and cannot outlive it.
        unsafe { Properties::from_raw(sys::mlt_producer_properties(self.raw)) }
    }

    pub(crate) fn service(&self) -> sys::mlt_service {
        // SAFETY: valid producer.
        unsafe { sys::mlt_producer_service(self.raw) }
    }

    /// Take a second owning handle by incrementing MLT's refcount.
    #[must_use]
    pub fn clone_ref(&self) -> Self {
        self.properties().inc_ref();
        Self { raw: self.raw }
    }

    pub fn set_in_and_out(&mut self, in_: i32, out: i32) {
        // SAFETY: valid producer.
        unsafe { sys::mlt_producer_set_in_and_out(self.raw, in_, out) };
    }

    pub fn seek(&mut self, position: i32) {
        // SAFETY: valid producer.
        unsafe { sys::mlt_producer_seek(self.raw, position) };
    }

    #[must_use]
    pub fn position(&self) -> i32 {
        // SAFETY: valid producer.
        unsafe { sys::mlt_producer_position(self.raw) }
    }

    #[must_use]
    pub fn length(&self) -> i32 {
        // SAFETY: valid producer.
        unsafe { sys::mlt_producer_get_length(self.raw) }
    }

    pub fn set_speed(&mut self, speed: f64) {
        // SAFETY: valid producer.
        unsafe { sys::mlt_producer_set_speed(self.raw, speed) };
    }

    pub fn attach(&mut self, filter: &Filter) -> Result<(), MltError> {
        // SAFETY: both handles are live and owned by the caller.
        let rc = unsafe { sys::mlt_service_attach(self.service(), filter.raw) };
        if rc == 0 {
            Ok(())
        } else {
            Err(MltError::AttachFailed)
        }
    }

    /// Pull one frame from this producer.
    pub fn next_frame(&mut self) -> Result<FrameRef, MltError> {
        let mut raw: sys::mlt_frame = ptr::null_mut();
        // SAFETY: `raw` is written by MLT on success; the frame is then owned
        // by the returned wrapper.
        let rc = unsafe { sys::mlt_service_get_frame(self.service(), &mut raw, 0) };
        if rc != 0 || raw.is_null() {
            return Err(MltError::NoFrame);
        }
        Ok(FrameRef { raw })
    }
}

impl Drop for Producer {
    fn drop(&mut self) {
        // SAFETY: releases exactly the one reference this wrapper owns.
        unsafe { sys::mlt_producer_close(self.raw) };
    }
}

/// An MLT filter, owned until attached.
#[derive(Debug)]
pub struct Filter {
    raw: sys::mlt_filter,
    attached: bool,
}

impl Filter {
    pub fn new(profile: &Profile, service: &str, arg: Option<&str>) -> Result<Self, MltError> {
        init()?;
        let s = cstr(service)?;
        let a = arg.map(cstr).transpose()?;
        let arg_ptr = a.as_ref().map_or(ptr::null(), |c| c.as_ptr());
        // SAFETY: valid profile, NUL-terminated strings, null means "no arg".
        let raw = unsafe { sys::mlt_factory_filter(profile.as_raw(), s.as_ptr(), arg_ptr) };
        if raw.is_null() {
            return Err(MltError::NoFilter {
                service: service.into(),
            });
        }
        Ok(Self {
            raw,
            attached: false,
        })
    }

    #[must_use]
    pub fn properties(&self) -> Properties<'_> {
        // SAFETY: the bag belongs to this filter.
        unsafe { Properties::from_raw(sys::mlt_filter_properties(self.raw)) }
    }

    /// Mark the filter as owned by a service. Attaching takes its own
    /// reference, so the wrapper still releases the one it holds.
    fn mark_attached(&mut self) {
        self.attached = true;
    }
}

impl Drop for Filter {
    fn drop(&mut self) {
        let _ = self.attached;
        // SAFETY: releases this wrapper's single reference.
        unsafe { sys::mlt_filter_close(self.raw) };
    }
}

/// An MLT playlist: one track's worth of entries and blanks.
#[derive(Debug)]
pub struct Playlist {
    raw: sys::mlt_playlist,
}

impl Playlist {
    pub fn new(profile: &Profile) -> Result<Self, MltError> {
        init()?;
        // SAFETY: valid profile.
        let raw = unsafe { sys::mlt_playlist_new(profile.as_raw()) };
        if raw.is_null() {
            return Err(MltError::Init);
        }
        Ok(Self { raw })
    }

    #[must_use]
    pub fn properties(&self) -> Properties<'_> {
        // SAFETY: the bag belongs to this playlist.
        unsafe { Properties::from_raw(sys::mlt_playlist_properties(self.raw)) }
    }

    #[must_use]
    pub fn count(&self) -> i32 {
        // SAFETY: valid playlist.
        unsafe { sys::mlt_playlist_count(self.raw) }
    }

    pub fn clear(&mut self) {
        // SAFETY: valid playlist.
        unsafe { sys::mlt_playlist_clear(self.raw) };
    }

    /// Append a producer with an explicit inclusive in/out range.
    pub fn append(&mut self, producer: &Producer, in_: i32, out: i32) -> Result<(), MltError> {
        // SAFETY: both handles live; MLT takes its own reference on the cut.
        let rc = unsafe { sys::mlt_playlist_append_io(self.raw, producer.as_raw(), in_, out) };
        Self::check(rc)
    }

    pub fn append_blank(&mut self, length: i32) -> Result<(), MltError> {
        // MLT's blank length is an inclusive `out`, so a blank of N frames is
        // N-1. Every off-by-one in a gap starts here.
        // SAFETY: valid playlist.
        let rc = unsafe { sys::mlt_playlist_blank(self.raw, length - 1) };
        Self::check(rc)
    }

    pub fn insert(
        &mut self,
        index: i32,
        producer: &Producer,
        in_: i32,
        out: i32,
    ) -> Result<(), MltError> {
        // SAFETY: both handles live.
        let rc = unsafe { sys::mlt_playlist_insert(self.raw, producer.as_raw(), index, in_, out) };
        Self::check(rc)
    }

    pub fn insert_blank(&mut self, index: i32, length: i32) {
        // SAFETY: valid playlist; inclusive out again.
        unsafe { sys::mlt_playlist_insert_blank(self.raw, index, length - 1) };
    }

    pub fn remove(&mut self, index: i32) -> Result<(), MltError> {
        // SAFETY: valid playlist; MLT bounds-checks the index itself.
        let rc = unsafe { sys::mlt_playlist_remove(self.raw, index) };
        Self::check(rc)
    }

    pub fn resize_clip(&mut self, index: i32, in_: i32, out: i32) -> Result<(), MltError> {
        // SAFETY: valid playlist.
        let rc = unsafe { sys::mlt_playlist_resize_clip(self.raw, index, in_, out) };
        Self::check(rc)
    }

    /// The playlist as a producer, for planting into a tractor.
    #[must_use]
    pub fn as_producer(&self) -> Producer {
        // SAFETY: `mlt_playlist_producer` borrows; the extra reference keeps
        // it alive independently of this wrapper.
        let raw = unsafe { sys::mlt_playlist_producer(self.raw) };
        let p = unsafe { Producer::from_raw(raw) };
        p.properties().inc_ref();
        p
    }

    fn check(rc: c_int) -> Result<(), MltError> {
        if rc == 0 {
            Ok(())
        } else {
            Err(MltError::PlaylistOp { code: rc })
        }
    }
}

impl Drop for Playlist {
    fn drop(&mut self) {
        // SAFETY: releases this wrapper's single reference.
        unsafe { sys::mlt_playlist_close(self.raw) };
    }
}

/// An MLT tractor: the multitrack composite the whole timeline projects onto.
#[derive(Debug)]
pub struct Tractor {
    raw: sys::mlt_tractor,
}

impl Tractor {
    pub fn new() -> Result<Self, MltError> {
        init()?;
        // SAFETY: no arguments; null means allocation failed.
        let raw = unsafe { sys::mlt_tractor_new() };
        if raw.is_null() {
            return Err(MltError::Init);
        }
        Ok(Self { raw })
    }

    pub fn set_track(&mut self, index: i32, producer: &Producer) -> Result<(), MltError> {
        // SAFETY: both handles live; MLT takes its own reference.
        let rc = unsafe { sys::mlt_tractor_set_track(self.raw, producer.as_raw(), index) };
        if rc == 0 {
            Ok(())
        } else {
            Err(MltError::PlaylistOp { code: rc })
        }
    }

    #[must_use]
    pub fn track_count(&self) -> i32 {
        // SAFETY: valid tractor.
        unsafe { sys::mlt_multitrack_count(sys::mlt_tractor_multitrack(self.raw)) }
    }

    pub fn refresh(&mut self) {
        // SAFETY: valid tractor.
        unsafe { sys::mlt_tractor_refresh(self.raw) };
    }

    /// The tractor as a producer, with its own reference.
    #[must_use]
    pub fn as_producer(&self) -> Producer {
        // SAFETY: as in `Playlist::as_producer`.
        let raw = unsafe { sys::mlt_tractor_producer(self.raw) };
        let p = unsafe { Producer::from_raw(raw) };
        p.properties().inc_ref();
        p
    }

    #[must_use]
    pub fn properties(&self) -> Properties<'_> {
        // SAFETY: the bag belongs to this tractor.
        unsafe { Properties::from_raw(sys::mlt_tractor_properties(self.raw)) }
    }
}

impl Drop for Tractor {
    fn drop(&mut self) {
        // SAFETY: releases this wrapper's single reference.
        unsafe { sys::mlt_tractor_close(self.raw) };
    }
}

/// One decoded frame, owned.
#[derive(Debug)]
pub struct FrameRef {
    raw: sys::mlt_frame,
}

impl FrameRef {
    #[must_use]
    pub fn position(&self) -> i32 {
        // SAFETY: valid frame.
        unsafe { sys::mlt_frame_get_position(self.raw) }
    }

    #[must_use]
    pub fn properties(&self) -> Properties<'_> {
        // SAFETY: the bag belongs to this frame.
        unsafe { Properties::from_raw(sys::mlt_frame_properties(self.raw)) }
    }

    /// Copy the frame's image out as RGBA at the requested size.
    ///
    /// The size request goes into `mlt_frame_get_image`, so asking for half
    /// resolution decodes and scales at half resolution rather than scaling
    /// afterwards - this is what makes scrubbing and the TUI cheap.
    pub fn rgba(&mut self, width: u32, height: u32) -> Result<(Vec<u8>, u32, u32), MltError> {
        let mut buf: *mut u8 = ptr::null_mut();
        let mut fmt: c_int = sys::MLT_IMAGE_RGBA;
        let mut w = width as c_int;
        let mut h = height as c_int;
        // SAFETY: all out-parameters are initialised; MLT writes a buffer it
        // continues to own, so the bytes are copied before returning.
        let rc =
            unsafe { sys::mlt_frame_get_image(self.raw, &mut buf, &mut fmt, &mut w, &mut h, 0) };
        if rc != 0 || buf.is_null() || w <= 0 || h <= 0 {
            return Err(MltError::NoImage);
        }
        if fmt != sys::MLT_IMAGE_RGBA {
            // MLT may hand back its native format when no converting
            // normaliser is planted. Trusting the requested format here would
            // read past the end of a smaller YUV buffer - this check is what
            // turns that into an error instead of a segfault.
            return Err(MltError::WrongFormat { format: fmt });
        }
        let len = (w as usize) * (h as usize) * 4;
        // SAFETY: MLT guarantees an RGBA buffer of w*h*4 bytes on success.
        let bytes = unsafe { std::slice::from_raw_parts(buf, len) }.to_vec();
        Ok((bytes, w as u32, h as u32))
    }
}

impl Drop for FrameRef {
    fn drop(&mut self) {
        // SAFETY: releases this wrapper's single reference.
        unsafe { sys::mlt_frame_close(self.raw) };
    }
}

/// An MLT consumer. davimci uses audio-only consumers: video never goes to an
/// MLT window, because the presenter owns the screen (plan.md Phase 6).
#[derive(Debug)]
pub struct Consumer {
    raw: sys::mlt_consumer,
}

impl Consumer {
    pub fn new(profile: &Profile, service: &str, resource: Option<&str>) -> Result<Self, MltError> {
        init()?;
        let s = cstr(service)?;
        let r = resource.map(cstr).transpose()?;
        let r_ptr = r.as_ref().map_or(ptr::null(), |c| c.as_ptr());
        // SAFETY: valid profile, NUL-terminated strings.
        let raw = unsafe { sys::mlt_factory_consumer(profile.as_raw(), s.as_ptr(), r_ptr) };
        if raw.is_null() {
            return Err(MltError::NoConsumer {
                service: service.into(),
            });
        }
        Ok(Self { raw })
    }

    #[must_use]
    pub fn properties(&self) -> Properties<'_> {
        // SAFETY: the bag belongs to this consumer.
        unsafe { Properties::from_raw(sys::mlt_consumer_properties(self.raw)) }
    }

    pub fn connect(&mut self, producer: &Producer) -> Result<(), MltError> {
        // SAFETY: both handles live.
        let rc = unsafe { sys::mlt_consumer_connect(self.raw, producer.service()) };
        if rc == 0 {
            Ok(())
        } else {
            Err(MltError::ConnectFailed)
        }
    }

    pub fn start(&mut self) -> Result<(), MltError> {
        // SAFETY: valid consumer.
        let rc = unsafe { sys::mlt_consumer_start(self.raw) };
        if rc == 0 {
            Ok(())
        } else {
            Err(MltError::ConsumerStart)
        }
    }

    pub fn stop(&mut self) {
        // SAFETY: valid consumer; idempotent in MLT.
        unsafe { sys::mlt_consumer_stop(self.raw) };
    }

    #[must_use]
    pub fn is_stopped(&self) -> bool {
        // SAFETY: valid consumer.
        unsafe { sys::mlt_consumer_is_stopped(self.raw) != 0 }
    }

    #[must_use]
    pub fn position(&self) -> i32 {
        // SAFETY: valid consumer.
        unsafe { sys::mlt_consumer_position(self.raw) }
    }

    /// Register a `consumer-frame-show` listener.
    ///
    /// # Safety
    /// `data` must remain valid, and safe to touch from the consumer's own
    /// thread, until the returned [`EventHandle`] is dropped.
    pub unsafe fn listen_frame_show(
        &mut self,
        data: *mut std::ffi::c_void,
        listener: sys::mlt_listener,
    ) -> Result<EventHandle, MltError> {
        let id = cstr("consumer-frame-show")?;
        // SAFETY: delegated to the caller by this function's contract.
        let ev = unsafe {
            sys::mlt_events_listen(
                sys::mlt_consumer_properties(self.raw),
                data,
                id.as_ptr(),
                listener,
            )
        };
        if ev.is_null() {
            return Err(MltError::ListenFailed);
        }
        // `mlt_events_listen` hands back an event the properties bag still
        // owns. Taking a reference is what makes this handle's `Drop` a
        // release rather than a double free.
        // SAFETY: `ev` is non-null and freshly registered.
        unsafe { sys::mlt_event_inc_ref(ev) };
        Ok(EventHandle { raw: ev })
    }
}

impl Drop for Consumer {
    fn drop(&mut self) {
        // Stopping first is required: closing a running consumer leaves its
        // thread reading a freed service.
        self.stop();
        // SAFETY: releases this wrapper's single reference.
        unsafe { sys::mlt_consumer_close(self.raw) };
    }
}

/// A registered event listener, unregistered on drop.
#[derive(Debug)]
pub struct EventHandle {
    raw: sys::mlt_event,
}

impl Drop for EventHandle {
    fn drop(&mut self) {
        // SAFETY: owned event, closed once.
        unsafe { sys::mlt_event_close(self.raw) };
    }
}

/// Attach a filter to a producer, transferring ownership to MLT's side.
pub fn attach_filter(producer: &mut Producer, mut filter: Filter) -> Result<(), MltError> {
    producer.attach(&filter)?;
    filter.mark_attached();
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
mod tests {
    use super::*;

    fn profile() -> Profile {
        Profile::new(64, 32, 60, 1).unwrap()
    }

    #[test]
    fn profile_geometry_is_explicit() {
        let p = Profile::new(1920, 1080, 24_000, 1001).unwrap();
        assert_eq!(p.size(), (1920, 1080));
        assert_eq!(p.fps(), (24_000, 1001));
    }

    /// The spec §10.1 accepted risk, tested directly: a cloned handle is a
    /// counted reference, and dropping it gives the count back.
    #[test]
    fn clone_ref_is_balanced_by_drop() {
        let p = profile();
        let producer = Producer::new(&p, "color", "#ffff0000").unwrap();
        let before = producer.properties().ref_count();
        {
            let cloned = producer.clone_ref();
            assert_eq!(cloned.properties().ref_count(), before + 1);
        }
        assert_eq!(producer.properties().ref_count(), before);
    }

    #[test]
    fn create_clone_drop_cycles_do_not_grow_the_refcount() {
        let p = profile();
        let producer = Producer::new(&p, "color", "#ff00ff00").unwrap();
        let start = producer.properties().ref_count();
        for _ in 0..64 {
            let a = producer.clone_ref();
            let b = a.clone_ref();
            drop(a);
            drop(b);
        }
        assert_eq!(producer.properties().ref_count(), start);
    }

    #[test]
    fn a_playlist_planted_in_a_tractor_survives_its_wrapper() {
        let p = profile();
        let mut tractor = Tractor::new().unwrap();
        {
            let mut pl = Playlist::new(&p).unwrap();
            let producer = Producer::new(&p, "color", "#ffff0000").unwrap();
            pl.append(&producer, 0, 9).unwrap();
            tractor.set_track(0, &pl.as_producer()).unwrap();
        }
        // The playlist wrapper is gone; the tractor still owns a reference.
        assert_eq!(tractor.track_count(), 1);
        tractor.refresh();
        let mut root = tractor.as_producer();
        let frame = root.next_frame();
        assert!(frame.is_ok(), "the planted track must still be playable");
    }

    #[test]
    fn playlist_entries_and_blanks_count_frames_not_out_points() {
        let p = profile();
        let mut pl = Playlist::new(&p).unwrap();
        let producer = Producer::new(&p, "color", "#ffff0000").unwrap();
        pl.append(&producer, 0, 9).unwrap();
        pl.append_blank(5).unwrap();
        pl.append(&producer, 0, 4).unwrap();
        assert_eq!(pl.count(), 3);
        let total = pl.as_producer().length();
        assert_eq!(total, 20, "10 frames + 5 blank + 5 frames");
    }

    #[test]
    fn a_missing_service_is_an_error_not_a_null_pointer() {
        let p = profile();
        let err = Producer::new(&p, "definitely_not_a_service", "x").unwrap_err();
        assert!(matches!(err, MltError::NoProducer { .. }));
    }

    #[test]
    fn interior_nul_bytes_are_rejected_before_reaching_c() {
        let p = profile();
        assert!(matches!(
            Producer::new(&p, "color", "a\0b"),
            Err(MltError::BadString { .. })
        ));
    }

    #[test]
    fn a_generated_frame_yields_rgba_at_the_requested_size() {
        let p = profile();
        let mut producer = Producer::new(&p, "color", "#ffff0000").unwrap();
        let mut frame = producer.next_frame().unwrap();
        let (bytes, w, h) = frame.rgba(32, 16).unwrap();
        assert_eq!((w, h), (32, 16));
        assert_eq!(bytes.len(), 32 * 16 * 4);
        assert_eq!(&bytes[0..4], &[255, 0, 0, 255], "colour producer is red");
    }
}
