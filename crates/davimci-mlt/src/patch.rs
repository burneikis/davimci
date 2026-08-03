//! Incremental projection diffing.
//!
//! Split and ripple are playlist mutations, not re-renders (spec 10.1), so
//! the backend asks this module what changed rather than rebuilding the
//! graph. The diff is pure data and is verified by a property test: applying
//! the ops to the old entry list must reproduce the new one exactly, which is
//! the only guarantee that lets the backend trust a patch over a rebuild.

use davimci_core::ClipId;

use crate::projection::{Entry, Projection};

/// One mutation of one playlist. Indices are into the playlist *as patched so
/// far*, so ops must be applied in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackOp {
    Insert {
        index: usize,
        entry: Entry,
    },
    Remove {
        index: usize,
    },
    /// Same identity, different geometry or filters.
    Update {
        index: usize,
        entry: Entry,
    },
}

/// Ops for one track, identified by its position in the tractor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackPatch {
    pub track_index: usize,
    pub ops: Vec<TrackOp>,
}

/// What the backend must do to catch the graph up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Patch {
    /// Nothing changed; do not touch the graph.
    None,
    /// The playlist set or the profile changed - patching cannot express it.
    Rebuild,
    /// Per-playlist mutations.
    Tracks(Vec<TrackPatch>),
}

/// Entry identity, which is what the diff aligns on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Blank,
    Clip(ClipId),
    /// A transition is identified by the clip it comes *into*, and must not
    /// be confused with that clip's own entry: they sit next to each other in
    /// the playlist and share an id (spec 6.2).
    Transition(ClipId),
}

fn key(e: &Entry) -> Key {
    match (e.clip_id(), e.is_transition()) {
        (Some(id), true) => Key::Transition(id),
        (Some(id), false) => Key::Clip(id),
        (None, _) => Key::Blank,
    }
}

/// Diff two projections of the same timeline.
#[must_use]
pub fn diff(old: &Projection, new: &Projection) -> Patch {
    if !old.same_shape(new) {
        return Patch::Rebuild;
    }
    let mut patches = Vec::new();
    for (i, (o, n)) in old.tracks.iter().zip(&new.tracks).enumerate() {
        // Mute is a tractor `hide` change, not a playlist edit, and there is
        // no op for it: the caller must rebuild the track flags. Treat it as
        // a rebuild so the graph can never disagree with the model.
        if o.muted != n.muted {
            return Patch::Rebuild;
        }
        let ops = diff_entries(&o.entries, &n.entries);
        if !ops.is_empty() {
            patches.push(TrackPatch {
                track_index: i,
                ops,
            });
        }
    }
    if patches.is_empty() {
        Patch::None
    } else {
        Patch::Tracks(patches)
    }
}

/// Greedy alignment on entry identity.
///
/// Clip ids are stable across every edit (a command never mints an id the log
/// does not record), so aligning on them turns a ripple delete into removes
/// and a split into one update plus one insert, rather than a wholesale
/// replacement of the tail.
fn diff_entries(old: &[Entry], new: &[Entry]) -> Vec<TrackOp> {
    let mut cur: Vec<Entry> = old.to_vec();
    let mut ops = Vec::new();
    let mut i = 0;
    while i < new.len() {
        if i < cur.len() && key(&cur[i]) == key(&new[i]) {
            if cur[i] != new[i] {
                ops.push(TrackOp::Update {
                    index: i,
                    entry: new[i].clone(),
                });
                cur[i] = new[i].clone();
            }
            i += 1;
            continue;
        }
        // Does this entry exist further along? Then everything before it went
        // away; otherwise it is genuinely new.
        let found = match key(&new[i]) {
            Key::Blank => None,
            k => cur[i.min(cur.len())..]
                .iter()
                .position(|e| key(e) == k)
                .map(|p| p + i),
        };
        match found {
            Some(j) => {
                for _ in i..j {
                    ops.push(TrackOp::Remove { index: i });
                    cur.remove(i);
                }
            }
            None => {
                ops.push(TrackOp::Insert {
                    index: i,
                    entry: new[i].clone(),
                });
                cur.insert(i, new[i].clone());
            }
        }
    }
    while cur.len() > new.len() {
        ops.push(TrackOp::Remove { index: new.len() });
        cur.remove(new.len());
    }
    ops
}

