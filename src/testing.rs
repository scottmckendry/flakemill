//! Small helpers shared by unit tests.

use std::{
    ops::Deref,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::config::Cfg;

/// A `Cfg` suitable for tests, with `serve_dir` pointed at `serve_dir` so the
/// router and manifest helpers read/write the test's temp directory.
pub fn test_cfg(serve_dir: &Path) -> Cfg {
    Cfg {
        repo_url: "https://example.invalid/repo".into(),
        branch: "main".into(),
        work_dir: PathBuf::from("/tmp/work"),
        serve_dir: serve_dir.to_path_buf(),
        systems: vec!["eris".into()],
        cache_name: "cache".into(),
        sign_key: None,
        interval: Duration::from_secs(300),
    }
}

/// Test convenience over `tempfile::TempDir`: labels the directory with the
/// calling test's name for debuggable `ls /tmp` output, panics on creation
/// failure (correct in tests), and derefs to `Path` so call sites can
/// `dir.join(..)` and pass `&dir` where a `&Path` is expected. Auto-removed on
/// drop by the underlying `tempfile::TempDir`.
pub struct TempDir(tempfile::TempDir);

impl TempDir {
    pub fn new(label: &str) -> Self {
        let prefix = format!("flakemill-{label}-");
        let dir = tempfile::Builder::new()
            .prefix(&prefix)
            .tempdir()
            .unwrap_or_else(|e| panic!("create temp dir: {e}"));
        Self(dir)
    }
}

impl Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        self.0.path()
    }
}
