//! The MLT implementation of [`RenderBackend`].
//!
//! Preview is frame pull: audio goes to a realtime MLT audio consumer,
//! which owns the master clock, while video frames are lifted out of the
//! consumer as RGBA and handed to `davimci-present`. MLT never opens a window
//! (plan.md Phase 6), which is what lets the GUI draw overlays on the video
//! and lets the TUI reuse the same path.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::c_void;
use std::path::Path;
use std::sync::{Arc, Mutex};

use davimci_backend::{
    BackendError, PreviewScale, RenderBackend, RenderJob, RenderProgress, RenderState, SourceInfo,
    VideoFrame,
};
use davimci_core::{ClipId, Fps, Frame, Resolution, Timeline, TimelineProps};
use davimci_mlt_sys as sys;

use crate::cache::FrameCache;
use crate::ffi::{
    Consumer, EventHandle, Filter, Playlist, Producer, Profile, Tractor, Transition, attach_filter,
};
use crate::patch::{Patch, TrackOp, diff};
use crate::projection::{
    AudioLayout, ClipEntry, Entry, Projection, Resource, StreamSelect, TrackProjection,
};

type Result<T> = std::result::Result<T, BackendError>;

/// The live MLT graph: one playlist per track, planted in one tractor.
#[derive(Debug)]
struct Graph {
    tractor: Tractor,
    playlists: Vec<Playlist>,
    root: Producer,
    /// The audio `mix` transitions. The field does not own them, so the graph
    /// does: dropping one while the tractor still points at it would be a
    /// use-after-free.
    _mixes: Vec<Transition>,
    /// Clip-to-clip transitions (spec 6.2), each a nested tractor with its
    /// own planted transition. Kept for the same reason as `_mixes`, and
    /// keyed by the incoming clip so a patch that removes one can drop it
    /// rather than leaking a tractor per edit.
    nested: BTreeMap<ClipId, Nested>,
}

/// One projected transition, alive for as long as the graph that plays it.
#[derive(Debug)]
struct Nested {
    _tractor: Tractor,
    _transition: Transition,
}

/// Frames lifted from the preview consumer, plus the size to lift them at.
#[derive(Debug)]
struct PreviewShared {
    frames: Mutex<VecDeque<VideoFrame>>,
    width: u32,
    height: u32,
}

/// A running preview: an audio consumer plus the listener stealing its video.
#[derive(Debug)]
struct Preview {
    consumer: Consumer,
    shared: Arc<PreviewShared>,
    // Dropped before the consumer, so no callback can fire into freed state.
    _event: EventHandle,
}

/// An export in flight.
#[derive(Debug)]
struct Render {
    consumer: Consumer,
    total: u64,
    cancelled: bool,
    /// An export that keeps audio tracks apart renders from its own graph,
    /// because routing samples onto a wide channel bus is an export-only
    /// shape: preview stays stereo.
    _graph: Option<Graph>,
}

/// MLT-backed renderer and previewer.
#[derive(Debug)]
pub struct MltBackend {
    profile: Profile,
    props: TimelineProps,
    projection: Option<Projection>,
    graph: Option<Graph>,
    preview: Option<Preview>,
    render: Option<Render>,
    finished: Option<RenderProgress>,
    /// How many times the graph has been built from scratch, and how many
    /// times it was patched instead. Tests assert the ratio: a split must not
    /// rebuild (spec 10.1).
    pub rebuilds: usize,
    pub patches: usize,
    /// Decoded stills, so a backward step is a lookup rather than a seek.
    frames: FrameCache,
    /// The last frame asked of [`RenderBackend::frame_at`], which is how a
    /// backward step is recognised as one.
    last_request: Option<Frame>,
    /// How many frames a backward step decodes in one pass. Public for the
    /// media tests, which assert the seek count rather than a wall clock.
    pub backstep_run: u64,
    /// Counts of decoded and served-from-cache stills, so the media tests can
    /// assert that walking backwards decodes each frame once.
    pub decodes: usize,
    pub cache_hits: usize,
}

