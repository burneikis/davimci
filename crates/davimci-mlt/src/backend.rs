//! The MLT implementation of [`RenderBackend`].
//!
//! Preview is frame pull: audio goes to a realtime MLT audio consumer,
//! which owns the master clock, while video frames are lifted out of the
//! consumer as RGBA and handed to `davimci-present`. MLT never opens a window,
//! which is what lets the GUI draw overlays on the video
//! and lets the TUI reuse the same path.
//!
//! Playing backwards is the exception. MLT decodes a backwards pass one seek
//! per frame and drops none of it, so the picture falls further behind the
//! sound the longer it runs. Instead the consumer plays sound only and
//! `Scrub` decodes the picture from a graph of its own, chasing the clock
//! and skipping what it cannot keep up with, so the speed stays honest.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::c_void;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use davimci_backend::{
    AccelerationStatus, BackendError, DecodePolicy, PlanarFrame, PreviewScale, RenderBackend,
    RenderJob, RenderProgress, RenderState, SourceInfo, VideoFrame,
};
use davimci_core::{ClipId, Fps, Frame, Resolution, Timeline, TimelineProps};
use davimci_mlt_sys as sys;

use crate::cache::FrameCache;
use crate::convert::{count, frames, mlt_int, size};
use crate::ffi::{
    Consumer, EventHandle, Filter, Playlist, Producer, Profile, Tractor, Transition, attach_filter,
};
use crate::hwaccel::Acceleration;
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
    /// The video blends, kept alive for the same reason as `_mixes`.
    _blends: Vec<Transition>,
    /// Clip-to-clip transitions, each a nested tractor with its
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
    /// Whether the listener should image the frames it is shown. False for a
    /// backwards pass, where the picture comes from [`Preview::scrub`]
    /// instead and imaging here would decode every frame twice.
    image: bool,
}

/// A running preview: an audio consumer plus the listener stealing its video.
#[derive(Debug)]
struct Preview {
    consumer: Consumer,
    shared: Arc<PreviewShared>,
    // Kept alive for as long as the consumer that fires it. `Drop` stops the
    // consumer first, so no callback can reach freed state.
    _event: EventHandle,
    scale: PreviewScale,
    /// Set while the consumer runs backwards. MLT decodes a backwards pass
    /// one seek per frame and never drops any of it, so the picture falls
    /// further behind the sound the longer the shuttle runs. Instead the
    /// consumer plays audio only and the picture is decoded by this worker,
    /// in runs, at whatever position the clock has reached.
    scrub: Option<Scrub>,
}

impl Preview {
    fn is_reverse(&self) -> bool {
        self.scrub.is_some()
    }
}

impl Drop for Preview {
    /// The consumer runs on a thread of its own that reads the listener's
    /// shared state and pulls from the graph. It is stopped before anything
    /// it touches is released; field order alone would free the shared state
    /// under a running thread.
    fn drop(&mut self) {
        self.consumer.stop();
        drop(self.scrub.take());
    }
}

/// Backwards preview pictures, decoded off the caller's thread.
///
/// Decoding backwards costs a seek and a run of frames, tens of milliseconds
/// at a time; doing that inline would stall the frontend's event loop for
/// longer than the frame it is trying to draw. The worker chases the audio
/// clock instead: it decodes the run leading up to wherever the clock has
/// reached and skips whatever the clock passed while it was busy, so the
/// picture tracks the sound rather than falling further behind it.
#[derive(Debug)]
struct Scrub {
    /// The frame the clock is on, written by the caller and read by the
    /// worker. Negative means "nothing wanted yet".
    target: Arc<AtomicI64>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

/// A graph moved to the worker that will own it from then on.
struct Handoff(Graph);

// SAFETY: MLT services are owned by one thread at a time, and this is the
// hand-over: the graph is built on the caller's thread and from the moment it
// is sent it is touched only by the worker. It is not shared, and the graph
// the consumer pulls from is a different one.
unsafe impl Send for Handoff {}

/// Hand `low..=high` from `window` to the preview queue, newest position
/// first, which is the order a backwards pass presents them in.
fn push_range(
    shared: &Arc<PreviewShared>,
    window: &VecDeque<VideoFrame>,
    low: i64,
    high: i64,
) -> Option<u64> {
    let mut lowest = None;
    let Ok(mut q) = shared.frames.lock() else {
        return lowest;
    };
    for p in (low..=high).rev() {
        #[allow(
            clippy::cast_sign_loss,
            reason = "both ends are positions, which are never negative"
        )]
        let at = Frame(p.max(0) as u64);
        if let Some(f) = window.iter().find(|f| f.position == at) {
            // Full means the pass is queueing pictures faster than they are
            // being shown, so the one thrown away is the furthest from being
            // due. Dropping from the front would throw away the picture the
            // clock is on and leave only frames it has not reached yet.
            while q.len() >= 16 {
                q.pop_back();
            }
            lowest = Some(f.position.get());
            q.push_back(f.clone());
        }
    }
    lowest
}

/// Blend a fresh cost measure into the running one.
///
/// A cold decode - the first of a shuttle, or one on a loaded machine - costs
/// hundreds of frames of clock. Aiming that far below the clock lands on frame
/// zero, where a backwards pass has nothing left to show, so the measure is
/// capped at `reach` and averaged with the last one.
fn blend_cost(cost: u64, travel: u64, decoded: u64, reach: u64) -> u64 {
    let measured = travel.div_ceil(decoded.max(1)).clamp(1, reach.max(1));
    cost.midpoint(measured).max(1)
}

