//! The analysis sidecar cache.
//!
//! Analysis is expensive and deterministic, so it is written to
//! `.davimci/cache/<content_hash>.analysis` next to the project and reused on
//! the next open. The hash is of the file's content, so a moved or renamed
//! source still hits and an edited one cannot.
//!
//! Every failure mode here is recoverable by recomputation: a missing entry,
//! an entry from an older [`ANALYSIS_VERSION`], a truncated file, or bytes
//! that are not JSON at all all read as "no cache". A corrupt cache must
//! never panic and must never be trusted.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::analysis::{ANALYSIS_VERSION, Analysis};
use crate::error::AnalysisError;
use crate::probe::StreamKind;

/// The cache key for one *stream*, not one file.
///
/// A container holds several streams that hash the same, so keying on content
/// alone makes every audio track imported from one file share the first
/// stream's measurement - identical envelopes under different audio.
#[must_use]
pub fn entry_key(content_hash: &str, stream: u32, kind: StreamKind) -> String {
    let kind = match kind {
        StreamKind::Video => 'v',
        StreamKind::Audio => 'a',
        StreamKind::Subtitle => 's',
    };
    format!("{content_hash}-{kind}{stream}")
}

/// FNV-1a over the file's contents plus its length.
///
/// Not cryptographic - it identifies media, it does not authenticate it -
/// but it is stable across machines and cheap on large files.
pub fn content_hash(path: &Path) -> Result<String, AnalysisError> {
    hash_file(path, None)
}

/// [`content_hash`], abandoned when the job is cancelled.
///
/// Hashing gigabytes takes seconds, and closing a project waits for the
/// thread doing it, so the read is checked between chunks.
pub fn hash_file(
    path: &Path,
    ctx: Option<&crate::jobs::JobContext>,
) -> Result<String, AnalysisError> {
    let name = path.display().to_string();
    let mut file = fs::File::open(path).map_err(|e| AnalysisError::io(&name, &e))?;
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut buf = vec![0u8; 64 * 1024];
    let mut len: u64 = 0;
    loop {
        if let Some(ctx) = ctx {
            ctx.check()?;
        }
        let n = file
            .read(&mut buf)
            .map_err(|e| AnalysisError::io(&name, &e))?;
        if n == 0 {
            break;
        }
        len += n as u64;
        for b in &buf[..n] {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
    }
    hash ^= len;
    hash = hash.wrapping_mul(0x100_0000_01b3);
    Ok(format!("{hash:016x}"))
}

/// A project's `.davimci/cache` directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisCache {
    root: PathBuf,
}