impl MltBackend {
    /// The graph's current playback speed, or `None` with no graph.
    ///
    /// Exposed because "playing" and "stopped at the end" look identical
    /// from outside MLT: reaching the end leaves the producer at speed zero,
    /// and the regression test for restarting playback needs to see that.
    #[must_use]
    pub fn playback_speed(&self) -> Option<f64> {
        self.graph.as_ref().map(|g| g.root.speed())
    }

    /// Create a backend for a timeline's profile.
    pub fn new(props: TimelineProps) -> Result<Self> {
        let profile = Profile::new(
            props.resolution.width,
            props.resolution.height,
            props.fps.num,
            props.fps.den,
        )
        .map_err(BackendError::from)?;
        Ok(Self {
            profile,
            props,
            projection: None,
            graph: None,
            preview: None,
            render: None,
            finished: None,
            rebuilds: 0,
            patches: 0,
            frames: FrameCache::default(),
            last_request: None,
            backstep_run: 12,
            decodes: 0,
            cache_hits: 0,
        })
    }

    /// The MLT XML for the current projection, for debugging and golden tests.
    #[must_use]
    pub fn to_xml(&self) -> Option<String> {
        self.projection.as_ref().map(crate::xml::to_xml)
    }

    /// Throw away decoded stills. Any change to the graph makes every cached
    /// picture a picture of a timeline that no longer exists.
    fn invalidate_frames(&mut self) {
        self.frames.clear();
        self.last_request = None;
    }

    fn require_graph(&mut self) -> Result<&mut Graph> {
        self.graph.as_mut().ok_or(BackendError::Projection {
            reason: "no timeline has been given to the render backend yet".into(),
        })
    }

    fn rebuild(&mut self, projection: &Projection) -> Result<()> {
        if projection.props != self.props {
            self.profile = Profile::new(
                projection.props.resolution.width,
                projection.props.resolution.height,
                projection.props.fps.num,
                projection.props.fps.den,
            )
            .map_err(BackendError::from)?;
            self.props = projection.props;
        }
        self.graph = Some(self.build_graph(projection)?);
        self.rebuilds += 1;
        Ok(())
    }

    /// Build a whole graph from a projection, without installing it.
    fn build_graph(&self, projection: &Projection) -> Result<Graph> {
        let mut tractor = Tractor::new().map_err(BackendError::from)?;
        let mut playlists = Vec::with_capacity(projection.tracks.len());
        let mut nested = BTreeMap::new();
        for (i, track) in projection.tracks.iter().enumerate() {
            let (pl, ns) = self.build_playlist(track)?;
            nested.extend(ns);
            tractor
                .set_track(i as i32, &pl.as_producer())
                .map_err(BackendError::from)?;
            // `hide` lives on the planted track, not on the playlist: it is
            // what mute and "audio tracks carry no video" both come down to.
            // The planted track and the playlist producer are the same
            // object, so `hide` set here is what the tractor reads.
            pl.as_producer()
                .properties()
                .set_int("hide", i32::from(track.hide()))
                .map_err(BackendError::from)?;
            playlists.push(pl);
        }
        // Without a `mix` per track a tractor plays the audio of one track
        // and drops the rest, so this is what makes a multi-track project
        // audible at all, not only exportable.
        let mut mixes = Vec::new();
        for b in projection.audio_mix_tracks() {
            let Ok(mix) = Transition::new(&self.profile, "mix") else {
                continue;
            };
            {
                let mut p = mix.properties();
                p.set_int("always_active", 1).map_err(BackendError::from)?;
                // Sum rather than average: halving every track's level to
                // avoid clipping is a silent gain change nobody asked for.
                p.set_int("sum", 1).map_err(BackendError::from)?;
            }
            tractor
                .plant(&mix, 0, b as i32)
                .map_err(BackendError::from)?;
            mixes.push(mix);
        }
        tractor.refresh();
        let root = tractor.as_producer();
        Ok(Graph {
            tractor,
            playlists,
            root,
            _mixes: mixes,
            nested,
        })
    }

    fn build_playlist(&self, track: &TrackProjection) -> Result<(Playlist, Vec<(ClipId, Nested)>)> {
        let mut pl = Playlist::new(&self.profile).map_err(BackendError::from)?;
        pl.properties()
            .set("davimci.track", &track.name)
            .map_err(BackendError::from)?;
        let mut nested = Vec::new();
        for entry in &track.entries {
            if let Some(n) = self.append_entry(&mut pl, entry)?
                && let Some(id) = entry.clip_id()
            {
                nested.push((id, n));
            }
        }
        Ok((pl, nested))
    }

