//! Small helpers shared by unit tests.

use std::{
    ops::Deref,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use crate::config::Cfg;

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

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

pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(label: &str) -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("flakemill-{label}-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }
}

impl Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
