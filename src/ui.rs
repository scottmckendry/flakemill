//! Read-only dashboard: shared run state, `/` page, and `/ui.json`.

use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Instant,
};

use serde::Serialize;

/// Runtime info the scheduler loop feeds back into the dashboard.
#[derive(Default)]
pub struct Inner {
    pub running: bool,
    pub next_run_at: Option<u64>, // unix epoch secs
}

#[derive(Clone)]
pub struct UiState {
    inner: Arc<Mutex<Inner>>,
    serve_dir: Arc<std::path::PathBuf>,
    started: Instant,
}

impl UiState {
    pub fn new(serve_dir: &Path) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            serve_dir: Arc::new(serve_dir.to_path_buf()),
            started: Instant::now(),
        }
    }

    pub fn set_running(&self, running: bool) {
        self.inner.lock().unwrap().running = running;
    }

    pub fn set_next_run_at(&self, at: u64) {
        self.inner.lock().unwrap().next_run_at = Some(at);
    }
}

#[derive(Serialize)]
struct UiJson<'a> {
    manifest: serde_json::Value,
    serving: bool,
    running: bool,
    next_run_at: Option<u64>,
    repo_url: &'a str,
    branch: &'a str,
    cachix_cache: &'a str,
    interval_secs: u64,
    uptime_secs: u64,
}

pub fn ui_json(state: &UiState, cfg: &crate::config::Cfg) -> serde_json::Value {
    let inner = state.inner.lock().unwrap();
    let manifest = std::fs::read(state.serve_dir.join("manifest.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(serde_json::json!({}));
    let serving = state.serve_dir.join("flake.lock").exists();
    serde_json::to_value(UiJson {
        manifest,
        serving,
        running: inner.running,
        next_run_at: inner.next_run_at,
        repo_url: &cfg.repo_url,
        branch: &cfg.branch,
        cachix_cache: &cfg.cache_name,
        interval_secs: cfg.interval.as_secs(),
        uptime_secs: state.started.elapsed().as_secs(),
    })
    .unwrap_or(serde_json::json!({}))
}

pub fn index_html() -> &'static str {
    include_str!("ui/index.html")
}