    fn append_entry(&self, pl: &mut Playlist, entry: &Entry) -> Result<Option<Nested>> {
        match entry {
            Entry::Blank { length } => {
                pl.append_blank(length.get() as i32)
                    .map_err(BackendError::from)?;
                Ok(None)
            }
            Entry::Clip(c) => {
                let producer = self.build_producer(c)?;
                pl.append(&producer, c.in_point.get() as i32, c.out_point.get() as i32)
                    .map_err(BackendError::from)?;
                Ok(None)
            }
            Entry::Transition(t) => {
                let (producer, nested) = self.build_transition(t)?;
                pl.append(&producer, 0, t.length().saturating_sub(1) as i32)
                    .map_err(BackendError::from)?;
                Ok(Some(nested))
            }
        }
    }

    /// Build the overlap between two clips as a two-track tractor.
    ///
    /// MLT composites tracks, not playlist entries, so a transition inside a
    /// playlist has to be a nested tractor: the outgoing clip's tail on track
    /// 0, the incoming clip's head on track 1, and the transition planted
    /// across them (spec 6.2).
    fn build_transition(
        &self,
        entry: &crate::projection::TransitionEntry,
    ) -> Result<(Producer, Nested)> {
        let from = self.build_producer(&entry.from)?;
        let to = self.build_producer(&entry.to)?;
        let mut tractor = Tractor::new().map_err(BackendError::from)?;
        tractor.set_track(0, &from).map_err(BackendError::from)?;
        tractor.set_track(1, &to).map_err(BackendError::from)?;
        let transition =
            Transition::new(&self.profile, &entry.service).map_err(BackendError::from)?;
        {
            let mut p = transition.properties();
            for (k, v) in &entry.props {
                p.set(k, v).map_err(BackendError::from)?;
            }
            p.set("davimci.transition", &entry.kind)
                .map_err(BackendError::from)?;
        }
        tractor
            .plant(&transition, 0, 1)
            .map_err(BackendError::from)?;
        tractor.refresh();
        let producer = tractor.as_producer();
        Ok((
            producer,
            Nested {
                _tractor: tractor,
                _transition: transition,
            },
        ))
    }

    fn build_producer(&self, entry: &ClipEntry) -> Result<Producer> {
        let mut producer = match Producer::new(
            &self.profile,
            entry.resource.service(),
            &entry.resource.resource(),
        ) {
            Ok(p) => p,
            // Phase 0 "degrade locally": a source that will not open becomes a
            // placeholder rather than an unopenable project.
            Err(_) => {
                Producer::new(&self.profile, "color", "#ff202080").map_err(BackendError::from)?
            }
        };
        {
            let mut props = producer.properties();
            props
                .set("davimci.clip", &entry.clip.to_string())
                .map_err(BackendError::from)?;
            if let Resource::Text(t) = &entry.resource {
                // Some builds lack the Qt text producer; the property is set
                // regardless so the placeholder still carries the payload.
                let _ = props.set("text", t);
            }
            if let Resource::Offline { path } = &entry.resource {
                props
                    .set("davimci.offline", path)
                    .map_err(BackendError::from)?;
            }
            // One track per stream (spec 7): a track that does not name its
            // stream decodes the container's default, so three audio tracks
            // off one file would all play the first stream.
            match entry.stream {
                Some(StreamSelect::Audio(s)) => {
                    props.set_int("audio_index", s as i32)?;
                    props.set_int("video_index", -1)?;
                }
                Some(StreamSelect::Video(s)) => {
                    props.set_int("video_index", s as i32)?;
                    props.set_int("audio_index", -1)?;
                }
                None => {}
            }
        }
        // Normalisers, in MLT's own loader order. A `loader` producer would
        // plant these; davimci creates services directly, so it has to plant
        // them itself or `mlt_frame_get_image` hands back native YUV at
        // native size and `mlt_frame_get_audio` hands back the source's own
        // channel count - which silently breaks every channel-routed export.
        // Each entry is a preference list, exactly as `loader.ini` has it.
        // Normalisers, in MLT's own `loader.ini` order. A `loader` producer
        // would plant these; davimci creates services directly, so it has to
        // plant them itself, and each one is load-bearing: without the image
        // pair `mlt_frame_get_image` returns native YUV at native size, and
        // without the audio three the frame keeps the source's channel count
        // and sample format, which breaks channel-routed export and hands
        // the encoder samples in a format it did not ask for.
        for alternatives in [
            &["avcolor_space"][..],
            &["rescale"][..],
            &["resize"][..],
            &["swresample", "audiochannels"][..],
            &["resample"][..],
            &["audioconvert"][..],
        ] {
            for service in alternatives {
                if let Ok(filter) = Filter::new(&self.profile, service, None) {
                    let _ = attach_filter(&mut producer, filter);
                    break;
                }
            }
        }
        for f in &entry.filters {
            let Ok(filter) = Filter::new(&self.profile, &f.service, None) else {
                continue;
            };
            {
                let mut fp = filter.properties();
                for (k, v) in &f.props {
                    fp.set(k, v).map_err(BackendError::from)?;
                }
            }
            attach_filter(&mut producer, filter).map_err(BackendError::from)?;
        }
        Ok(producer)
    }