impl Scrub {
    fn spawn(graph: Graph, shared: &Arc<PreviewShared>, res: Resolution, run: u64) -> Self {
        let target = Arc::new(AtomicI64::new(-1));
        let stop = Arc::new(AtomicBool::new(false));
        let handoff = Handoff(graph);
        let (want, halt, out) = (Arc::clone(&target), Arc::clone(&stop), Arc::clone(shared));
        let worker = std::thread::spawn(move || {
            // The whole wrapper moves, not its field: the wrapper is what
            // carries the promise that only this thread will touch it.
            let handoff = handoff;
            let mut graph = handoff.0;
            // Frames already decoded, ascending, covering a window that ends
            // where the clock is and reaches down to where it is going.
            let mut window: VecDeque<VideoFrame> = VecDeque::new();
            let mut served = -1_i64;
            let run = run.max(1);
            let gap = i64::try_from(run).unwrap_or(i64::MAX);
            let keep = usize::try_from(run.saturating_mul(4)).unwrap_or(usize::MAX);
            // Clock frames that go by per frame decoded, which is the one
            // measure that says whether a run can keep up at this speed and
            // how far ahead to aim when it cannot. It is capped: a cold first
            // decode costs hundreds of frames of clock, and aiming that far
            // down lands on frame zero, where a backwards pass has nothing
            // left to show and the preview freezes.
            let mut cost = 1_u64;
            let reach = run.saturating_mul(4);
            // The lowest picture handed over so far: a backwards pass only
            // ever goes down.
            let mut published: Option<u64> = None;
            // The consumer reports position zero until it has played its
            // first frame. Decoding for that would open every backwards
            // shuttle by flashing the first frame of the timeline.
            let mut running = false;
            while !halt.load(Ordering::Relaxed) {
                let target = want.load(Ordering::Relaxed);
                if target <= 0 && !running {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                running = true;
                #[allow(
                    clippy::cast_sign_loss,
                    reason = "the sentinel is negative and was returned above"
                )]
                let at = Frame(target as u64);
                let held = window.iter().any(|f| f.position == at);
                if target != served && held {
                    // The clock is reported in steps of more than a frame, so
                    // the frames it passed between two readings are handed
                    // over too: pacing puts each one up when the sound
                    // reaches it, which is the difference between a backwards
                    // pass that moves and one that jerks.
                    let from = match served {
                        s if s > target && s - target <= gap => s - 1,
                        _ => target,
                    };
                    served = target;
                    // A backwards pass only ever goes down, whatever the
                    // clock says: a picture at or above the last one handed
                    // over is one pacing would throw away.
                    let ceiling = published.map_or(i64::MAX, |p| p.cast_signed().saturating_sub(1));
                    if target <= ceiling {
                        published =
                            push_range(&out, &window, target, from.min(ceiling)).or(published);
                    }
                }

                let bottom = window.front().map(|f| f.position.get());
                // Faster than a run can be decoded, the pass stops trying to
                // show every frame: it decodes one picture at a time, aimed
                // at where the clock will have got to by the time that
                // picture is ready. Decoding runs it will never catch up with
                // is what leaves the preview frozen at speed.
                let chasing = cost > 1;
                let plan = if chasing {
                    // One picture at a time, one decode's worth below the
                    // clock, and never above the last picture handed over: a
                    // backwards pass that goes back up is a picture pacing
                    // throws away.
                    let ceiling = published.map_or(u64::MAX, |p| p.saturating_sub(1));
                    let aim = Frame(at.get().saturating_sub(cost).min(ceiling));
                    (published != Some(0)).then_some((aim, aim))
                } else if !held {
                    // Nothing for where the clock is: re-acquire there.
                    Some((Frame(at.get().saturating_sub(run - 1)), at))
                } else {
                    // Decode below the clock so the frames it is about to
                    // reach are already there when it arrives.
                    match bottom {
                        Some(0) => None,
                        Some(lo) if at.get().saturating_sub(lo) < run => {
                            Some((Frame(lo.saturating_sub(run)), Frame(lo.saturating_sub(1))))
                        }
                        _ => None,
                    }
                };
                let Some((start, end)) = plan else {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                };

                let before = want.load(Ordering::Relaxed);
                let Ok(fresh) = decode_run(&mut graph, start, end, res) else {
                    // A picture that will not decode leaves the last one up
                    // rather than ending the shuttle.
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                };
                let travel = before
                    .saturating_sub(want.load(Ordering::Relaxed))
                    .unsigned_abs();
                let decoded = end.get().saturating_sub(start.get()).saturating_add(1);
                // Averaged with the last measure so one slow decode moves the
                // aim rather than deciding it.
                cost = blend_cost(cost, travel, decoded, reach);
                let adjacent = bottom.is_some_and(|lo| end.get().saturating_add(1) == lo);
                if !adjacent {
                    window.clear();
                }
                for f in fresh.into_iter().rev() {
                    window.push_front(f);
                }
                if chasing {
                    // The clock has moved on while this was decoding, so it
                    // will never be asked for by position: hand it over now.
                    // It is below the clock, which is where pacing expects
                    // the next backwards picture to be.
                    served = want.load(Ordering::Relaxed);
                    let at = end.get().cast_signed();
                    published = push_range(&out, &window, at, at).or(published);
                }
                // Only frames the pass has already gone by are forgotten:
                // trimming the top would throw away the picture the clock is
                // about to ask for and put the window straight back into a
                // miss.
                while window.len() > keep && window.back().is_some_and(|f| f.position > at) {
                    window.pop_back();
                }
            }
        });
        Self {
            target,
            stop,
            worker: Some(worker),
        }
    }

    /// Tell the worker which frame the clock is on.
    fn track(&self, at: Frame) {
        self.target.store(
            i64::try_from(at.get()).unwrap_or(i64::MAX),
            Ordering::Relaxed,
        );
    }
}

impl Drop for Scrub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

/// What the backstep prefetcher is doing, shared with its worker.
#[derive(Debug, Default)]
struct PrefetchState {
    /// The run to decode next, replacing whatever was queued: the user's
    /// latest position is the only one worth decoding for.
    want: Option<(Frame, Frame)>,
    /// The run being decoded right now, so a caller that needs a frame from
    /// it waits for it instead of decoding it a second time.
    busy: Option<(Frame, Frame)>,
    /// Decoded frames waiting to be taken into the frame cache.
    ready: Vec<VideoFrame>,
    stop: bool,
}

/// Backward-step pictures, decoded before they are asked for.
///
/// A backward step decodes the run *leading up to* the target, which is one
/// seek and a dozen sequential decodes - tens of milliseconds on the thread
/// drawing the UI, once per run. Held `h` therefore hitched every
/// `backstep_run` frames. The worker decodes the next run down from a graph
/// of its own while the transport is idle, so by the time the walk reaches
/// it the pictures are already there.
#[derive(Debug)]
struct Prefetch {
    shared: Arc<(Mutex<PrefetchState>, std::sync::Condvar)>,
    /// The scale it decodes at; a different one needs a different worker,
    /// since a cached picture is only valid for the scale it was made at.
    scale: PreviewScale,
    worker: Option<JoinHandle<()>>,
}

