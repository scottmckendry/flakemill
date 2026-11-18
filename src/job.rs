//! The update pipeline: sync, bump lock, build, push to cachix, publish.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::atomic::{copy_file, write_bytes};
use crate::clock::{now_nanos, now_secs};
use crate::cmd::{run, run_capture, run_probe, run_with_sink};
use crate::config::Cfg;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// A full sync + update + build + publish run succeeded.
    Ok,
    /// The run failed; `Manifest::failure` may carry the partial outcome.
    Failed,
    /// The refreshed lock matched the published one, so nothing was built.
    UpToDate,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub status: Status,
    pub revision: Option<String>,
    pub systems: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub toplevels: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<HostStat>,
    pub updated_at: String,
    pub error: Option<String>,
    /// Outcomes from the failed attempt, separate from published host details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<RunFailure>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct RunFailure {
    pub host: String,
    pub stage: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
    pub completed: Vec<HostStat>,
    #[serde(default)]
    pub systems: Vec<String>,
}

impl std::fmt::Display for RunFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} for {}", self.stage, self.host)
    }
}

impl std::error::Error for RunFailure {}

/// Per-host build outcome for the dashboard.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct HostStat {
    pub name: String,
    pub action: HostAction,
    /// Filename (within /logs/) of the latest captured build output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
    /// Wall-clock seconds the host took (eval + plan + build), rounded.
    pub secs: u64,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HostAction {
    Built,
    Cached,
}

pub fn read_manifest(serve_dir: &Path) -> Option<Manifest> {
    let path = serve_dir.join("manifest.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            log::warn!("reading {}: {e}", path.display());
            return None;
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(manifest) => Some(manifest),
        Err(e) => {
            log::warn!("parsing {}: {e}", path.display());
            None
        }
    }
}

pub fn write_manifest(serve_dir: &Path, m: &Manifest) -> Result<()> {
    write_bytes(&serve_dir.join("manifest.json"), &serde_json::to_vec(m)?)
}

/// Record a failed run while preserving the last-known-good revision and
/// host details for the dashboard.
pub fn record_failure(
    serve_dir: &Path,
    error: impl Into<String>,
    failure: Option<RunFailure>,
) -> Result<()> {
    let prev = read_manifest(serve_dir);
    write_manifest(
        serve_dir,
        &Manifest {
            status: Status::Failed,
            revision: prev.as_ref().and_then(|p| p.revision.clone()),
            systems: prev.as_ref().map(|p| p.systems.clone()).unwrap_or_default(),
            toplevels: prev
                .as_ref()
                .map(|p| p.toplevels.clone())
                .unwrap_or_default(),
            hosts: prev.as_ref().map(|p| p.hosts.clone()).unwrap_or_default(),
            updated_at: now_secs().to_string(),
            error: Some(error.into()),
            failure,
        },
    )
}