    fn apply_patch(
        &mut self,
        patches: &[crate::patch::TrackPatch],
        old: &Projection,
    ) -> Result<()> {
        for tp in patches {
            let old_entries = old
                .tracks
                .get(tp.track_index)
                .map(|t| t.entries.clone())
                .unwrap_or_default();
            let mut live = old_entries;
            for op in &tp.ops {
                self.apply_op(tp.track_index, op, &mut live)?;
            }
        }
        if let Some(g) = self.graph.as_mut() {
            g.tractor.refresh();
        }
        self.patches += 1;
        Ok(())
    }

    fn apply_op(&mut self, track: usize, op: &TrackOp, live: &mut Vec<Entry>) -> Result<()> {
        // Build producers before borrowing the playlist: `build_producer`
        // needs `&self` and the playlist borrow is exclusive.
        let mut keep = None;
        let built = match op {
            TrackOp::Insert { entry, .. } | TrackOp::Update { entry, .. } => match entry {
                Entry::Clip(c) => Some(self.build_producer(c)?),
                Entry::Transition(t) => {
                    let (producer, nested) = self.build_transition(t)?;
                    keep = Some((t.clip, nested));
                    Some(producer)
                }
                Entry::Blank { .. } => None,
            },
            TrackOp::Remove { .. } => None,
        };
        let graph = self.require_graph()?;
        // The nested tractor has to outlive the patch that planted it; the
        // graph is what owns it, exactly as it owns the audio mixes.
        if let Some((id, n)) = keep {
            graph.nested.insert(id, n);
        }
        let pl = graph
            .playlists
            .get_mut(track)
            .ok_or(BackendError::Projection {
                reason: "the render graph has fewer tracks than the timeline".into(),
            })?;
        match op {
            TrackOp::Remove { index } => {
                pl.remove(*index as i32).map_err(BackendError::from)?;
                if *index < live.len() {
                    live.remove(*index);
                }
            }
            TrackOp::Insert { index, entry } => {
                insert_entry(pl, *index, entry, built.as_ref())?;
                live.insert((*index).min(live.len()), entry.clone());
            }
            TrackOp::Update { index, entry } => {
                let resizable = matches!((live.get(*index), entry), (Some(Entry::Clip(a)), Entry::Clip(b))
                    if a.clip == b.clip && a.filters == b.filters);
                if resizable && let Entry::Clip(c) = entry {
                    pl.resize_clip(
                        *index as i32,
                        c.in_point.get() as i32,
                        c.out_point.get() as i32,
                    )
                    .map_err(BackendError::from)?;
                } else {
                    pl.remove(*index as i32).map_err(BackendError::from)?;
                    insert_entry(pl, *index, entry, built.as_ref())?;
                }
                if let Some(slot) = live.get_mut(*index) {
                    *slot = entry.clone();
                }
            }
        }
        Ok(())
    }
}

