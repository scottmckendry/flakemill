//! The update pipeline: sync, bump lock, build, push to cachix, publish.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::cmd::{run, run_capture};
use crate::config::Cfg;

#[derive(Serialize, Default)]
pub struct Manifest {
    pub status: String, // "ok" | "failed" | "up-to-date"
    pub revision: Option<String>,
    pub systems: Vec<String>,
    pub updated_at: String,
    pub error: Option<String>,
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
    // 1. Sync ---------------------------------------------------------------
    let work = &cfg.work_dir;
    std::fs::create_dir_all(&cfg.serve_dir)?;
    if work.join(".git").exists() {
        run("git", &["fetch", "--prune", "origin"], work)?;
    } else {
        // git clone accepts an existing empty directory; create it up front
        // so current_dir(work) doesn't fail with ENOENT.
        std::fs::create_dir_all(work)?;
        run("git", &["clone", "--branch", &cfg.branch, &cfg.repo_url, "."], work)?;
    }
    run("git", &["reset", "--hard", &format!("origin/{}", cfg.branch)], work)?;
    let rev = run_capture("git", &["rev-parse", "HEAD"], work)?;

    // 2. Update lock ----------------------------------------------------------
    let lock = work.join("flake.lock");
    let published_lock = cfg.serve_dir.join("flake.lock");
    let unchanged = |a: &Path, b: &Path| {
        a.exists()
            && b.exists()
            && std::fs::read(a).is_ok_and(|x| std::fs::read(b).is_ok_and(|y| x == y))
    };
    if unchanged(&lock, &published_lock) {
        log::info!("lock unchanged since last publish; nothing to do");
        write_manifest(
            &cfg.serve_dir,
            &Manifest {
                status: "up-to-date".into(),
                revision: Some(rev),
                systems: cfg.systems.clone(),
                updated_at: iso_now(),
                error: None,
            },
        )?;
        return Ok(());
    }

    run("nix", &["flake", "update", "--flake", ".", "--refresh"], work)?;

    // 3. Build every system ---------------------------------------------------
    let mut toplevels = Vec::new();
    for system in &cfg.systems {
        let attr = format!("nixosConfigurations.{system}.config.system.build.toplevel");
        // Plan first: skip a system whose toplevel is already on cachix.
        let out = run_capture("nix", &["eval", "--raw", &format!(".#{attr}")], work)?;
        if run("nix", &["path-info", "--store", &format!("https://{}.cachix.org", cfg.cache_name), &out], work).is_ok() {
            log::info!("{system}: toplevel already cached, skipping build");
        } else {
            let attr_arg = format!(".{attr}");
            run(
                "nix",
                &["build", &attr_arg, "--no-link", "--print-build-logs"],
                work,
            )
            .map_err(|e| e.context(format!("building {system}")))?;
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
            &["-S", "-s", key.to_str().unwrap(), "-m", published_lock.to_str().unwrap()],
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
            updated_at: iso_now(),
            error: None,
        },
    )
}