impl AnalysisCache {
    /// The cache belonging to a project directory.
    #[must_use]
    pub fn for_project(project_dir: &Path) -> Self {
        Self {
            root: project_dir.join(".davimci").join("cache"),
        }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn path_for(&self, hash: &str) -> PathBuf {
        self.root.join(format!("{hash}.analysis"))
    }

    /// Read a cached analysis, or `None` for any reason at all.
    ///
    /// There is deliberately no error case: every failure here means
    /// "recompute", and a caller that had to distinguish them would only
    /// recompute anyway.
    #[must_use]
    pub fn load(&self, hash: &str) -> Option<Analysis> {
        let text = fs::read_to_string(self.path_for(hash)).ok()?;
        let analysis: Analysis = serde_json::from_str(&text).ok()?;
        if analysis.version != ANALYSIS_VERSION || analysis.source_hash != hash {
            return None;
        }
        Some(analysis)
    }

    /// Write an analysis, stamping it with the current version.
    pub fn store(&self, hash: &str, analysis: &Analysis) -> Result<(), AnalysisError> {
        let mut stamped = analysis.clone();
        stamped.version = ANALYSIS_VERSION;
        stamped.source_hash = hash.to_string();
        fs::create_dir_all(&self.root).map_err(|e| AnalysisError::CacheUnwritable {
            reason: e.to_string(),
        })?;
        let text = serde_json::to_string(&stamped).map_err(|e| AnalysisError::CacheUnwritable {
            reason: e.to_string(),
        })?;
        // Write-then-rename: a crash mid-write leaves the old entry, not half
        // a new one.
        let tmp = self.path_for(hash).with_extension("tmp");
        fs::write(&tmp, text).map_err(|e| AnalysisError::CacheUnwritable {
            reason: e.to_string(),
        })?;
        fs::rename(&tmp, self.path_for(hash)).map_err(|e| AnalysisError::CacheUnwritable {
            reason: e.to_string(),
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::analysis::{AnalysisParams, analyze_samples};

    fn tmpdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("davimci-cache-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample() -> Analysis {
        analyze_samples(&vec![0.0; 4800], 48_000, AnalysisParams::default())
    }

    #[test]
    fn a_stored_analysis_reads_back_identically() {
        let dir = tmpdir("hit");
        let cache = AnalysisCache::for_project(&dir);
        cache.store("abc123", &sample()).unwrap();
        let back = cache.load("abc123").unwrap();
        assert_eq!(back.hops, sample().hops);
        assert_eq!(back.source_hash, "abc123");
        assert_eq!(back.version, ANALYSIS_VERSION);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn each_stream_of_one_file_gets_its_own_entry() {
        // Regression: keying on content alone gave every audio track imported
        // from one container the first stream's envelope.
        let h = "abc123";
        let a0 = entry_key(h, 0, StreamKind::Audio);
        let a1 = entry_key(h, 1, StreamKind::Audio);
        let v0 = entry_key(h, 0, StreamKind::Video);
        assert_ne!(a0, a1);
        assert_ne!(a0, v0);
        assert_eq!(a1, entry_key(h, 1, StreamKind::Audio), "keys are stable");
    }

    #[test]
    fn a_miss_is_none_not_an_error() {
        let dir = tmpdir("miss");
        assert!(AnalysisCache::for_project(&dir).load("nothing").is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_version_bump_invalidates_every_entry() {
        let dir = tmpdir("version");
        let cache = AnalysisCache::for_project(&dir);
        cache.store("abc123", &sample()).unwrap();
        // Simulate the next release bumping ANALYSIS_VERSION.
        let text = fs::read_to_string(cache.path_for("abc123")).unwrap();
        let bumped = text.replace(
            &format!("\"version\":{ANALYSIS_VERSION}"),
            &format!("\"version\":{}", ANALYSIS_VERSION + 1),
        );
        fs::write(cache.path_for("abc123"), bumped).unwrap();
        assert!(cache.load("abc123").is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_entry_recomputes_and_never_panics() {
        let dir = tmpdir("corrupt");
        let cache = AnalysisCache::for_project(&dir);
        cache.store("abc123", &sample()).unwrap();
        for junk in ["", "{", "null", "\u{0}\u{1}\u{2}", "{\"version\":1}"] {
            fs::write(cache.path_for("abc123"), junk).unwrap();
            assert!(cache.load("abc123").is_none(), "{junk:?} was trusted");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_entry_filed_under_another_hash_is_not_trusted() {
        let dir = tmpdir("mismatch");
        let cache = AnalysisCache::for_project(&dir);
        cache.store("abc123", &sample()).unwrap();
        let text = fs::read_to_string(cache.path_for("abc123")).unwrap();
        fs::write(cache.path_for("deadbeef"), text).unwrap();
        assert!(cache.load("deadbeef").is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_content_hash_follows_the_bytes_not_the_name() {
        let dir = tmpdir("hash");
        let a = dir.join("a.bin");
        let b = dir.join("b.bin");
        let c = dir.join("c.bin");
        fs::write(&a, b"hello world").unwrap();
        fs::write(&b, b"hello world").unwrap();
        fs::write(&c, b"hello worlD").unwrap();
        assert_eq!(content_hash(&a).unwrap(), content_hash(&b).unwrap());
        assert_ne!(content_hash(&a).unwrap(), content_hash(&c).unwrap());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn hashing_a_missing_file_is_offline_media() {
        use davimci_core::{Classify, ErrorClass};
        let err = content_hash(Path::new("/definitely/not/here.mkv")).unwrap_err();
        assert_eq!(err.class(), ErrorClass::OfflineMedia);
    }
}