fn insert_entry(
    pl: &mut Playlist,
    index: usize,
    entry: &Entry,
    producer: Option<&Producer>,
) -> Result<()> {
    match (entry, producer) {
        (Entry::Blank { length }, _) => {
            pl.insert_blank(index as i32, length.get() as i32);
            Ok(())
        }
        (Entry::Clip(c), Some(p)) => pl
            .insert(
                index as i32,
                p,
                c.in_point.get() as i32,
                c.out_point.get() as i32,
            )
            .map_err(BackendError::from),
        (Entry::Transition(t), Some(p)) => pl
            .insert(index as i32, p, 0, t.length().saturating_sub(1) as i32)
            .map_err(BackendError::from),
        (Entry::Clip(_) | Entry::Transition(_), None) => Err(BackendError::Projection {
            reason: "a clip entry reached the graph without a producer".into(),
        }),
    }
}

/// Recover an exact rational framerate from MLT's decimal report.
///
/// NTSC rates are 1000/1001 of an integer, and a project has exactly one
/// framerate (spec 7.1), so guessing a float here would poison every
/// conform downstream.
fn rational_fps(rate: f64) -> Option<Fps> {
    if rate <= 0.0 {
        return None;
    }
    let nearest = rate.round();
    if (rate - nearest).abs() < 0.001 {
        return Fps::new(nearest as u32, 1).ok();
    }
    let ntsc = nearest * 1000.0 / 1001.0;
    if (rate - ntsc).abs() < 0.01 {
        return Fps::new((nearest as u32) * 1000, 1001).ok();
    }
    // Fall back to thousandths, still exact and still rational.
    Fps::new((rate * 1000.0).round() as u32, 1000).ok()
}

/// `consumer-frame-show` listener: copies the shown frame's image out.
///
/// Runs on the consumer's own thread, so it catches unwinds: a panic must
/// never cross back into C (Phase 0 rule 3).
unsafe extern "C" fn on_frame_show(
    _owner: sys::mlt_properties,
    data: *mut c_void,
    event: sys::mlt_event_data,
) {
    let _ = std::panic::catch_unwind(|| {
        if data.is_null() {
            return;
        }
        // SAFETY: `data` is the `Arc<PreviewShared>` registered alongside this
        // listener, kept alive by `Preview` until the event handle is dropped.
        let shared = unsafe { &*(data as *const PreviewShared) };
        // SAFETY: the event carries a frame for `consumer-frame-show`.
        let raw = unsafe { sys::mlt_event_data_to_frame(event) };
        if raw.is_null() {
            return;
        }
        // SAFETY: the frame is owned by the consumer for the duration of the
        // callback; it is borrowed, imaged, and never closed here.
        let position = unsafe { sys::mlt_frame_get_position(raw) };
        let mut buf: *mut u8 = std::ptr::null_mut();
        let mut fmt = sys::MLT_IMAGE_RGBA;
        let mut w = shared.width as i32;
        let mut h = shared.height as i32;
        // SAFETY: all out-parameters are initialised; MLT owns the buffer.
        let rc = unsafe { sys::mlt_frame_get_image(raw, &mut buf, &mut fmt, &mut w, &mut h, 0) };
        if rc != 0 || buf.is_null() || w <= 0 || h <= 0 {
            return;
        }
        // SAFETY: an RGBA buffer of w*h*4 bytes on success.
        let bytes = unsafe { std::slice::from_raw_parts(buf, (w * h * 4) as usize) }.to_vec();
        if let Ok(mut q) = shared.frames.lock() {
            // Bounded: a presenter that stops pulling must not grow the queue
            // without limit. Dropping the oldest keeps playback current.
            if q.len() >= 8 {
                q.pop_front();
            }
            q.push_back(VideoFrame {
                position: Frame(position.max(0) as u64),
                width: w as u32,
                height: h as u32,
                rgba: bytes,
            });
        }
    });
}