impl Prefetch {
    fn spawn(graph: Graph, scale: PreviewScale, res: Resolution) -> Self {
        let shared = Arc::new((
            Mutex::new(PrefetchState::default()),
            std::sync::Condvar::new(),
        ));
        let handoff = Handoff(graph);
        let mine = Arc::clone(&shared);
        let worker = std::thread::spawn(move || {
            // The wrapper moves, not its field: it is what carries the
            // promise that only this thread touches the graph.
            let handoff = handoff;
            let mut graph = handoff.0;
            let (lock, signal) = &*mine;
            loop {
                let run = {
                    let Ok(mut state) = lock.lock() else {
                        return;
                    };
                    while !state.stop && state.want.is_none() {
                        let Ok((next, _)) = signal.wait_timeout(state, Duration::from_millis(50))
                        else {
                            return;
                        };
                        state = next;
                    }
                    if state.stop {
                        return;
                    }
                    let run = state.want.take();
                    state.busy = run;
                    run
                };
                let Some((start, end)) = run else {
                    continue;
                };
                let decoded = decode_run(&mut graph, start, end, res).unwrap_or_default();
                let Ok(mut state) = lock.lock() else {
                    return;
                };
                state.ready.extend(decoded);
                state.busy = None;
                signal.notify_all();
            }
        });
        Self {
            shared,
            scale,
            worker: Some(worker),
        }
    }

    /// Ask for `start..=end` next, unless a run is already queued or being
    /// decoded: one run outstanding is what keeps the worker one run ahead
    /// of the walk rather than a whole timeline ahead of it.
    fn request(&self, start: Frame, end: Frame) {
        let (lock, signal) = &*self.shared;
        if let Ok(mut state) = lock.lock() {
            if state.want.is_some() || state.busy.is_some() {
                return;
            }
            state.want = Some((start, end));
            signal.notify_all();
        }
    }

    /// Frames decoded since the last call.
    fn take_ready(&self) -> Vec<VideoFrame> {
        let (lock, _) = &*self.shared;
        lock.lock()
            .map(|mut s| std::mem::take(&mut s.ready))
            .unwrap_or_default()
    }

    /// Wait for the run covering `frame`, if one is being decoded.
    ///
    /// Bounded: a worker that is wedged costs one late picture, never the
    /// event loop. Returns whatever landed while waiting.
    fn wait_for(&self, frame: Frame, timeout: Duration) -> Vec<VideoFrame> {
        let (lock, signal) = &*self.shared;
        let Ok(mut state) = lock.lock() else {
            return Vec::new();
        };
        let covers =
            |run: Option<(Frame, Frame)>| run.is_some_and(|(s, e)| s <= frame && frame <= e);
        if !covers(state.busy) && !covers(state.want) {
            return std::mem::take(&mut state.ready);
        }
        let deadline = std::time::Instant::now() + timeout;
        while covers(state.busy) || covers(state.want) {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                break;
            }
            let Ok((next, _)) = signal.wait_timeout(state, left) else {
                return Vec::new();
            };
            state = next;
        }
        std::mem::take(&mut state.ready)
    }
}