fn files_equal(a: &Path, b: &Path) -> bool {
    match (std::fs::read(a), std::fs::read(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn toplevel_attr(system: &str) -> String {
    format!("nixosConfigurations.{system}.config.system.build.toplevel")
}

fn is_cache_miss(stderr: &str) -> bool {
    let stderr = stderr.to_ascii_lowercase();
    stderr.contains("is not valid") || stderr.contains("cannot substitute")
}

/// Create an empty generation directory for the indirect GC roots of one
/// publication candidate. Older generations stay live until the new one is
/// published; roots are registered under `/nix/var/nix/gcroots/auto`, so they
/// survive restarts and protect closures from concurrent GC.
fn new_generation(serve_dir: &Path) -> Result<std::path::PathBuf> {
    let roots = serve_dir.join("gc-roots");
    std::fs::create_dir_all(&roots)?;
    let roots = roots
        .canonicalize()
        .context("resolving GC roots directory")?;
    Ok(tempfile::Builder::new()
        .prefix("generation-")
        .tempdir_in(&roots)?
        .keep())
}

/// Root a store path inside `generation` at `index`. `nix build` on a valid
/// path is free: no rebuild, fetches only if missing locally.
fn root_one(work: &Path, generation: &Path, index: usize, path: &str) -> Result<()> {
    let link = generation.join(index.to_string());
    let link = link.to_str().context("GC root path is not valid UTF-8")?;
    run("nix", &["build", path, "--out-link", link], work)
        .with_context(|| format!("rooting {path}"))
}

/// Retire older generations after the new one is published, collecting
/// individual failures so one stuck directory doesn't block GC. Paths are
/// canonicalized so a symlinked `serve_dir` can't alias the live generation.
fn remove_old_generations(serve_dir: &Path, current: &Path) -> Result<()> {
    let roots = serve_dir.join("gc-roots").canonicalize()?;
    let current = current
        .canonicalize()
        .with_context(|| format!("resolving current generation {}", current.display()))?;
    let mut failed = Vec::new();
    for entry in std::fs::read_dir(&roots)? {
        let entry = entry?;
        let path = entry.path();
        if path.canonicalize().ok().as_deref() == Some(current.as_path()) {
            continue;
        }
        if path
            .file_name()
            .map(|n| n.to_string_lossy().starts_with("generation-"))
            .unwrap_or(false)
            && entry.file_type()?.is_dir()
            && std::fs::remove_dir_all(&path).is_err()
        {
            failed.push(path);
        }
    }
    if !failed.is_empty() {
        anyhow::bail!("could not remove {} stale generation(s)", failed.len());
    }
    Ok(())
}

fn collect_after_publish(serve_dir: &Path, work: &Path, current: &Path) {
    // Retiring is best-effort; stale roots are pruned by the GC anyway.
    if let Err(e) = remove_old_generations(serve_dir, current) {
        log::warn!("retiring old GC generations failed; GC will prune stale roots: {e:#}");
    }
    if let Err(e) = run("nix-store", &["--gc"], work) {
        log::warn!("Nix store garbage collection failed; will retry: {e:#}");
    }
}

pub fn run_job(cfg: &Cfg, ui: &crate::ui::UiState) -> Result<()> {
    // Previous manifest, if any. Used to keep the last-known toplevels/hosts
    // in the dashboard when a run is up-to-date or fails.
    let prev = read_manifest(&cfg.serve_dir);

    // 1. Sync ---------------------------------------------------------------
    let work = &cfg.work_dir;
    std::fs::create_dir_all(&cfg.serve_dir)?;
    if work.join(".git").exists() {
        run("git", &["fetch", "--prune", "origin"], work)?;
    } else {
        // git clone accepts an existing empty directory; create it up front
        // so current_dir(work) doesn't fail with ENOENT.
        std::fs::create_dir_all(work)?;
        run(
            "git",
            &["clone", "--branch", &cfg.branch, &cfg.repo_url, "."],
            work,
        )?;
    }
    run(
        "git",
        &["reset", "--hard", &format!("origin/{}", cfg.branch)],
        work,
    )?;
    let rev = run_capture("git", &["rev-parse", "HEAD"], work)?;

    // 2. Update lock ----------------------------------------------------------
    let lock = work.join("flake.lock");
    let published_lock = cfg.serve_dir.join("flake.lock");
    run("nix", &["flake", "update", "--refresh"], work)?;

    // Compare the *updated* lock against the last published one. The committed
    // lock in the repo may be identical to what we published even when upstream
    // inputs have moved, so the check must run after the refresh, not before.
    if files_equal(&lock, &published_lock) {
        log::info!("lock unchanged after update; nothing to publish");
        // Eval-only (no builds): resolve each system's toplevel store path so
        // the dashboard's top-level column stays populated even when the lock
        // hasn't changed for a long time.
        let mut toplevels = Vec::new();
        let generation = new_generation(&cfg.serve_dir)?;
        for (index, system) in cfg.systems.iter().enumerate() {
            let attr = toplevel_attr(system);
            let out = run_capture("nix", &["eval", "--raw", &format!(".#{attr}")], work)
                .map_err(|e| e.context(format!("evaluating toplevel for {system}")))?;
            root_one(work, &generation, index, &out)?;
            toplevels.push(out);
        }
        write_manifest(
            &cfg.serve_dir,
            &Manifest {
                status: Status::UpToDate,
                revision: Some(rev),
                systems: cfg.systems.clone(),
                toplevels,
                hosts: cfg
                    .systems
                    .iter()
                    .filter_map(|s| {
                        prev.as_ref()
                            .and_then(|p| p.hosts.iter().find(|h| h.name == *s))
                            .cloned()
                    })
                    .collect(),
                updated_at: now_secs().to_string(),
                error: None,
                failure: None,
            },
        )?;
        collect_after_publish(&cfg.serve_dir, work, &generation);
        return Ok(());
    }

    // 3. Build every system ---------------------------------------------------
    let logs_dir = cfg.serve_dir.join("logs");
    std::fs::create_dir_all(&logs_dir)?;
    let generation = new_generation(&cfg.serve_dir)?;

    let mut toplevels = Vec::new();
    let mut hosts = Vec::new();
    for system in &cfg.systems {
        let host_start = std::time::Instant::now();
        let attr = toplevel_attr(system);
        // Plan first: skip a system whose toplevel is already on cachix.
        let failure = |stage: &str, log: Option<String>| RunFailure {
            host: system.clone(),
            stage: stage.into(),
            log,
            completed: hosts.clone(),
            systems: cfg.systems.clone(),
        };
        let out = run_capture("nix", &["eval", "--raw", &format!(".#{attr}")], work)
            .map_err(|e| e.context(failure("evaluation", None)))?;
        // Do not overwrite logs referenced by the last published manifest.
        let log_name = format!("{}-{}.log", system, now_nanos());
        let log_path = logs_dir.join(&log_name);
        let cache_store = format!("https://{}.cachix.org", cfg.cache_name);
        let (cached, probe_err) =
            run_probe("nix", &["path-info", "--store", &cache_store, &out], work)
                .map_err(|e| e.context(failure("cache probe", None)))?;
        // Anything other than a clean miss (network down, bad token, nix
        // itself broken) must not silently trigger a full rebuild.
        if !cached && !is_cache_miss(&probe_err) {
            return Err(anyhow::anyhow!("{probe_err}").context(failure("cache probe", None)));
        }
        if cached {
            log::info!("{system}: toplevel already cached, skipping build");
            std::fs::write(
                &log_path,
                format!("# {system}: toplevel already on cachix, build skipped\n{out}\n"),
            )
            .with_context(|| format!("writing {}", log_path.display()))?;
            hosts.push(HostStat {
                name: system.clone(),
                action: HostAction::Cached,
                log: Some(log_name),
                secs: host_start.elapsed().as_secs(),
            });
        } else {
            let mut log_file = std::fs::File::create(&log_path)
                .with_context(|| format!("creating {}", log_path.display()))?;
            // The guard clears the live-build marker on scope exit, so the
            // early `?` return below (a failed build) can't leave a stale link.
            let _active_build = ui.active_build_guard(system, &log_name);
            let mut sink = |stream: &str, line: &str| {
                use std::io::Write;
                let _ = writeln!(log_file, "[{stream}] {line}");
            };
            // Every line has already been teed to the log file by the sink,
            // so an error here only needs the timing context re-attached.
            run_with_sink(
                "nix",
                &[
                    "build",
                    &format!(".#{attr}"),
                    "--no-link",
                    "--print-build-logs",
                ],
                work,
                Some(&mut sink),
            )
            .map_err(|e| e.context(failure("build", Some(log_name.clone()))))?;
            hosts.push(HostStat {
                name: system.clone(),
                action: HostAction::Built,
                log: Some(log_name),
                secs: host_start.elapsed().as_secs(),
            });
        }
        let index = toplevels.len();
        toplevels.push(out.clone());
        // Root now so concurrent GC can't collect it while later systems build.
        root_one(work, &generation, index, &out)?;
    }

    // 4. Push to cachix ---------------------------------------------------------
    let args: Vec<&str> = std::iter::once("push")
        .chain(std::iter::once(cfg.cache_name.as_str()))
        .chain(toplevels.iter().map(String::as_str))
        .collect();
    run("cachix", &args, work)?;

    // 5. Publish signed lock (atomic) -------------------------------------------
    copy_file(&lock, &published_lock)?;
    if let Some(key) = &cfg.sign_key {
        let key = key
            .to_str()
            .with_context(|| format!("SIGN_KEY path is not valid UTF-8: {}", key.display()))?;
        let published = published_lock.to_str().with_context(|| {
            format!(
                "published lock path is not valid UTF-8: {}",
                published_lock.display()
            )
        })?;
        run(
            "minisign",
            &["-S", "-s", key, "-m", published],
            &cfg.serve_dir,
        )?;
        log::info!("published signed flake.lock at rev {rev}");
    } else {
        log::warn!("SIGN_KEY unset — publishing UNSIGNED lock (PoC/dev only)");
    }

    write_manifest(
        &cfg.serve_dir,
        &Manifest {
            status: Status::Ok,
            revision: Some(rev),
            systems: cfg.systems.clone(),
            toplevels,
            hosts,
            updated_at: now_secs().to_string(),
            error: None,
            failure: None,
        },
    )?;
    collect_after_publish(&cfg.serve_dir, work, &generation);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;

    fn sample_manifest() -> Manifest {
        Manifest {
            status: Status::Ok,
            revision: Some("abc123".into()),
            systems: vec!["eris".into()],
            toplevels: vec!["/nix/store/abc-system".into()],
            hosts: vec![HostStat {
                name: "eris".into(),
                action: HostAction::Built,
                log: Some("eris.log".into()),
                secs: 42,
            }],
            updated_at: "100".into(),
            error: None,
            failure: None,
        }
    }

    #[test]
    fn manifest_round_trips_and_actions_serialize_lowercase() {
        let dir = TempDir::new("manifest-roundtrip");
        let manifest = sample_manifest();
        write_manifest(&dir, &manifest).unwrap();

        let json = std::fs::read_to_string(dir.join("manifest.json")).unwrap();
        assert!(json.contains("\"action\":\"built\""), "{json}");
        assert_eq!(read_manifest(&dir), Some(manifest));
    }

    #[test]
    fn manifest_tolerates_skipped_empty_vectors() {
        let manifest: Manifest = serde_json::from_str(
            r#"{"status":"ok","revision":null,"systems":[],"updated_at":"1","error":null}"#,
        )
        .unwrap();
        assert!(manifest.toplevels.is_empty());
        assert!(manifest.hosts.is_empty());
    }

    #[test]
    fn write_manifest_replaces_previous_atomically() {
        let dir = TempDir::new("manifest-replace");
        let mut first = sample_manifest();
        write_manifest(&dir, &first).unwrap();
        first.status = Status::UpToDate;
        first.hosts.clear();
        write_manifest(&dir, &first).unwrap();

        assert_eq!(read_manifest(&dir), Some(first));
        let leftovers: Vec<_> = std::fs::read_dir(&*dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            leftovers,
            vec!["manifest.json".to_string()],
            "{leftovers:?}"
        );
    }

    #[test]
    fn record_failure_preserves_last_known_good_details() {
        let dir = TempDir::new("manifest-failure");
        let previous = sample_manifest();
        write_manifest(&dir, &previous).unwrap();

        record_failure(&dir, "boom", None).unwrap();
        let failed = read_manifest(&dir).unwrap();

        assert_eq!(failed.status, Status::Failed);
        assert_eq!(failed.revision, previous.revision);
        assert_eq!(failed.systems, previous.systems);
        assert_eq!(failed.toplevels, previous.toplevels);
        assert_eq!(failed.hosts, previous.hosts);
        assert_eq!(failed.error.as_deref(), Some("boom"));
    }

    #[test]
    fn failed_attempt_preserves_published_data_and_tracks_failure() {
        let dir = TempDir::new("manifest-host-failure");
        let previous = sample_manifest();
        write_manifest(&dir, &previous).unwrap();
        let failure = RunFailure {
            host: "eris".into(),
            stage: "build".into(),
            log: Some("eris-attempt.log".into()),
            completed: vec![],
            systems: vec!["eris".into(), "other".into()],
        };
        record_failure(&dir, "build failed", Some(failure.clone())).unwrap();
        let result = read_manifest(&dir).unwrap();
        assert_eq!(result.hosts, previous.hosts);
        assert_eq!(result.toplevels, previous.toplevels);
        assert_eq!(result.failure, Some(failure));
    }

    #[test]
    fn missing_or_unreadable_manifest_is_none() {
        let dir = TempDir::new("manifest-missing");
        assert_eq!(read_manifest(&dir), None);

        std::fs::write(dir.join("manifest.json"), b"not json").unwrap();
        assert_eq!(read_manifest(&dir), None);
    }

    #[test]
    fn files_equal_requires_both_files_and_matching_contents() {
        let dir = TempDir::new("files-equal");
        let a = dir.join("a");
        let b = dir.join("b");
        std::fs::write(&a, b"same").unwrap();
        std::fs::write(&b, b"same").unwrap();
        assert!(files_equal(&a, &b));

        std::fs::write(&b, b"different").unwrap();
        assert!(!files_equal(&a, &b));
        assert!(!files_equal(&a, &dir.join("missing")));
    }

    #[test]
    fn cleanup_preserves_current_generation_and_unrelated_files() {
        let dir = TempDir::new("gc-roots");
        let roots = dir.join("gc-roots");
        let current = roots.join("generation-current");
        let old = roots.join("generation-old");
        std::fs::create_dir_all(&current).unwrap();
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("root"), "old").unwrap();
        std::fs::write(roots.join("unrelated"), "keep").unwrap();
        remove_old_generations(&dir, &current).unwrap();
        assert!(current.exists());
        assert!(!old.exists());
        assert!(roots.join("unrelated").exists());
    }

    #[test]
    fn cache_misses_are_recognised() {
        assert!(is_cache_miss("error: path '/nix/store/x' is not valid"));
        assert!(is_cache_miss("cannot substitute path"));
        assert!(!is_cache_miss("SSL certificate problem"));
    }

    #[test]
    fn toplevel_attribute_is_built_from_system_name() {
        assert_eq!(
            toplevel_attr("eris"),
            "nixosConfigurations.eris.config.system.build.toplevel"
        );
    }
}
