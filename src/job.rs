//! The update pipeline: sync, bump lock, build, push to cachix, publish.

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::cmd::{run, run_capture, run_probe, run_with_sink};
use crate::config::Cfg;

#[derive(Serialize, Deserialize, Default, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub status: String, // "ok" | "failed" | "up-to-date"
    pub revision: Option<String>,
    pub systems: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub toplevels: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<HostStat>,
    pub updated_at: String,
    pub error: Option<String>,
}

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
    std::fs::create_dir_all(serve_dir)
        .with_context(|| format!("creating {}", serve_dir.display()))?;
    let tmp = serve_dir.join("manifest.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(m)?)
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, serve_dir.join("manifest.json"))
        .with_context(|| format!("replacing {}", serve_dir.join("manifest.json").display()))?;
    Ok(())
}

/// Record a failed run while preserving the last-known-good revision and
/// host details for the dashboard.
pub fn record_failure(serve_dir: &Path, error: impl Into<String>) -> Result<()> {
    let prev = read_manifest(serve_dir);
    write_manifest(
        serve_dir,
        &Manifest {
            status: "failed".into(),
            revision: prev.as_ref().and_then(|p| p.revision.clone()),
            systems: prev.as_ref().map(|p| p.systems.clone()).unwrap_or_default(),
            toplevels: prev
                .as_ref()
                .map(|p| p.toplevels.clone())
                .unwrap_or_default(),
            hosts: prev.as_ref().map(|p| p.hosts.clone()).unwrap_or_default(),
            updated_at: unix_now().to_string(),
            error: Some(error.into()),
        },
    )
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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

pub fn run_job(cfg: &Cfg) -> Result<()> {
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
    run(
        "nix",
        &["flake", "update", "--flake", ".", "--refresh"],
        work,
    )?;

    // Compare the *updated* lock against the last published one. The committed
    // lock in the repo may be identical to what we published even when upstream
    // inputs have moved, so the check must run after the refresh, not before.
    if files_equal(&lock, &published_lock) {
        log::info!("lock unchanged after update; nothing to publish");
        // Eval-only (no builds): resolve each system's toplevel store path so
        // the dashboard's top-level column stays populated even when the lock
        // hasn't changed for a long time.
        let mut toplevels = Vec::new();
        for system in &cfg.systems {
            let attr = toplevel_attr(system);
            toplevels.push(
                run_capture("nix", &["eval", "--raw", &format!(".#{attr}")], work)
                    .map_err(|e| e.context(format!("evaluating toplevel for {system}")))?,
            );
        }
        write_manifest(
            &cfg.serve_dir,
            &Manifest {
                status: "up-to-date".into(),
                revision: Some(rev),
                systems: cfg.systems.clone(),
                toplevels,
                hosts: cfg
                    .systems
                    .iter()
                    .map(|s| {
                        prev.as_ref()
                            .and_then(|p| p.hosts.iter().find(|h| h.name == *s))
                            .cloned()
                            .unwrap_or(HostStat {
                                name: s.clone(),
                                action: HostAction::Cached,
                                log: None,
                                secs: 0,
                            })
                    })
                    .collect(),
                updated_at: unix_now().to_string(),
                error: None,
            },
        )?;
        return Ok(());
    }

    // 3. Build every system ---------------------------------------------------
    let logs_dir = cfg.serve_dir.join("logs");
    std::fs::create_dir_all(&logs_dir)?;

    let mut toplevels = Vec::new();
    let mut hosts = Vec::new();
    for system in &cfg.systems {
        let host_start = std::time::Instant::now();
        let attr = toplevel_attr(system);
        // Plan first: skip a system whose toplevel is already on cachix.
        let out = run_capture("nix", &["eval", "--raw", &format!(".#{attr}")], work)?;
        let log_name = format!("{system}.log");
        let log_path = logs_dir.join(&log_name);
        let cache_store = format!("https://{}.cachix.org", cfg.cache_name);
        let (cached, probe_err) =
            run_probe("nix", &["path-info", "--store", &cache_store, &out], work)?;
        // Anything other than a clean miss (network down, bad token, nix
        // itself broken) must not silently trigger a full rebuild.
        if !cached && !is_cache_miss(&probe_err) {
            bail!("cache probe for {system} failed: {probe_err}");
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
            .map_err(|e| {
                e.context(format!(
                    "building {system} (failed after {}s)",
                    host_start.elapsed().as_secs()
                ))
            })?;
            hosts.push(HostStat {
                name: system.clone(),
                action: HostAction::Built,
                log: Some(log_name),
                secs: host_start.elapsed().as_secs(),
            });
        }
        toplevels.push(out);
    }

    // 4. Push to cachix ---------------------------------------------------------
    let args: Vec<&str> = std::iter::once("push")
        .chain(std::iter::once(cfg.cache_name.as_str()))
        .chain(toplevels.iter().map(String::as_str))
        .collect();
    run("cachix", &args, work)?;

    // 5. Publish signed lock (atomic) --------------------------------------------
    let tmp = cfg.serve_dir.join("flake.lock.tmp");
    std::fs::copy(&lock, &tmp)
        .with_context(|| format!("copying {} to {}", lock.display(), tmp.display()))?;
    std::fs::rename(&tmp, &published_lock)
        .with_context(|| format!("replacing {}", published_lock.display()))?;
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
            status: "ok".into(),
            revision: Some(rev),
            systems: cfg.systems.clone(),
            toplevels,
            hosts,
            updated_at: unix_now().to_string(),
            error: None,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    fn sample_manifest() -> Manifest {
        Manifest {
            status: "ok".into(),
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
        first.status = "up-to-date".into();
        first.hosts.clear();
        write_manifest(&dir, &first).unwrap();

        assert_eq!(read_manifest(&dir), Some(first));
        assert!(!dir.join("manifest.json.tmp").exists());
    }

    #[test]
    fn record_failure_preserves_last_known_good_details() {
        let dir = TempDir::new("manifest-failure");
        let previous = sample_manifest();
        write_manifest(&dir, &previous).unwrap();

        record_failure(&dir, "boom").unwrap();
        let failed = read_manifest(&dir).unwrap();

        assert_eq!(failed.status, "failed");
        assert_eq!(failed.revision, previous.revision);
        assert_eq!(failed.systems, previous.systems);
        assert_eq!(failed.toplevels, previous.toplevels);
        assert_eq!(failed.hosts, previous.hosts);
        assert_eq!(failed.error.as_deref(), Some("boom"));
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