impl Drop for Prefetch {
    fn drop(&mut self) {
        {
            let (lock, signal) = &*self.shared;
            if let Ok(mut state) = lock.lock() {
                state.stop = true;
                state.want = None;
            }
            signal.notify_all();
        }
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
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
///
/// Teardown order is load-bearing and is spelled out in [`Drop`]: field
/// order would close the profile and the graph while the preview consumer's
/// thread and the prefetch worker are still reading them.
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
    /// rebuild.
    pub rebuilds: usize,
    pub patches: usize,
    /// Decoded stills, so a backward step is a lookup rather than a seek.
    frames: FrameCache,
    /// The worker decoding the next backward run before it is asked for,
    /// alive only while the transport is idle.
    prefetch: Option<Prefetch>,
    /// The lowest frame of the contiguous backward run currently cached.
    /// What the prefetcher works down from.
    run_floor: Option<u64>,
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
    /// Frames the prefetcher had ready before they were asked for. The media
    /// tests assert this: a backward walk that hitches is one whose runs were
    /// decoded on the caller's thread instead.
    pub prefetched: usize,
    /// The decode policy and what the hardware probe found. Software decode
    /// until the session opts in, so every default run - CI included - takes
    /// the reference path.
    accel: Acceleration,
    /// How many producers were built with a hardware decoder attached, for
    /// the benches and the slow tests: the policy is per source, so a count
    /// is the only way to assert it took effect. A `Cell` because producers
    /// are built behind a shared reference.
    hardware_producers: std::cell::Cell<usize>,
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

    /// Serve one still from the cache, or decode it.
    ///
    /// Decoding backwards one frame at a time is the pathological case: a
    /// seek to `n - 1` discards the decoder state and re-decodes from the
    /// preceding keyframe, so each step costs a whole GOP. When the request
    /// is a step backwards, decode the run *leading up to* the target
    /// instead - one seek, then sequential decodes - so the steps that follow
    /// are cache hits.
    fn pull(
        &mut self,
        frame: Frame,
        scale: PreviewScale,
        stepping_back: bool,
    ) -> Result<VideoFrame> {
        self.adopt_prefetched(scale);
        if let Some(hit) = self.frames.get(frame, scale).cloned() {
            self.cache_hits = self.cache_hits.saturating_add(1);
            if stepping_back {
                self.prefetch_below(frame, scale);
            }
            return Ok(hit);
        }
        // A run the worker is already decoding is waited for rather than
        // decoded again: it started before the walk got here, so it is
        // usually a wait of nothing at all.
        if stepping_back && let Some(prefetch) = self.prefetch.as_ref().filter(|p| p.scale == scale)
        {
            let ready = prefetch.wait_for(frame, Duration::from_millis(250));
            for decoded in ready {
                self.decodes = self.decodes.saturating_add(1);
                self.run_floor = Some(
                    self.run_floor
                        .map_or(decoded.position.get(), |f| f.min(decoded.position.get())),
                );
                self.frames.insert(scale, decoded);
            }
            if let Some(hit) = self.frames.get(frame, scale).cloned() {
                self.cache_hits = self.cache_hits.saturating_add(1);
                self.prefetch_below(frame, scale);
                return Ok(hit);
            }
        }
        let start = if stepping_back {
            frame
                .get()
                .saturating_sub(self.backstep_run.saturating_sub(1))
        } else {
            frame.get()
        };

        let res = scale.apply(self.props.resolution);
        let graph = self.graph.as_mut().ok_or(BackendError::Projection {
            reason: "no timeline has been given to the render backend yet".into(),
        })?;
        let run = decode_run(graph, Frame(start), frame, res)?;
        let served = keep_run(&mut self.frames, &mut self.decodes, scale, frame, run)
            .ok_or(BackendError::Seek { frame: frame.get() })?;
        if stepping_back {
            self.run_floor = Some(start);
            self.prefetch_below(frame, scale);
        }
        Ok(served)
    }

    /// Stop everything running on a thread of its own, then release the
    /// services those threads read.
    ///
    /// Shared by [`Drop`] and by any path that has to reach a quiet backend
    /// before touching the profile.
    fn shutdown(&mut self) {
        if let Some(r) = self.render.as_mut() {
            r.consumer.stop();
        }
        self.render = None;
        // The consumer's thread pulls from `graph`; the workers own graphs of
        // their own built from `profile`. Both are joined before either is
        // freed.
        self.preview = None;
        self.prefetch = None;
        self.graph = None;
        self.projection = None;
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
            prefetch: None,
            run_floor: None,
            last_request: None,
            backstep_run: 12,
            decodes: 0,
            cache_hits: 0,
            prefetched: 0,
            accel: Acceleration::default(),
            hardware_producers: std::cell::Cell::new(0),
        })
    }

    /// How many producers the current session opened with a hardware
    /// decoder attached.
    #[must_use]
    pub fn hardware_producers(&self) -> usize {
        self.hardware_producers.get()
    }

    /// Attach a hardware decoder to a freshly opened `avformat` producer,
    /// when the probe says this source is worth it.
    ///
    /// Safe to do after construction: MLT opens the container when the
    /// producer is created but does not initialise the video codec until the
    /// first frame is pulled, which is where `hwaccel` is read. Frames still
    /// come back through system memory, so they stay comparable with the
    /// software path.
    fn request_hardware_decode(&self, props: &mut crate::ffi::Properties<'_>) {
        let codec = props.get("meta.media.0.codec.name").or_else(|| {
            // Stream 0 is not always the video stream; the first video one is.
            let streams = props.get_int("meta.media.nb_streams").max(0);
            (0..streams)
                .find(|i| {
                    props.get(&format!("meta.media.{i}.stream.type")).as_deref() == Some("video")
                })
                .and_then(|i| props.get(&format!("meta.media.{i}.codec.name")))
        });
        let pixels = u64::from(
            props
                .get_int("meta.media.0.codec.width")
                .max(0)
                .unsigned_abs(),
        ) * u64::from(
            props
                .get_int("meta.media.0.codec.height")
                .max(0)
                .unsigned_abs(),
        );
        let Some(choice) = self.accel.choose(codec.as_deref(), pixels) else {
            return;
        };
        // A property MLT refuses is a source that keeps decoding in
        // software, never a failed edit.
        if props.set("hwaccel", choice.method).is_ok()
            && props.set("hwaccel_device", &choice.device).is_ok()
        {
            self.hardware_producers
                .set(self.hardware_producers.get() + 1);
        }
    }

    /// Whether this machine can really encode with `encoder`, established
    /// by encoding with it.
    ///
    /// A render node is a decode capability at best: cards that decode
    /// H.264 and cannot encode it are common, and the failure surfaces as a
    /// container with no header - an export that looks like it ran and
    /// produced nothing. Export correctness outranks export speed, so the
    /// encoder is tried on two frames before a real job is handed to it, and
    /// the answer is cached for the session.
    pub fn hardware_encoder_probe(&mut self, encoder: &str, device: &str) -> bool {
        self.hardware_encoder_works(encoder, device)
    }

    fn hardware_encoder_works(&mut self, encoder: &str, device: &str) -> bool {
        // Keyed by device as well as encoder: two cards in one machine do
        // not have the same entrypoints.
        let key = format!("{encoder}@{device}");
        if let Some(known) = self.accel.encoder_verified(&key) {
            return known;
        }
        let works = Self::trial_encode(encoder, device).unwrap_or(false);
        self.accel.mark_encoder(&key, works);
        works
    }

    fn trial_encode(encoder: &str, device: &str) -> Result<bool> {
        let path = std::env::temp_dir().join(format!(
            "davimci-probe-{encoder}-{}.mkv",
            device.replace('/', "_")
        ));
        let _ = std::fs::remove_file(&path);
        let output = path.to_string_lossy().to_string();
        let profile = Profile::new(320, 240, 25, 1)?;
        let mut source = Producer::new(&profile, "color", "#ff102030")?;
        source.set_in_and_out(0, 1);
        let mut consumer = Consumer::new(&profile, "avformat", Some(&output))?;
        {
            let mut props = consumer.properties();
            props.set_int("real_time", 0)?;
            props.set_int("terminate_on_pause", 1)?;
            props.set("vcodec", encoder)?;
            props.set("vaapi_device", device)?;
            props.set_int("mlt_hwupload", 1)?;
            props.set("an", "1")?;
        }
        consumer.connect(&source)?;
        consumer.start()?;
        // Two frames at 320x240 is milliseconds of work; the deadline only
        // exists so a wedged driver cannot hang the session.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !consumer.is_stopped() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        consumer.stop();
        // The file is the evidence, and only a file that decodes counts: a
        // driver without an encode entrypoint still leaves a few bytes of
        // container behind, so its size proves nothing. Reading a picture
        // back out proves the whole path.
        let playable = Producer::new(&profile, "avformat", &output)
            .ok()
            .filter(|p| p.length() > 0)
            .and_then(|mut p| p.next_frame().ok())
            .and_then(|mut f| f.rgba(320, 240).ok())
            .is_some();
        let _ = std::fs::remove_file(&path);
        Ok(playable)
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
        // The prefetcher's graph is a copy of the one that just changed, so
        // everything it has decoded or is decoding is a picture of a
        // timeline that no longer exists.
        self.prefetch = None;
        self.run_floor = None;
    }

    /// Take whatever the prefetcher has decoded into the frame cache.
    fn adopt_prefetched(&mut self, scale: PreviewScale) {
        let Some(prefetch) = self.prefetch.as_ref() else {
            return;
        };
        if prefetch.scale != scale {
            return;
        }
        let ready = prefetch.take_ready();
        for frame in ready {
            self.decodes = self.decodes.saturating_add(1);
            self.prefetched = self.prefetched.saturating_add(1);
            self.run_floor = Some(
                self.run_floor
                    .map_or(frame.position.get(), |f| f.min(frame.position.get())),
            );
            self.frames.insert(scale, frame);
        }
    }

    /// Keep a worker decoding the run below what is cached.
    ///
    /// Only while the transport is idle: during playback the picture comes
    /// from the preview consumer, and a second decoding graph would compete
    /// with it for the disk and the CPU it is pacing against.
    ///
    /// Exactly one run ahead. Asking for the run below the last one decoded
    /// unconditionally would advance the worker twelve frames for every one
    /// the user steps, so it would race the walk to frame 0, decode the
    /// whole timeline and evict the pictures the walk is about to ask for.
    fn prefetch_below(&mut self, frame: Frame, scale: PreviewScale) {
        if self.preview.is_some() {
            self.prefetch = None;
            return;
        }
        let Some(floor) = self.run_floor.filter(|f| *f > 0) else {
            return;
        };
        let run = self.backstep_run.max(1);
        // Not into the lowest cached run yet: what is already decoded is a
        // run's worth of pictures ahead of the walk, which is the whole
        // point.
        if frame.get() > floor.saturating_add(run) {
            return;
        }
        let end = Frame(floor.saturating_sub(1));
        let start = Frame(end.get().saturating_sub(run - 1));
        if self.prefetch.as_ref().is_none_or(|p| p.scale != scale) {
            let Some(projection) = self.projection.clone() else {
                return;
            };
            let Ok(graph) = self.build_graph(&projection) else {
                return;
            };
            self.prefetch = Some(Prefetch::spawn(
                graph,
                scale,
                scale.apply(self.props.resolution),
            ));
        }
        if let Some(prefetch) = self.prefetch.as_ref() {
            prefetch.request(start, end);
        }
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
                .set_track(mlt_int(i), &pl.as_producer())
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
        // Without a blend per visual track a tractor shows the topmost one
        // and drops what is under it, so a burned-in subtitle or an overlay
        // would replace the picture instead of sitting on it.
        let mut blends = Vec::new();
        for b in projection.video_blend_tracks() {
            let Some(blend) = video_blend(&self.profile) else {
                continue;
            };
            blend
                .properties()
                .set_int("always_active", 1)
                .map_err(BackendError::from)?;
            tractor
                .plant(&blend, 0, mlt_int(b))
                .map_err(BackendError::from)?;
            blends.push(blend);
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
                .plant(&mix, 0, mlt_int(b))
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
            _blends: blends,
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
                pl.append_blank(mlt_int(length.get()))
                    .map_err(BackendError::from)?;
                Ok(None)
            }
            Entry::Clip(c) => {
                let producer = self.build_producer(c)?;
                pl.append(
                    &producer,
                    mlt_int(c.in_point.get()),
                    mlt_int(c.out_point.get()),
                )
                .map_err(BackendError::from)?;
                Ok(None)
            }
            Entry::Transition(t) => {
                let (producer, nested) = self.build_transition(t)?;
                pl.append(&producer, 0, mlt_int(t.length().saturating_sub(1)))
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
    /// across them.
    fn build_transition(
        &self,
        entry: &crate::projection::TransitionEntry,
    ) -> Result<(Producer, Nested)> {
        // A track of a tractor is played whole, with no in/out of its own to
        // pass: unlike a playlist entry, there is nowhere else to say which
        // part of the source this is. Without this both tracks start at
        // source frame 0 and the overlap composites the wrong pictures.
        let mut from = self.build_producer(&entry.from)?;
        from.set_in_and_out(
            mlt_int(entry.from.in_point.get()),
            mlt_int(entry.from.out_point.get()),
        );
        let mut to = self.build_producer(&entry.to)?;
        to.set_in_and_out(
            mlt_int(entry.to.in_point.get()),
            mlt_int(entry.to.out_point.get()),
        );
        let mut tractor = Tractor::new().map_err(BackendError::from)?;
        tractor.set_track(0, &from).map_err(BackendError::from)?;
        tractor.set_track(1, &to).map_err(BackendError::from)?;
        let transition =
            Transition::new(&self.profile, &entry.service).map_err(BackendError::from)?;
        {
            let mut p = transition.properties();
            // The nested tractor *is* the overlap, so the transition spans
            // all of it, from its first frame to its last. This is also what
            // MLT computes the blend's progress from: left at the default
            // 0/0 it falls back to the b-track producer's own in/out, which
            // are source positions and have nothing to do with where the
            // overlap has got to.
            p.set_int("in", 0).map_err(BackendError::from)?;
            p.set_int("out", mlt_int(entry.length().saturating_sub(1)))
                .map_err(BackendError::from)?;
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

    /// The text producer to use, best first: builds without MLT's Qt module
    /// have no `qtext`, and a title card that silently degraded to a
    /// transparent card would export a picture with no text on it.
    ///
    /// `qtext` is demoted where Qt is unsafe to build (see [`qt_is_safe_here`])
    /// but never dropped, since a build with no `pango` would otherwise export
    /// a card with no text at all.
    fn text_services() -> &'static [&'static str] {
        if qt_is_safe_here() {
            &["qtext", "pango"]
        } else {
            &["pango", "qtext"]
        }
    }

    fn build_resource_producer(&self, entry: &ClipEntry) -> Option<Producer> {
        let resource = entry.resource.resource();
        if matches!(entry.resource, Resource::Text(_)) {
            return Self::text_services()
                .iter()
                .find_map(|s| Producer::new(&self.profile, s, &resource).ok());
        }
        Producer::new(&self.profile, entry.resource.service(), &resource).ok()
    }

    fn build_producer(&self, entry: &ClipEntry) -> Result<Producer> {
        // "Degrade locally": a source that will not open becomes a
        // placeholder rather than an unopenable project. A title card that
        // will not open degrades to nothing visible instead: an opaque
        // placeholder would hide the picture it sits on.
        let fallback = if matches!(entry.resource, Resource::Text(_)) {
            "#00000000"
        } else {
            "#ff202080"
        };
        let mut producer = match self.build_resource_producer(entry) {
            Some(p) => p,
            None => Producer::new(&self.profile, "color", fallback).map_err(BackendError::from)?,
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
                // The card behind the glyphs has to be transparent, or the
                // blend below composites an opaque rectangle over the video.
                let _ = props.set("bgcolour", "#00000000");
            }
            if matches!(entry.resource, Resource::File(_)) {
                self.request_hardware_decode(&mut props);
            }
            if let Resource::Offline { path } = &entry.resource {
                props
                    .set("davimci.offline", path)
                    .map_err(BackendError::from)?;
            }
            // One track per stream: a track that does not name its
            // stream decodes the container's default, so three audio tracks
            // off one file would all play the first stream.
            match entry.stream {
                Some(StreamSelect::Audio(s)) => {
                    props.set_int("audio_index", mlt_int(s))?;
                    props.set_int("video_index", -1)?;
                }
                Some(StreamSelect::Video(s)) => {
                    props.set_int("video_index", mlt_int(s))?;
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
                pl.remove(mlt_int(*index)).map_err(BackendError::from)?;
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
                    if a.same_producer(b));
                if resizable && let Entry::Clip(c) = entry {
                    pl.resize_clip(
                        mlt_int(*index),
                        mlt_int(c.in_point.get()),
                        mlt_int(c.out_point.get()),
                    )
                    .map_err(BackendError::from)?;
                } else {
                    pl.remove(mlt_int(*index)).map_err(BackendError::from)?;
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
            pl.insert_blank(mlt_int(index), mlt_int(length.get()));
            Ok(())
        }
        (Entry::Clip(c), Some(p)) => pl
            .insert(
                mlt_int(index),
                p,
                mlt_int(c.in_point.get()),
                mlt_int(c.out_point.get()),
            )
            .map_err(BackendError::from),
        (Entry::Transition(t), Some(p)) => pl
            .insert(mlt_int(index), p, 0, mlt_int(t.length().saturating_sub(1)))
            .map_err(BackendError::from),
        (Entry::Clip(_) | Entry::Transition(_), None) => Err(BackendError::Projection {
            reason: "a clip entry reached the graph without a producer".into(),
        }),
    }
}

/// Recover an exact rational framerate from MLT's decimal report.
///
/// NTSC rates are 1000/1001 of an integer, and a project has exactly one
/// framerate, so guessing a float here would poison every
/// conform downstream.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the rate is rounded and clamped to a sane framerate before the conversion"
)]
fn rational_fps(rate: f64) -> Option<Fps> {
    if rate <= 0.0 {
        return None;
    }
    // A framerate MLT reports is a small positive number, so rounding it and
    // clamping to the sane range makes each conversion below exact.
    let whole = |v: f64| v.clamp(0.0, 1_000_000.0) as u32;
    let nearest = rate.round();
    if (rate - nearest).abs() < 0.001 {
        return Fps::new(whole(nearest), 1).ok();
    }
    let ntsc = nearest * 1000.0 / 1001.0;
    if (rate - ntsc).abs() < 0.01 {
        return Fps::new(whole(nearest) * 1000, 1001).ok();
    }
    // Fall back to thousandths, still exact and still rational.
    Fps::new(whole((rate * 1000.0).round()), 1000).ok()
}

/// The alpha-aware video blend this build of MLT can offer.
///
/// The names are tried in the order kdenlive tries them: `cairoblend` is the
/// one that honours alpha everywhere, `qtblend` is the Qt build's, and
/// `composite` is in every MLT there has ever been. A build with none of
/// them keeps the topmost track, which is the old behaviour rather than a
/// broken graph.
///
/// The order is not thread-dependent, unlike the text producer's: `composite`
/// ignores alpha, so a burned-in subtitle drawn through it is invisible, and
/// a correct picture is worth building Qt for.
fn video_blend(profile: &Profile) -> Option<Transition> {
    ["frei0r.cairoblend", "qtblend", "composite"]
        .iter()
        .find_map(|service| Transition::new(profile, service).ok())
}

/// Whether a Qt-backed MLT service may be built here.
///
/// The first one built constructs the process `QApplication`, and a Qt built
/// off the main thread corrupts the heap when the process tears it down: it
/// aborted the render test binary with `malloc_consolidate(): unaligned
/// fastbin chunk` after every test in it had passed. Only the main thread is
/// known safe; a test harness thread never is.
fn qt_is_safe_here() -> bool {
    std::thread::current().name() == Some("main")
}

/// Decode `start..=target` from `graph` in one pass.
///
/// One seek then sequential decodes: seeking per frame is what makes a
/// backwards walk cost a whole GOP per picture. A frame short of the target
/// that will not decode ends the run; only the target itself is an error.
fn decode_run(
    graph: &mut Graph,
    start: Frame,
    target: Frame,
    res: Resolution,
) -> Result<Vec<VideoFrame>> {
    let count = target.get().saturating_sub(start.get()).saturating_add(1);
    let mut run = Vec::with_capacity(usize::try_from(count).unwrap_or(0));
    graph.root.seek(mlt_int(start.get()));
    for i in 0..count {
        let at = Frame(start.get().saturating_add(i));
        let decoded = match graph.root.next_frame() {
            Ok(mut pulled) => match pulled.rgba(res.width, res.height) {
                Ok((rgba, width, height)) => VideoFrame {
                    position: at,
                    width,
                    height,
                    rgba,
                },
                // One bad frame degrades to black, it does not end
                // the session.
                Err(_) => VideoFrame::black(at, res),
            },
            Err(_) if at != target => break,
            Err(_) => {
                return Err(BackendError::Seek {
                    frame: target.get(),
                });
            }
        };
        run.push(decoded);
    }
    Ok(run)
}

/// Cache a decoded run and hand back the frame that was asked for.
fn keep_run(
    cache: &mut FrameCache,
    decodes: &mut usize,
    scale: PreviewScale,
    target: Frame,
    run: Vec<VideoFrame>,
) -> Option<VideoFrame> {
    let mut wanted = None;
    for decoded in run {
        *decodes = decodes.saturating_add(1);
        if decoded.position == target {
            wanted = Some(decoded.clone());
        }
        cache.insert(scale, decoded);
    }
    wanted
}

/// `consumer-frame-show` listener: copies the shown frame's image out.
///
/// Runs on the consumer's own thread, so it catches unwinds: a panic must
/// never cross back into C (a hard rule of the FFI layer).
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
        if !shared.image {
            return;
        }
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
        let mut w = mlt_int(shared.width);
        let mut h = mlt_int(shared.height);
        // SAFETY: all out-parameters are initialised; MLT owns the buffer.
        let rc = unsafe {
            sys::mlt_frame_get_image(raw, &raw mut buf, &raw mut fmt, &raw mut w, &raw mut h, 0)
        };
        if rc != 0 || buf.is_null() || w <= 0 || h <= 0 {
            return;
        }
        // SAFETY: an RGBA buffer of w*h*4 bytes on success.
        let bytes = unsafe { std::slice::from_raw_parts(buf, count(w * h * 4)) }.to_vec();
        if let Ok(mut q) = shared.frames.lock() {
            // Bounded: a presenter that stops pulling must not grow the queue
            // without limit. Dropping the oldest keeps playback current.
            if q.len() >= 8 {
                q.pop_front();
            }
            q.push_back(VideoFrame {
                position: Frame(frames(position.max(0))),
                width: size(w),
                height: size(h),
                rgba: bytes,
            });
        }
    });
}