/// Apply ops to an entry list. The backend does the same thing to a live
/// playlist; this is the reference the property test checks it against.
pub fn apply_ops(entries: &mut Vec<Entry>, ops: &[TrackOp]) {
    for op in ops {
        match op {
            TrackOp::Insert { index, entry } => {
                entries.insert((*index).min(entries.len()), entry.clone())
            }
            TrackOp::Remove { index } => {
                if *index < entries.len() {
                    entries.remove(*index);
                }
            }
            TrackOp::Update { index, entry } => {
                if let Some(slot) = entries.get_mut(*index) {
                    *slot = entry.clone();
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use davimci_core::testing::{clip_ids, fixture, track_id};
    use davimci_core::{Frame, Timeline, TimelineProps};
    use proptest::prelude::*;

    fn proj(tl: &Timeline) -> Projection {
        Projection::of(tl)
    }

    #[test]
    fn an_unchanged_timeline_produces_no_ops() {
        let tl = fixture(&[("V1", &[(0, 100, "a")])]);
        assert_eq!(diff(&proj(&tl), &proj(&tl)), Patch::None);
    }

    #[test]
    fn a_split_updates_one_entry_and_inserts_one() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a")])]);
        let before = proj(&tl);
        let v1 = track_id(&tl, "V1");
        tl.split_at(v1, Frame(40)).unwrap();
        let after = proj(&tl);
        let Patch::Tracks(patches) = diff(&before, &after) else {
            panic!("expected playlist ops, not a rebuild");
        };
        assert_eq!(patches.len(), 1);
        assert!(matches!(
            patches[0].ops[0],
            TrackOp::Update { index: 0, .. }
        ));
        assert!(matches!(
            patches[0].ops[1],
            TrackOp::Insert { index: 1, .. }
        ));
    }

    #[test]
    fn a_ripple_delete_removes_rather_than_rebuilds() {
        let mut tl = fixture(&[("V1", &[(0, 50, "a"), (50, 50, "b"), (100, 50, "c")])]);
        let before = proj(&tl);
        let v1 = track_id(&tl, "V1");
        let b = clip_ids(&tl, "V1")[1];
        tl.ripple_delete_clip(v1, b).unwrap();
        let Patch::Tracks(patches) = diff(&before, &proj(&tl)) else {
            panic!("expected playlist ops");
        };
        // `b` leaves; `c` keeps its identity and only its position changes,
        // which a playlist expresses by the removal alone.
        assert_eq!(patches[0].ops, vec![TrackOp::Remove { index: 1 }]);
    }

    /// Spec 6.2 / plan.md Phase 9f: the overlap is its own playlist entry,
    /// so planting one is an insert next to two resizes - not a rebuild - and
    /// rippling a neighbour away removes it rather than orphaning it.
    #[test]
    fn a_transition_patches_in_and_out_without_a_rebuild() {
        let mut tl = davimci_core::testing::media_fixture(&[
            (0, 100, 20, 400),
            (100, 100, 20, 400),
            (200, 100, 20, 400),
        ]);
        let track = track_id(&tl, "V1");
        let ids = clip_ids(&tl, "V1");
        let before = proj(&tl);
        tl.set_transition(track, ids[1], Some(davimci_core::Transition::dissolve()))
            .unwrap();
        let after = proj(&tl);
        let Patch::Tracks(patches) = diff(&before, &after) else {
            panic!("expected playlist ops, not a rebuild");
        };
        let mut entries = before.tracks[0].entries.clone();
        apply_ops(&mut entries, &patches[0].ops);
        assert_eq!(entries, after.tracks[0].entries);
        assert!(after.tracks[0].entries.iter().any(Entry::is_transition));

        // Rippling away the outgoing clip takes the overlap with it.
        let before = after;
        tl.ripple_delete_clip(track, ids[0]).unwrap();
        let after = proj(&tl);
        assert!(
            !after.tracks[0].entries.iter().any(Entry::is_transition),
            "the cut is gone, so the overlap is too"
        );
        if let Patch::Tracks(patches) = diff(&before, &after) {
            let mut entries = before.tracks[0].entries.clone();
            apply_ops(&mut entries, &patches[0].ops);
            assert_eq!(entries, after.tracks[0].entries);
        }
    }

    #[test]
    fn adding_a_track_forces_a_rebuild() {
        let tl = fixture(&[("V1", &[(0, 10, "a")])]);
        let before = proj(&tl);
        let mut tl2 = tl.clone();
        tl2.add_track(davimci_core::TrackKind::Audio);
        assert_eq!(diff(&before, &proj(&tl2)), Patch::Rebuild);
    }

    #[test]
    fn changing_the_profile_forces_a_rebuild() {
        let tl = fixture(&[("V1", &[(0, 10, "a")])]);
        let before = proj(&tl);
        let mut tl2 = tl.clone();
        tl2.props = TimelineProps {
            fps: davimci_core::Fps::FPS_25,
            ..tl.props
        };
        assert_eq!(diff(&before, &proj(&tl2)), Patch::Rebuild);
    }

    #[test]
    fn muting_a_track_forces_a_rebuild_because_hide_is_not_a_playlist_op() {
        let tl = fixture(&[("A1", &[(0, 10, "a")])]);
        let before = proj(&tl);
        let mut tl2 = tl.clone();
        let a1 = track_id(&tl2, "A1");
        tl2.set_track_muted(a1, true).unwrap();
        assert_eq!(diff(&before, &proj(&tl2)), Patch::Rebuild);
    }

    proptest! {
        /// The load-bearing guarantee: a patch is never a different result
        /// from a rebuild.
        #[test]
        fn patching_reproduces_the_rebuilt_playlist(
            edits in prop::collection::vec(0u8..6, 0..12)
        ) {
            let mut tl = fixture(&[("V1", &[(0, 40, "a"), (40, 40, "b"), (120, 40, "c")])]);
            let v1 = track_id(&tl, "V1");
            let mut before = proj(&tl);
            for e in edits {
                let ids = clip_ids(&tl, "V1");
                match e {
                    0 => { let _ = tl.split_at(v1, Frame(20)); }
                    1 => { let _ = tl.split_at(v1, Frame(60)); }
                    2 => { if let Some(id) = ids.first() { let _ = tl.ripple_delete_clip(v1, *id); } }
                    3 => { if let Some(id) = ids.last() { let _ = tl.lift_clip(v1, *id); } }
                    4 => { if let Some(id) = ids.get(1) { let _ = tl.lift_clip(v1, *id); } }
                    _ => { let _ = tl.split_at(v1, Frame(130)); }
                }
                let after = proj(&tl);
                let patch = diff(&before, &after);
                if let Patch::Tracks(patches) = &patch {
                    let mut entries = before.tracks[0].entries.clone();
                    for p in patches {
                        prop_assert_eq!(p.track_index, 0);
                        apply_ops(&mut entries, &p.ops);
                    }
                    prop_assert_eq!(entries, after.tracks[0].entries.clone());
                } else {
                    prop_assert!(matches!(patch, Patch::None | Patch::Rebuild));
                }
                before = after;
            }
        }
    }
}