impl RenderBackend for MltBackend {
    fn probe(&mut self, path: &Path) -> Result<SourceInfo> {
        let path_str = path.to_string_lossy().to_string();
        if !path.exists() {
            return Err(BackendError::Offline { path: path_str });
        }
        let producer = Producer::new(&self.profile, "avformat", &path_str)?;
        let props = producer.properties();
        // Keys are per stream (`meta.media.<n>.codec.*`); there is no
        // file-level width or frame rate to read.
        let streams = props.get_int("meta.media.nb_streams").max(0);
        let mut audio_streams = 0usize;
        let mut resolution = None;
        let mut fps = None;
        let mut sample_rate = None;
        for i in 0..streams {
            match props.get(&format!("meta.media.{i}.stream.type")).as_deref() {
                Some("audio") => {
                    audio_streams += 1;
                    if sample_rate.is_none() {
                        let r = props.get_int(&format!("meta.media.{i}.codec.sample_rate"));
                        sample_rate = (r > 0).then_some(r as u32);
                    }
                }
                Some("video") if resolution.is_none() => {
                    let w = props.get_int(&format!("meta.media.{i}.codec.width"));
                    let h = props.get_int(&format!("meta.media.{i}.codec.height"));
                    if w > 0 && h > 0 {
                        resolution = Some(Resolution {
                            width: w as u32,
                            height: h as u32,
                        });
                    }
                    // MLT reports the rate as a decimal; recover the exact
                    // rational rather than storing a float (spec 7.1).
                    let rate = props
                        .get(&format!("meta.media.{i}.stream.frame_rate"))
                        .and_then(|v| v.parse::<f64>().ok())
                        .unwrap_or(0.0);
                    fps = rational_fps(rate);
                }
                _ => {}
            }
        }
        Ok(SourceInfo {
            path: path_str,
            has_video: resolution.is_some(),
            resolution,
            fps,
            frames: producer.length().max(0) as u64,
            audio_streams,
            sample_rate,
        })
    }

    fn set_timeline(&mut self, timeline: &Timeline) -> Result<()> {
        let next = Projection::of(timeline);
        match self.projection.take() {
            Some(prev) if self.graph.is_some() => match diff(&prev, &next) {
                Patch::None => {}
                Patch::Rebuild => {
                    self.invalidate_frames();
                    self.rebuild(&next)?;
                }
                Patch::Tracks(patches) => {
                    self.invalidate_frames();
                    self.apply_patch(&patches, &prev)?;
                    // Transitions the patch removed are no longer played by
                    // any playlist, so the graph stops owning them.
                    let live = next.transition_clips();
                    if let Some(g) = self.graph.as_mut() {
                        g.nested.retain(|id, _| live.contains(id));
                    }
                }
            },
            _ => {
                self.invalidate_frames();
                self.rebuild(&next)?;
            }
        }
        self.projection = Some(next);
        Ok(())
    }

    fn seek(&mut self, frame: Frame) -> Result<()> {
        let graph = self.require_graph()?;
        graph.root.seek(frame.get() as i32);
        Ok(())
    }

    fn frame_at(&mut self, frame: Frame, scale: PreviewScale) -> Result<VideoFrame> {
        let previous = self.last_request.replace(frame);
        if let Some(hit) = self.frames.get(frame, scale) {
            self.cache_hits = self.cache_hits.saturating_add(1);
            return Ok(hit.clone());
        }

        // Decoding backwards one frame at a time is the pathological case: a
        // seek to `n - 1` discards the decoder state and re-decodes from the
        // preceding keyframe, so each step costs a whole GOP. When the
        // request is a step backwards, decode the run *leading up to* the
        // target instead - one seek, then sequential decodes - so the steps
        // that follow are cache hits.
        let stepping_back = previous.is_some_and(|p| p > frame);
        let start = if stepping_back {
            frame
                .get()
                .saturating_sub(self.backstep_run.saturating_sub(1))
        } else {
            frame.get()
        };

        let res = scale.apply(self.props.resolution);
        let count = frame.get().saturating_sub(start).saturating_add(1);
        let mut run: Vec<VideoFrame> = Vec::new();
        let graph = self.require_graph()?;
        graph.root.seek(start as i32);
        for i in 0..count {
            let at = Frame(start.saturating_add(i));
            let decoded = match graph.root.next_frame() {
                Ok(mut pulled) => match pulled.rgba(res.width, res.height) {
                    Ok((rgba, width, height)) => VideoFrame {
                        position: at,
                        width,
                        height,
                        rgba,
                    },
                    // Phase 0: one bad frame degrades to black, it does not
                    // end the session.
                    Err(_) => VideoFrame::black(at, res),
                },
                // The run is an optimisation: only failing on the frame that
                // was actually asked for is an error.
                Err(_) if at != frame => break,
                Err(_) => return Err(BackendError::Seek { frame: frame.get() }),
            };
            run.push(decoded);
        }

        let mut wanted: Option<VideoFrame> = None;
        for decoded in run {
            self.decodes = self.decodes.saturating_add(1);
            if decoded.position == frame {
                wanted = Some(decoded.clone());
            }
            self.frames.insert(scale, decoded);
        }
        match wanted {
            Some(f) => Ok(f),
            None => Err(BackendError::Seek { frame: frame.get() }),
        }
    }