impl Drop for MltBackend {
    /// Quit is a teardown order, not a free order.
    ///
    /// Declaration order would close the profile first and the graph second,
    /// while the preview consumer's thread is still pulling from that graph
    /// and the scrub and prefetch workers are still decoding from graphs of
    /// their own. The consumer then never stops and the join at the end of
    /// the drop never returns: closing the window hangs.
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl MltBackend {
    /// Open the preview consumer at `from`, running at `rate`.
    ///
    /// A backwards pass is a different shape of preview, not a property of
    /// this one: the consumer plays audio only and the picture comes from a
    /// scrub graph of its own. MLT reads `video_off` once, when the consumer
    /// thread starts, so changing direction reopens the consumer.
    fn open_preview(&mut self, from: Frame, scale: PreviewScale, rate: f64) -> Result<()> {
        let reverse = rate < 0.0;
        let res = scale.apply(self.props.resolution);
        self.seek(from)?;
        // A graph of its own: seeking the one the consumer is pulling from
        // would move the sound with the picture.
        let scrub_graph = if reverse {
            let projection = self.projection.clone().ok_or(BackendError::Projection {
                reason: "no timeline has been given to the render backend yet".into(),
            })?;
            Some(self.build_graph(&projection)?)
        } else {
            None
        };
        let graph = self.require_graph()?;
        // Reaching the end of a producer leaves MLT with its speed at zero,
        // and a seek does not undo that. Without this, the *second* play
        // after playback once ran off the end reports "playing" and never
        // advances a frame.
        graph.root.set_speed(rate);
        let root = graph.root.clone_ref();

        // Audio-only consumers, in preference order: MLT must never own a
        // video window here. `sdl2_audio` leads because it is the only one
        // that honours `scrub_audio`, and so the only one that can be heard
        // while shuttling.
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
            // Without this the consumer queues audio only at exactly 1x, so
            // every shuttle - fast, slow or backwards - would be silent. The
            // consumer reads it once, when it starts, so it is set here
            // rather than per rate change.
            let _ = props.set_int("scrub_audio", 1);
            if reverse {
                // The consumer plays the sound and keeps the clock; the
                // picture is decoded from the scrub graph instead. Read once
                // at start, which is why a change of direction reopens the
                // consumer rather than setting it live.
                let _ = props.set_int("video_off", 1);
            }
        }
        let shared = Arc::new(PreviewShared {
            frames: Mutex::new(VecDeque::new()),
            width: res.width,
            height: res.height,
            image: !reverse,
        });
        // SAFETY: the pointer is an `Arc` this struct keeps alive for exactly
        // as long as the event handle, which is dropped before the consumer.
        let event = unsafe {
            consumer.listen_frame_show(Arc::as_ptr(&shared) as *mut c_void, Some(on_frame_show))
        }?;
        consumer.connect(&root)?;
        consumer.start()?;
        let scrub = scrub_graph.map(|g| Scrub::spawn(g, &shared, res, self.backstep_run));
        self.preview = Some(Preview {
            consumer,
            shared,
            _event: event,
            scale,
            scrub,
        });
        Ok(())
    }

