//! Atomic file publication: write to a `tempfile`-created random file in the
//! destination's own directory, then `persist` (rename) it into place so a
//! reader never observes a half-written file. The temp must share the
//! destination's directory because `rename` is only atomic within a single
//! filesystem; `tempfile` gives each write a unique name, so concurrent
//! writers to the same destination never collide on a shared scratch file.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

/// Write `bytes` to `path`, atomically replacing any existing file.
pub fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("creating temp file in {}", parent.display()))?;
    tmp.write_all(bytes)
        .with_context(|| format!("writing {}", tmp.path().display()))?;
    tmp.persist(path)
        .map(drop)
        .with_context(|| format!("replacing {}", path.display()))
}

/// Copy `src` to `path`, atomically replacing any existing file.
pub fn copy_file(src: &Path, path: &Path) -> Result<()> {
    let bytes = std::fs::read(src).with_context(|| format!("reading {}", src.display()))?;
    write_bytes(path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;

    #[test]
    fn write_bytes_creates_parents_and_content() {
        let dir = TempDir::new("atomic");
        let dest = dir.join("nested").join("manifest.json");
        write_bytes(&dest, b"{\"ok\":true}").unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"{\"ok\":true}");
    }

    #[test]
    fn write_bytes_replaces_existing_contents() {
        let dir = TempDir::new("atomic");
        let dest = dir.join("flake.lock");
        write_bytes(&dest, b"first").unwrap();
        write_bytes(&dest, b"second").unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"second");
    }

    #[test]
    fn concurrent_writes_never_observe_a_partial_file() {
        // Unique scratch names (vs a fixed `.name.tmp`) mean a reader only ever
        // sees the complete old or complete new file, never a torn mix.
        let dir = TempDir::new("atomic");
        let dest = dir.join("lock");
        write_bytes(&dest, b"aaaaaaaa").unwrap();
        let threads: Vec<_> = (0..8u8)
            .map(|i| {
                let dest = dest.clone();
                std::thread::spawn(move || write_bytes(&dest, &[b'a' + i; 8]))
            })
            .collect();
        for t in threads {
            t.join().unwrap().unwrap();
        }
        let bytes = std::fs::read(&dest).unwrap();
        assert_eq!(bytes.len(), 8);
        assert!(bytes.windows(2).all(|w| w[0] == w[1]), "{bytes:?}");
    }

    #[test]
    fn copy_file_moves_source_bytes_to_destination() {
        let dir = TempDir::new("atomic");
        let src = dir.join("src");
        let dest = dir.join("sub").join("dest");
        std::fs::write(&src, b"payload").unwrap();
        copy_file(&src, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"payload");
    }

    #[test]
    fn copy_failure_returns_err_and_leaves_no_destination() {
        let dir = TempDir::new("atomic");
        let missing_src = dir.join("does-not-exist");
        let dest = dir.join("dest");
        assert!(copy_file(&missing_src, &dest).is_err());
        assert!(!dest.exists());
    }
}