    fn preview_start(&mut self, from: Frame, scale: PreviewScale) -> Result<()> {
        if self.preview.is_some() {
            return Err(BackendError::PreviewAlreadyRunning);
        }
        let res = scale.apply(self.props.resolution);
        self.seek(from)?;
        let graph = self.require_graph()?;
        // Reaching the end of a producer leaves MLT with its speed at zero,
        // and a seek does not undo that. Without this, the *second* play
        // after playback once ran off the end reports "playing" and never
        // advances a frame.
        graph.root.set_speed(1.0);
        let root = graph.root.clone_ref();

        // Audio-only consumers, in preference order: MLT must never own a
        // video window here.
        let mut consumer = ["sdl2_audio", "rtaudio", "null"]
            .iter()
            .find_map(|s| Consumer::new(&self.profile, s, None).ok())
            .ok_or(BackendError::Unavailable {
                reason: "no audio output is available for preview".into(),
            })?;
        {
            let mut props = consumer.properties();
            props.set_int("real_time", 1)?;
            let _ = props.set("terminate_on_pause", "0");
        }
        let shared = Arc::new(PreviewShared {
            frames: Mutex::new(VecDeque::new()),
            width: res.width,
            height: res.height,
        });
        // SAFETY: the pointer is an `Arc` this struct keeps alive for exactly
        // as long as the event handle, which is dropped before the consumer.
        let event = unsafe {
            consumer.listen_frame_show(Arc::as_ptr(&shared) as *mut c_void, Some(on_frame_show))
        }?;
        consumer.connect(&root)?;
        consumer.start()?;
        self.preview = Some(Preview {
            consumer,
            shared,
            _event: event,
        });
        Ok(())
    }

    fn preview_stop(&mut self) -> Result<()> {
        match self.preview.take() {
            Some(mut p) => {
                p.consumer.stop();
                Ok(())
            }
            None => Err(BackendError::PreviewNotRunning),
        }
    }

    fn is_previewing(&self) -> bool {
        self.preview.is_some()
    }

    fn supports_varispeed(&self) -> bool {
        true
    }

    fn set_rate(&mut self, rate: f64) -> Result<()> {
        if !rate.is_finite() {
            return Err(BackendError::Unavailable {
                reason: "a playback rate must be a finite number".into(),
            });
        }
        // Speed lives on the producer, not the consumer: the consumer keeps
        // pulling at wall-clock rate and the producer decides which frame
        // that is, which is what makes the audio clock stay the master.
        let graph = self.require_graph()?;
        graph.root.set_speed(rate);
        if let Some(p) = self.preview.as_mut() {
            p.consumer.properties().set_int("refresh", 1)?;
        }
        Ok(())
    }

    fn next_preview_frame(&mut self) -> Result<Option<VideoFrame>> {
        let preview = self
            .preview
            .as_ref()
            .ok_or(BackendError::PreviewNotRunning)?;
        let mut q = preview
            .shared
            .frames
            .lock()
            .map_err(|_| BackendError::Unavailable {
                reason: "the preview frame queue was poisoned".into(),
            })?;
        Ok(q.pop_front())
    }

    fn audio_clock_position(&self) -> Option<Frame> {
        let p = self.preview.as_ref()?;
        let pos = p.consumer.position();
        (pos >= 0).then_some(Frame(pos as u64))
    }

    fn register_transition(&mut self, def: davimci_backend::TransitionDef) -> Result<()> {
        if def.service.trim().is_empty() {
            return Err(BackendError::Unavailable {
                reason: format!("the transition type '{}' names no service", def.name),
            });
        }
        crate::transitions::register(&def.name, &def.service, def.props);
        Ok(())
    }