    /// Where the preview has reached, for reopening it in the other
    /// direction.
    ///
    /// The consumer reports zero until it has played its first frame, and a
    /// reopen that believed it would restart every shuttle at the start of
    /// the timeline, so before the clock runs the graph's own position is
    /// the honest answer.
    fn preview_position(&self) -> Frame {
        self.audio_clock_position()
            .filter(|at| *at != Frame::ZERO)
            .or_else(|| {
                self.graph
                    .as_ref()
                    .map(|g| Frame(frames(g.root.position().max(0))))
            })
            .unwrap_or(Frame::ZERO)
    }
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
                        sample_rate = (r > 0).then_some(size(r));
                    }
                }
                Some("video") if resolution.is_none() => {
                    let w = props.get_int(&format!("meta.media.{i}.codec.width"));
                    let h = props.get_int(&format!("meta.media.{i}.codec.height"));
                    if w > 0 && h > 0 {
                        resolution = Some(Resolution {
                            width: size(w),
                            height: size(h),
                        });
                    }
                    // MLT reports the rate as a decimal; recover the exact
                    // rational rather than storing a float.
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
            frames: frames(producer.length().max(0)),
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

    /// Changing the policy rebuilds the graph, because `hwaccel` is read
    /// when a producer first decodes and the producers in a live graph have
    /// already done that. Cached stills were decoded under the old policy,
    /// so they go too.
    fn set_decode_policy(&mut self, policy: DecodePolicy) -> AccelerationStatus {
        if policy == self.accel.policy() {
            return self.accel.status();
        }
        let status = self.accel.set_policy(policy);
        self.hardware_producers.set(0);
        self.invalidate_frames();
        if let Some(projection) = self.projection.take() {
            // A rebuild that fails leaves the previous graph playing on the
            // previous policy, which is a slower session rather than a
            // broken one.
            let rebuilt = self.rebuild(&projection).is_ok();
            self.projection = Some(projection);
            if !rebuilt {
                return self
                    .accel
                    .record_failure("the render graph would not rebuild");
            }
        }
        status
    }

    fn acceleration(&self) -> AccelerationStatus {
        self.accel.status()
    }

    /// Takes effect on the next export: an encoder is chosen when the job
    /// starts, and a render already running keeps the one it opened with.
    fn set_encode_policy(&mut self, policy: davimci_backend::EncodePolicy) -> AccelerationStatus {
        self.accel.set_encode_policy(policy)
    }

    fn supports_planar(&self) -> bool {
        true
    }

    /// Pull the picture without converting it to RGBA.
    ///
    /// Deliberately not cached: the still cache holds the RGBA frames the
    /// presenter composes, and a planar pull is a host asking for the same
    /// picture in its own upload format. Caching both would double the
    /// memory for one picture.
    fn planar_frame_at(&mut self, frame: Frame, scale: PreviewScale) -> Result<PlanarFrame> {
        let res = scale.apply(self.props.resolution);
        let graph = self.require_graph()?;
        graph.root.seek(mlt_int(frame.get()));
        let mut pulled = graph
            .root
            .next_frame()
            .map_err(|_| BackendError::Seek { frame: frame.get() })?;
        let planes =
            pulled
                .yuv420p(res.width, res.height)
                .map_err(|_| BackendError::Unavailable {
                    reason: "this frame could not be decoded as planar YUV".into(),
                })?;
        Ok(PlanarFrame {
            position: frame,
            width: planes.width,
            height: planes.height,
            y: planes.y,
            u: planes.u,
            v: planes.v,
        })
    }

    fn seek(&mut self, frame: Frame) -> Result<()> {
        let graph = self.require_graph()?;
        graph.root.seek(mlt_int(frame.get()));
        Ok(())
    }

    fn frame_at(&mut self, frame: Frame, scale: PreviewScale) -> Result<VideoFrame> {
        let previous = self.last_request.replace(frame);
        let stepping_back = previous.is_some_and(|p| p > frame);
        self.pull(frame, scale, stepping_back)
    }

    /// A thumbnail leaves `last_request` alone: it is not the user moving,
    /// and letting it stand in for the playhead makes the next genuine
    /// backward step look like a forward one, which costs a GOP per frame.
    fn thumbnail_at(&mut self, frame: Frame, scale: PreviewScale) -> Result<VideoFrame> {
        self.pull(frame, scale, false)
    }

    fn preview_start(&mut self, from: Frame, scale: PreviewScale) -> Result<()> {
        if self.preview.is_some() {
            return Err(BackendError::PreviewAlreadyRunning);
        }
        self.open_preview(from, scale, 1.0)
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

    fn supports_reverse_varispeed(&self) -> bool {
        true
    }

    fn set_rate(&mut self, rate: f64) -> Result<()> {
        if !rate.is_finite() {
            return Err(BackendError::Unavailable {
                reason: "a playback rate must be a finite number".into(),
            });
        }
        // Forwards and backwards are two different previews, so crossing
        // between them reopens the consumer at wherever the clock has
        // reached rather than changing a property on the running one.
        if let Some(p) = self.preview.as_ref()
            && p.is_reverse() != (rate < 0.0)
        {
            let scale = p.scale;
            let at = self.preview_position();
            self.preview_stop()?;
            return self.open_preview(at, scale, rate);
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
        // Backwards, the queue is filled by the scrub worker rather than by
        // the consumer, and it is told where the clock has reached here -
        // the one place that is called every tick of the frontend's loop.
        let clock = self.audio_clock_position();
        if let (Some(p), Some(at)) = (self.preview.as_ref(), clock)
            && let Some(scrub) = p.scrub.as_ref()
        {
            scrub.track(at);
        }
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
        (pos >= 0).then_some(Frame(frames(pos)))
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
        let mut names: Vec<String> = crate::transitions::registered_names();
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
                    .map_or(0, |g| frames(g.root.length().max(0))),
            ),
        ));
        if end < start {
            return Err(BackendError::Render {
                reason: format!("the export range {start} to {end} runs backwards"),
            });
        }
        let output = job.output.to_string_lossy().to_string();

        // Resolved before anything is opened, so a refusal leaves no partial
        // file behind: export correctness outranks export speed, and a
        // preset that cannot be met is refused rather than substituted.
        let (video_codec, vaapi_device) = match self
            .accel
            .encoder_for(&job.settings.video_codec, job.settings.hardware)
        {
            crate::hwaccel::EncodeChoice::Software => (job.settings.video_codec.clone(), None),
            crate::hwaccel::EncodeChoice::Hardware { encoder, device } => {
                if self.hardware_encoder_works(&encoder, &device) {
                    (encoder, Some(device))
                } else if job.settings.hardware.required() {
                    return Err(BackendError::Render {
                        reason: Acceleration::encoder_refusal(&encoder),
                    });
                } else {
                    // Nothing was promised, so an encoder that does not work
                    // is a software export rather than a failed one.
                    (job.settings.video_codec.clone(), None)
                }
            }
            crate::hwaccel::EncodeChoice::Refused(reason) => {
                return Err(BackendError::Render { reason });
            }
        };

        // An export that keeps audio tracks separate renders from its own
        // graph: each audio track is routed onto its own channel pair before
        // the mix sums them, and the consumer cuts the bus into streams.
        let mut export_graph = None;
        let mut layout: Option<AudioLayout> = None;
        let needs_own_graph = job.settings.separate_audio_tracks || !job.settings.burn_subtitles;
        if needs_own_graph && let Some(mut projection) = self.projection.clone() {
            // Subtitles that are not burned in must not reach the picture:
            // the text tracks are dropped from the exported graph and
            // carried by the sidecar or the muxed stream instead.
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
        root.set_in_and_out(mlt_int(start.get()), mlt_int(end.get().saturating_sub(1)));
        root.seek(mlt_int(start.get()));

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
            props.set("vcodec", &video_codec)?;
            if let Some(device) = &vaapi_device {
                // MLT plants the `hwupload` filter itself once the consumer
                // has a VAAPI device: the frames it composites are in system
                // memory, so they have to be uploaded before a hardware
                // encoder can see them.
                props.set("vaapi_device", device)?;
                props.set_int("mlt_hwupload", 1)?;
            }
            props.set("acodec", &job.settings.audio_codec)?;
            props.set_int("width", mlt_int(job.settings.resolution.width))?;
            props.set_int("height", mlt_int(job.settings.resolution.height))?;
            props.set_int("frame_rate_num", mlt_int(job.settings.fps.num))?;
            props.set_int("frame_rate_den", mlt_int(job.settings.fps.den))?;
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
            let rendered = frames(r.consumer.position().max(0)).min(r.total);
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
mod tests {
    use super::{MltBackend, blend_cost, qt_is_safe_here};

    /// Regression: one cold decode measured a cost of the whole timeline, so
    /// the backwards shuttle aimed at frame zero, published it, and had
    /// nothing left below it - the preview showed a single picture and froze.
    #[test]
    fn a_cold_decode_cannot_aim_a_backwards_shuttle_at_frame_zero() {
        let reach = 48;
        assert!(
            blend_cost(1, 200, 1, reach) < 200,
            "a single slow decode set the aim to the whole clock travel"
        );
        assert!(blend_cost(1, 10_000, 1, reach) <= reach);
        assert_eq!(blend_cost(1, 0, 1, reach), 1, "a stalled clock costs one");
        assert_eq!(blend_cost(8, 8, 1, reach), 8, "a steady cost should hold");
    }

    fn off_main<T: Send + 'static>(f: fn() -> T) -> T {
        std::thread::spawn(f)
            .join()
            .expect("the probe thread should not panic")
    }

    /// Regression: preferring `qtext` everywhere built the `QApplication` on
    /// a libtest worker, and tearing that down off the main thread aborted
    /// the process with a corrupt heap once the tests had all passed.
    #[test]
    fn a_worker_thread_is_offered_pango_before_qtext() {
        assert!(!off_main(qt_is_safe_here), "a worker thread claimed Qt");
        assert_eq!(
            off_main(|| MltBackend::text_services().to_vec()).first(),
            Some(&"pango"),
            "a worker thread was offered the Qt text producer first"
        );
    }
}
