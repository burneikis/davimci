//! Listing a directory for the media picker (spec §3.2, `i`/`a`/`r`).
//!
//! This lives in `davimci-app` rather than in a frontend because both the GUI
//! and the TUI need exactly the same list in exactly the same order. A
//! frontend that sorted differently would be a parity failure waiting to
//! happen.
//!
//! It is the one place in the app crate that touches the filesystem, and it
//! only reads.

use std::path::{Path, PathBuf};

/// One row in the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowseEntry {
    pub path: PathBuf,
    /// What the user sees: the file name, or `..` for the parent.
    pub label: String,
    pub is_dir: bool,
}

/// File extensions the picker offers. Anything else is hidden, because
/// picking it could only end in a failed probe.
const MEDIA_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "mov", "webm", "avi", "m4v", "mts", "m2ts", "wav", "flac", "mp3", "aac", "ogg",
    "opus", "m4a", "png", "jpg", "jpeg", "webp",
];

/// True when the picker should offer this file.
#[must_use]
pub fn is_media(path: &Path) -> bool {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .is_some_and(|e| MEDIA_EXTENSIONS.contains(&e.as_str()))
}

/// List `dir` for the picker: the parent first, then directories, then media
/// files, each group sorted by name.
///
/// Unreadable directories produce an empty listing rather than an error: the
/// picker stays open and the user can navigate elsewhere (Phase 0,
/// recoverable errors degrade locally).
#[must_use]
pub fn list_dir(dir: &Path) -> Vec<BrowseEntry> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();

    if let Ok(read) = std::fs::read_dir(dir) {
        for entry in read.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            // Hidden files stay hidden, as in any file manager.
            if name.starts_with('.') {
                continue;
            }
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            if is_dir {
                dirs.push(BrowseEntry {
                    path,
                    label: name,
                    is_dir: true,
                });
            } else if is_media(&path) {
                files.push(BrowseEntry {
                    path,
                    label: name,
                    is_dir: false,
                });
            }
        }
    }

    dirs.sort_by(|a, b| a.label.cmp(&b.label));
    files.sort_by(|a, b| a.label.cmp(&b.label));

    let mut out = Vec::with_capacity(dirs.len() + files.len() + 1);
    if let Some(parent) = dir.parent() {
        out.push(BrowseEntry {
            path: parent.to_path_buf(),
            label: "..".to_string(),
            is_dir: true,
        });
    }
    out.append(&mut dirs);
    out.append(&mut files);
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn media_files_are_offered_and_others_are_not() {
        assert!(is_media(Path::new("/m/a.mkv")));
        assert!(is_media(Path::new("/m/A.MKV")), "extension is case-folded");
        assert!(is_media(Path::new("/m/a.wav")));
        // Picking these could only end in a failed probe.
        assert!(!is_media(Path::new("/m/notes.txt")));
        assert!(!is_media(Path::new("/m/a.davimci")));
        assert!(!is_media(Path::new("/m/noext")));
    }

    #[test]
    fn a_listing_is_parent_then_dirs_then_files_each_sorted() {
        let d = temp_dir("davimci-browse-order");
        std::fs::create_dir(d.join("zeta")).unwrap();
        std::fs::create_dir(d.join("alpha")).unwrap();
        std::fs::write(d.join("b.mkv"), b"").unwrap();
        std::fs::write(d.join("a.mkv"), b"").unwrap();
        std::fs::write(d.join("notes.txt"), b"").unwrap();
        std::fs::write(d.join(".hidden.mkv"), b"").unwrap();

        let labels: Vec<_> = list_dir(&d).into_iter().map(|e| e.label).collect();
        assert_eq!(labels, vec!["..", "alpha", "zeta", "a.mkv", "b.mkv"]);
    }

    #[test]
    fn an_unreadable_directory_lists_as_empty_rather_than_failing() {
        // The picker must stay open so the user can go somewhere else.
        let entries = list_dir(Path::new("/definitely/not/a/directory"));
        assert!(entries.iter().all(|e| e.label == ".."));
    }

    #[test]
    fn the_parent_entry_points_at_the_parent() {
        let d = temp_dir("davimci-browse-parent");
        let entries = list_dir(&d);
        let up = entries.first().expect("a parent entry");
        assert_eq!(up.label, "..");
        assert_eq!(up.path, d.parent().unwrap());
        assert!(up.is_dir);
    }
}
