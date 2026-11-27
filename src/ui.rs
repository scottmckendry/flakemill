//! Read-only dashboard: shared run state, `/` page, and `/ui.json`.

use std::{
    ffi::CString,
    os::unix::ffi::OsStrExt,
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

/// Clears the dashboard's live-build marker when dropped. Obtained from
/// [`UiState::active_build_guard`]; each live build holds one for its duration,
/// so the marker always tracks the currently running build.
pub struct ActiveBuildGuard {
    inner: Arc<Mutex<Inner>>,
    revision: tokio::sync::watch::Sender<u64>,
}

impl Drop for ActiveBuildGuard {
    fn drop(&mut self) {
        // Tolerate a poisoned lock: dropping during unwind must never panic.
        if let Ok(mut inner) = self.inner.lock() {
            inner.active_build = None;
        }
        self.revision.send_modify(|r| *r += 1);
    }
}

#[derive(Clone)]
pub struct UiState {
    inner: Arc<Mutex<Inner>>,
    serve_dir: Arc<std::path::PathBuf>,
    started: Instant,
    /// Bumped on every state change so `/events` subscribers can push a fresh
    /// snapshot immediately instead of waiting for the next poll.
    revision: tokio::sync::watch::Sender<u64>,
}

impl UiState {
    pub fn new(serve_dir: &Path) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            serve_dir: Arc::new(serve_dir.to_path_buf()),
            started: Instant::now(),
            revision: tokio::sync::watch::channel(0).0,
        }
    }

    /// Watch channel of the state revision; changes whenever dashboard state
    /// changes. The current value is always available (initial snapshot).
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.revision.subscribe()
    }

    pub fn set_running(&self, running: bool) {
        self.inner.lock().unwrap().running = running;
        self.revision.send_modify(|r| *r += 1);
    }

    /// Mark `host`/`log` as the live build and return a guard that clears the
    /// marker when it goes out of scope. Clearing is guaranteed on every exit
    /// path from the build, including an early `?` return when `nix build`
    /// fails, so a stale live-build link cannot linger.
    #[must_use = "the guard clears the live-build marker when dropped; bind it \
                  to a name (e.g. `let _b = …`) to keep it alive for the build"]
    pub fn active_build_guard(&self, host: &str, log: &str) -> ActiveBuildGuard {
        self.inner.lock().unwrap().active_build = Some(ActiveBuild {
            host: host.into(),
            log: log.into(),
        });
        self.revision.send_modify(|r| *r += 1);
        ActiveBuildGuard {
            inner: Arc::clone(&self.inner),
            revision: self.revision.clone(),
        }
    }

    /// Whether `log` belongs to the build currently in progress.
    pub fn is_active_log(&self, log: &str) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.running && inner.active_build.as_ref().is_some_and(|b| b.log == log)
    }

    pub fn set_next_run_at(&self, at: u64) {
        self.inner.lock().unwrap().next_run_at = Some(at);
        self.revision.send_modify(|r| *r += 1);
    }
}

/// Capacity of the filesystem containing a path, not the size of the directory.
#[derive(Serialize)]
struct Storage {
    total_bytes: u64,
    available_bytes: u64,
    used_bytes: u64,
}

/// `statvfs` field widths are platform dependent (they are narrower than u64 on
/// 32-bit targets), so the casts stay even though clippy flags them as
/// redundant on this host target.
#[allow(clippy::unnecessary_cast)]
fn storage(path: &Path) -> Option<Storage> {
    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // statvfs initializes stats on success and does not retain either pointer.
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return None;
    }
    let stats = unsafe { stats.assume_init() };
    let block_size = stats.f_frsize as u64;
    Some(Storage {
        total_bytes: (stats.f_blocks as u64).saturating_mul(block_size),
        available_bytes: (stats.f_bavail as u64).saturating_mul(block_size),
        used_bytes: (stats.f_blocks as u64)
            .saturating_sub(stats.f_bfree as u64)
            .saturating_mul(block_size),
    })
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
    nix_storage: Option<Storage>,
    data_storage: Option<Storage>,
    data_path: &'a Path,
    /// Recent runs, oldest first (capped ring buffer from `history.json`).
    history: Vec<crate::history::RunRecord>,
}

