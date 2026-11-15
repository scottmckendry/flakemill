//! The update pipeline: sync, bump lock, build, push to cachix, publish.

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::cmd::{run, run_capture, run_probe, run_with_sink};
use crate::config::Cfg;

#[derive(Serialize, serde::Deserialize, Default)]
pub struct Manifest {
    pub status: String, // "ok" | "failed" | "up-to-date"
    pub revision: Option<String>,
    pub systems: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub toplevels: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<HostStat>,
    pub updated_at: String,
    pub error: Option<String>,
}

/// Per-host build outcome for the dashboard.
#[derive(Serialize, serde::Deserialize, Clone)]
pub struct HostStat {
    pub name: String,
    /// "built" | "cached" (toplevel already on cachix, build skipped)
    pub action: String,
    /// Filename (within /logs/) of the latest captured build output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
    /// Wall-clock seconds the host took (eval + plan + build), rounded.
    pub secs: u64,
}

pub fn write_manifest(serve_dir: &Path, m: &Manifest) -> Result<()> {
    let tmp = serve_dir.join("manifest.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(m)?)
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, serve_dir.join("manifest.json"))?;
    Ok(())
}

pub fn iso_now() -> String {
    // Unix epoch; swap for `time`/`chrono` when the manifest grows up.
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .to_string()
}

pub async fn run_job(cfg: &Cfg) -> Result<()> {
    // Previous manifest, if any. Used to keep the last-known toplevels/hosts
    // in the dashboard when a run is up-to-date or fails.
    let prev: Option<Manifest> = std::fs::read(cfg.serve_dir.join("manifest.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());

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
    let unchanged = |a: &Path, b: &Path| {
        a.exists()
            && b.exists()
            && std::fs::read(a).is_ok_and(|x| std::fs::read(b).is_ok_and(|y| x == y))
    };
    if unchanged(&lock, &published_lock) {
        log::info!("lock unchanged after update; nothing to publish");
        // Eval-only (no builds): resolve each system's toplevel store path so
        // the dashboard's top-level column stays populated even when the lock
        // hasn't changed for a long time.
        let mut toplevels = Vec::new();
        for system in &cfg.systems {
            let attr = format!("nixosConfigurations.{system}.config.system.build.toplevel");
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
                                action: "cached".into(),
                                log: None,
                                secs: 0,
                            })
                    })
                    .collect(),
                updated_at: iso_now(),
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
        let attr = format!("nixosConfigurations.{system}.config.system.build.toplevel");
        // Plan first: skip a system whose toplevel is already on cachix.
        let out = run_capture("nix", &["eval", "--raw", &format!(".#{attr}")], work)?;
        let log_name = format!("{system}.log");
        let log_path = logs_dir.join(&log_name);
        let cache_store = format!("https://{}.cachix.org", cfg.cache_name);
        let (cached, probe_err) =
            run_probe("nix", &["path-info", "--store", &cache_store, &out], work)?;
        // Anything other than a clean miss (network down, bad token, nix
        // itself broken) must not silently trigger a full rebuild.
        if !cached
            && !probe_err.contains("is not valid")
            && !probe_err.contains("cannot substitute")
        {
            bail!("cache probe for {system} failed: {probe_err}");
        }
        if cached {
            log::info!("{system}: toplevel already cached, skipping build");
            let _ = std::fs::write(
                &log_path,
                format!("# {system}: toplevel already on cachix, build skipped\n{out}\n"),
            );
            hosts.push(HostStat {
                name: system.clone(),
                action: "cached".into(),
                log: Some(log_name),
                secs: host_start.elapsed().as_secs(),
            });
        } else {
            let mut log_file = std::fs::File::create(&log_path).ok();
            let mut sink = |stream: &str, line: &str| {
                if let Some(f) = log_file.as_mut() {
                    use std::io::Write;
                    let _ = writeln!(f, "[{stream}] {line}");
                }
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
                action: "built".into(),
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
    std::fs::copy(&lock, &tmp)?;
    std::fs::rename(&tmp, &published_lock)?;
    if let Some(key) = &cfg.sign_key {
        run(
            "minisign",
            &[
                "-S",
                "-s",
                key.to_str().unwrap(),
                "-m",
                published_lock.to_str().unwrap(),
            ],
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
            updated_at: iso_now(),
            error: None,
        },
    )
}