    fn transition_names(&self) -> Vec<String> {
        let mut names: Vec<String> = crate::transitions::names()
            .into_iter()
            .map(str::to_string)
            .collect();
        names.extend(crate::transitions::registered_names());
        names.sort();
        names.dedup();
        names
    }

    fn render(&mut self, job: RenderJob) -> Result<()> {
        if self.render.is_some() {
            return Err(BackendError::Render {
                reason: "a render is already in progress".into(),
            });
        }
        let (start, end) = job.range.unwrap_or((
            Frame::ZERO,
            Frame(
                self.graph
                    .as_ref()
                    .map_or(0, |g| g.root.length().max(0) as u64),
            ),
        ));
        if end < start {
            return Err(BackendError::Render {
                reason: format!("the export range {start} to {end} runs backwards"),
            });
        }
        let output = job.output.to_string_lossy().to_string();

        // An export that keeps audio tracks separate renders from its own
        // graph: each audio track is routed onto its own channel pair before
        // the mix sums them, and the consumer cuts the bus into streams.
        let mut export_graph = None;
        let mut layout: Option<AudioLayout> = None;
        let needs_own_graph = job.settings.separate_audio_tracks || !job.settings.burn_subtitles;
        if needs_own_graph && let Some(mut projection) = self.projection.clone() {
            // Subtitles that are not burned in must not reach the picture:
            // the text tracks are dropped from the exported graph and
            // carried by the sidecar or the muxed stream instead (spec 8).
            let dropped = !job.settings.burn_subtitles && projection.drop_text_tracks();
            if job.settings.separate_audio_tracks {
                layout = projection.route_audio();
            }
            if layout.is_some() || dropped {
                export_graph = Some(self.build_graph(&projection)?);
            }
        }

        let mut root = match &export_graph {
            Some(g) => g.root.clone_ref(),
            None => self.require_graph()?.root.clone_ref(),
        };
        root.set_in_and_out(start.get() as i32, end.get().saturating_sub(1) as i32);
        root.seek(start.get() as i32);

        let mut consumer = Consumer::new(&self.profile, "avformat", Some(&output))?;
        {
            let mut props = consumer.properties();
            if let Some(layout) = &layout {
                props.set_int("channels", i32::from(layout.total_channels))?;
                for (n, route) in layout.routes.iter().enumerate() {
                    props.set_int(&format!("channels.{n}"), i32::from(route.channels))?;
                }
            }
            // Not realtime: an export must drop nothing.
            props.set_int("real_time", 0)?;
            props.set_int("terminate_on_pause", 1)?;
            props.set("vcodec", &job.settings.video_codec)?;
            props.set("acodec", &job.settings.audio_codec)?;
            props.set_int("width", job.settings.resolution.width as i32)?;
            props.set_int("height", job.settings.resolution.height as i32)?;
            props.set_int("frame_rate_num", job.settings.fps.num as i32)?;
            props.set_int("frame_rate_den", job.settings.fps.den as i32)?;
            for (k, v) in &job.settings.extra {
                props.set(k, v)?;
            }
        }
        consumer.connect(&root)?;
        consumer.start()?;
        self.finished = None;
        self.render = Some(Render {
            consumer,
            total: end.get().saturating_sub(start.get()),
            cancelled: false,
            _graph: export_graph,
        });
        Ok(())
    }

    fn progress(&self) -> RenderProgress {
        if let Some(r) = &self.render {
            let rendered = (r.consumer.position().max(0) as u64).min(r.total);
            let state = if r.cancelled {
                RenderState::Cancelled
            } else if r.consumer.is_stopped() {
                RenderState::Done
            } else {
                RenderState::Running
            };
            return RenderProgress {
                state,
                rendered,
                total: r.total,
            };
        }
        self.finished.clone().unwrap_or_else(RenderProgress::idle)
    }

    fn cancel_render(&mut self) -> Result<()> {
        if let Some(mut r) = self.render.take() {
            r.consumer.stop();
            r.cancelled = true;
            self.finished = Some(RenderProgress {
                state: RenderState::Cancelled,
                rendered: 0,
                total: r.total,
            });
        }
        Ok(())
    }
}