pub fn ui_json(state: &UiState, cfg: &crate::config::Cfg) -> serde_json::Value {
    let (running, active_build, next_run_at) = {
        let inner = state.inner.lock().unwrap();
        (inner.running, inner.active_build.clone(), inner.next_run_at)
    };
    let manifest = crate::job::read_manifest(&state.serve_dir)
        .and_then(|manifest| serde_json::to_value(manifest).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let serving = state.serve_dir.join("flake.lock").exists();
    let signed = state.serve_dir.join("flake.lock.minisig").exists();
    serde_json::to_value(UiJson {
        manifest,
        serving,
        signed,
        running,
        active_build,
        next_run_at,
        repo_url: &cfg.repo_url,
        source_url: env!("CARGO_PKG_REPOSITORY"),
        version: env!("CARGO_PKG_VERSION"),
        branch: &cfg.branch,
        cachix_cache: &cfg.cache_name,
        interval_secs: cfg.interval.as_secs(),
        uptime_secs: state.started.elapsed().as_secs(),
        nix_storage: storage(Path::new("/nix/store")),
        data_storage: storage(&cfg.serve_dir),
        data_path: &cfg.serve_dir,
        history: crate::history::read(&state.serve_dir),
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
    use super::*;
    use crate::{
        job::{Manifest, Status},
        testing::{test_cfg, TempDir},
    };

    #[test]
    fn ui_json_reports_manifest_and_runtime_state() {
        let dir = TempDir::new("ui-json");
        let manifest = Manifest {
            status: Status::UpToDate,
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
        let active = state.active_build_guard("eris", "eris.log");
        state.set_next_run_at(123);
        let value = ui_json(&state, &test_cfg(&dir));

        assert_eq!(value["manifest"]["status"], "up-to-date");
        assert_eq!(value["manifest"]["revision"], "abc123");
        assert_eq!(value["serving"], true);
        assert_eq!(value["signed"], true);
        assert_eq!(value["running"], true);
        assert_eq!(value["active_build"]["host"], "eris");
        assert_eq!(value["active_build"]["log"], "eris.log");
        // The guard owns the live-build marker: dropping it clears the link.
        state.set_running(false);
        assert!(!ui_json(&state, &test_cfg(&dir))["active_build"].is_null());
        drop(active);
        assert!(ui_json(&state, &test_cfg(&dir))["active_build"].is_null());
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
    fn storage_reports_capacity_and_handles_missing_paths() {
        let dir = TempDir::new("ui-storage");
        let info = storage(&dir).unwrap();
        assert!(info.total_bytes > 0);
        assert!(info.used_bytes <= info.total_bytes);
        assert!(info.available_bytes <= info.total_bytes);
        assert!(storage(&dir.join("missing")).is_none());
        let value = ui_json(&UiState::new(&dir), &test_cfg(&dir));
        assert!(value["data_storage"]["total_bytes"].as_u64().unwrap() > 0);
        assert_eq!(value["data_path"], dir.to_str().unwrap());
    }

    #[test]
    fn state_changes_bump_the_revision_for_subscribers() {
        let dir = TempDir::new("ui-revision");
        let state = UiState::new(&dir);
        let mut rx = state.subscribe();
        assert!(!rx.has_changed().unwrap());

        state.set_running(true);
        assert!(rx.has_changed().unwrap());
        rx.borrow_and_update();

        let guard = state.active_build_guard("eris", "eris.log");
        assert!(rx.has_changed().unwrap());
        rx.borrow_and_update();

        drop(guard);
        assert!(rx.has_changed().unwrap());
        rx.borrow_and_update();

        state.set_next_run_at(42);
        assert!(rx.has_changed().unwrap());
    }

    #[test]
    fn ui_json_handles_a_fresh_install() {
        let dir = TempDir::new("ui-empty");
        let value = ui_json(&UiState::new(&dir), &test_cfg(&dir));

        assert_eq!(value["manifest"], serde_json::json!({}));
        assert_eq!(value["serving"], false);
        assert_eq!(value["signed"], false);
        assert_eq!(value["running"], false);
        assert_eq!(value["next_run_at"], serde_json::Value::Null);
        assert_eq!(value["history"], serde_json::json!([]));
    }

    #[test]
    fn ui_json_includes_persisted_run_history() {
        let dir = TempDir::new("ui-history");
        let manifest = Manifest {
            status: Status::Ok,
            revision: Some("abc123".into()),
            systems: vec!["eris".into()],
            toplevels: Vec::new(),
            hosts: Vec::new(),
            updated_at: "100".into(),
            error: None,
            failure: None,
        };
        crate::history::record(&dir, &manifest).unwrap();
        let value = ui_json(&UiState::new(&dir), &test_cfg(&dir));
        assert_eq!(value["history"][0]["at"], 100);
        assert_eq!(value["history"][0]["status"], "ok");
    }
}
