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
    pub active_build: Option<ActiveBuild>,
}

#[derive(Clone, Serialize)]
pub struct ActiveBuild {
    pub host: String,
    pub log: String,
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
        let mut inner = self.inner.lock().unwrap();
        inner.running = running;
        if !running {
            inner.active_build = None;
        }
    }

    pub fn set_active_build(&self, host: &str, log: &str) {
        self.inner.lock().unwrap().active_build = Some(ActiveBuild {
            host: host.into(),
            log: log.into(),
        });
    }

    pub fn clear_active_build(&self) {
        self.inner.lock().unwrap().active_build = None;
    }

    pub fn set_next_run_at(&self, at: u64) {
        self.inner.lock().unwrap().next_run_at = Some(at);
    }
}

#[derive(Serialize)]
struct UiJson<'a> {
    manifest: serde_json::Value,
    serving: bool,
    signed: bool,
    running: bool,
    active_build: Option<ActiveBuild>,
    next_run_at: Option<u64>,
    repo_url: &'a str,
    source_url: &'static str,
    version: &'static str,
    branch: &'a str,
    cachix_cache: &'a str,
    interval_secs: u64,
    uptime_secs: u64,
}

pub fn ui_json(state: &UiState, cfg: &crate::config::Cfg) -> serde_json::Value {
    let inner = state.inner.lock().unwrap();
    let manifest = crate::job::read_manifest(&state.serve_dir)
        .and_then(|manifest| serde_json::to_value(manifest).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let serving = state.serve_dir.join("flake.lock").exists();
    let signed = state.serve_dir.join("flake.lock.minisig").exists();
    serde_json::to_value(UiJson {
        manifest,
        serving,
        signed,
        running: inner.running,
        active_build: inner.active_build.clone(),
        next_run_at: inner.next_run_at,
        repo_url: &cfg.repo_url,
        source_url: env!("CARGO_PKG_REPOSITORY"),
        version: env!("CARGO_PKG_VERSION"),
        branch: &cfg.branch,
        cachix_cache: &cfg.cache_name,
        interval_secs: cfg.interval.as_secs(),
        uptime_secs: state.started.elapsed().as_secs(),
    })
    .expect("UiJson serialization cannot fail")
}

/// Embedded dashboard HTML for release builds.
const EMBEDDED_INDEX_HTML: &str = include_str!("ui/index.html");

/// In debug builds the dashboard is served from disk so HTML/CSS/JS edits
/// show up on browser refresh without recompiling. Release builds embed it.
pub fn index_html() -> String {
    #[cfg(debug_assertions)]
    {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/ui/index.html");
        if let Ok(html) = std::fs::read_to_string(path) {
            return html;
        }
    }
    EMBEDDED_INDEX_HTML.to_string()
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, time::Duration};

    use super::*;
    use crate::{config::Cfg, job::Manifest, test_support::TempDir};

    fn test_cfg() -> Cfg {
        Cfg {
            repo_url: "https://example.invalid/repo".into(),
            branch: "main".into(),
            work_dir: PathBuf::from("/tmp/work"),
            serve_dir: PathBuf::from("/tmp/serve"),
            systems: vec!["eris".into()],
            cache_name: "cache".into(),
            sign_key: None,
            interval: Duration::from_secs(300),
        }
    }

    #[test]
    fn ui_json_reports_manifest_and_runtime_state() {
        let dir = TempDir::new("ui-json");
        let manifest = Manifest {
            status: "up-to-date".into(),
            revision: Some("abc123".into()),
            systems: vec!["eris".into()],
            toplevels: vec!["/nix/store/abc-system".into()],
            hosts: Vec::new(),
            updated_at: "100".into(),
            error: None,
            failure: None,
        };
        crate::job::write_manifest(&dir, &manifest).unwrap();
        std::fs::write(dir.join("flake.lock"), b"{}").unwrap();
        std::fs::write(dir.join("flake.lock.minisig"), b"sig").unwrap();

        let state = UiState::new(&dir);
        state.set_running(true);
        state.set_active_build("eris", "eris.log");
        state.set_next_run_at(123);
        let value = ui_json(&state, &test_cfg());

        assert_eq!(value["manifest"]["status"], "up-to-date");
        assert_eq!(value["manifest"]["revision"], "abc123");
        assert_eq!(value["serving"], true);
        assert_eq!(value["signed"], true);
        assert_eq!(value["running"], true);
        assert_eq!(value["active_build"]["host"], "eris");
        assert_eq!(value["active_build"]["log"], "eris.log");
        state.set_running(false);
        assert!(ui_json(&state, &test_cfg())["active_build"].is_null());
        assert_eq!(value["next_run_at"], 123);
        assert_eq!(value["repo_url"], "https://example.invalid/repo");
        assert_eq!(
            value["source_url"],
            "https://github.com/scottmckendry/flakemill"
        );
        assert_eq!(value["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(value["branch"], "main");
        assert_eq!(value["cachix_cache"], "cache");
        assert_eq!(value["interval_secs"], 300);
        assert!(value["uptime_secs"].as_u64().is_some());
    }

    #[test]
    fn ui_json_handles_a_fresh_install() {
        let dir = TempDir::new("ui-empty");
        let value = ui_json(&UiState::new(&dir), &test_cfg());

        assert_eq!(value["manifest"], serde_json::json!({}));
        assert_eq!(value["serving"], false);
        assert_eq!(value["signed"], false);
        assert_eq!(value["running"], false);
        assert_eq!(value["next_run_at"], serde_json::Value::Null);
    }
}
