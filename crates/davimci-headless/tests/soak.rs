//! Soak fuzz: random key sequences against a fixture project.
//!
//! The properties are the three that must hold no matter what is typed: the
//! app never panics, the timeline's invariants never break, and undoing every
//! edit returns the timeline to exactly what it was. The last one is the
//! sharpest: it fails if any command's inverse is wrong, and it fails on the
//! whole random sequence rather than on a case somebody thought of.
//!
//! The generator is a seeded xorshift rather than a dependency, so a failure
//! reproduces from the seed printed with it.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "the scripted session works in small whole counts"
)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use davimci_app::{App, Event, NullHost};
use davimci_cmd::Session;
use davimci_core::testing::fixture;
use davimci_keys::Key;

/// Keys the fuzzer draws from: every default binding's first character, plus
/// counts, registers and the object/mark characters the grammar composes.
const ALPHABET: &[&str] = &[
    "h", "l", "j", "k", "w", "b", "e", "0", "$", "G", "gg", "{", "}", "%", "s", "gs", "x", "d",
    "gd", "y", "c", "p", "P", "gp", "gP", "u", "<C-r>", ".", "t", "gt", "T", "gT", "f", "+", "-",
    "v", "V", "o", "i", "a", "w", "ic", "ac", "it", "at", "is", "m", "`", "q", "@", "1", "2", "3",
    "\"", "z", "<Esc>", "<Space>", "<Left>", "<Right>",
];

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        let i = (self.next() % items.len() as u64) as usize;
        &items[i]
    }
}

fn session() -> Session {
    Session::new(fixture(&[
        (
            "V1",
            &[
                (0, 100, "a"),
                (100, 150, "b"),
                (250, 50, "c"),
                (400, 90, "d"),
            ],
        ),
        ("A1", &[(0, 300, "music"), (320, 120, "vo")]),
        ("T1", &[(50, 100, "line one")]),
    ]))
}

#[test]
fn random_keys_never_panic_and_always_undo_back_to_the_start() {
    // A fuzz run that edits nothing proves nothing, so the edits are counted
    // and the count is asserted at the end.
    let mut undone = 0usize;
    for seed in 1u64..=64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let start = session();
        let before = start.timeline().clone();
        let mut app = App::new(start);
        let mut host = NullHost;

        let mut typed = String::new();
        for _ in 0..200 {
            let chunk = rng.pick(ALPHABET);
            typed.push_str(chunk);
            for key in Key::parse_str(chunk) {
                app.key(key, &mut host);
            }
            app.session()
                .timeline()
                .check_invariants()
                .unwrap_or_else(|e| panic!("seed {seed} broke an invariant after `{typed}`: {e}"));
        }

        // Leave any mode the fuzzer wandered into, so undo is not swallowed
        // by a pending operator or a live selection.
        app.key(Key::parse_str("<Esc>")[0], &mut host);
        app.event(Event::Tick, &mut host);

        while app.session_mut().undo().is_ok() {
            undone += 1;
        }
        assert_eq!(
            app.session().timeline().dump(),
            before.dump(),
            "seed {seed}: undoing `{typed}` did not restore the timeline"
        );
        app.session().timeline().check_invariants().unwrap();
    }
    assert!(undone > 100, "the fuzzer only made {undone} edits");
}
