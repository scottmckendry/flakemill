//! flakemill
//!
//! Pulls origin/main, updates flake.lock, builds all systems, pushes to
//! cachix, then serves a *signed* flake.lock over HTTP. Never pushes to
//! the source repo — hosts fetch and apply as they please.

mod cmd;
mod config;
mod job;
mod server;
mod ui;

use anyhow::Result;

use config::load_cfg;
use job::{iso_now, run_job, write_manifest, Manifest};

#[tokio::main]
async fn main() -> Result<()> {
    use tracing_subscriber::EnvFilter;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,tower_http=debug")),
        )
        .init();
    let cfg = load_cfg();
    std::fs::create_dir_all(&cfg.serve_dir)?;

    let addr = std::env::var("LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let ui = ui::UiState::new(&cfg.serve_dir);
    let app = server::build_router(&cfg.serve_dir, &cfg, ui.clone());

    tokio::spawn({
        let cfg = cfg.clone();
        async move {
            loop {
                ui.set_running(true);
                if let Err(e) = run_job(&cfg).await {
                    ui.set_running(false);
                    log::error!("update job failed: {e:#}");
                    // Keep the previous manifest's toplevels/hosts so the
                    // dashboard shows last-known-good info alongside the error.
                    let prev: Option<crate::job::Manifest> =
                        std::fs::read(cfg.serve_dir.join("manifest.json"))
                            .ok()
                            .and_then(|b| serde_json::from_slice(&b).ok());
                    let _ = write_manifest(
                        &cfg.serve_dir,
                        &Manifest {
                            status: "failed".into(),
                            revision: prev.as_ref().and_then(|p| p.revision.clone()),
                            systems: prev.as_ref().map(|p| p.systems.clone()).unwrap_or_default(),
                            toplevels: prev
                                .as_ref()
                                .map(|p| p.toplevels.clone())
                                .unwrap_or_default(),
                            hosts: prev.as_ref().map(|p| p.hosts.clone()).unwrap_or_default(),
                            updated_at: iso_now(),
                            error: Some(format!("{e:#}")),
                        },
                    );
                }
                ui.set_running(false);
                let now: u64 = iso_now().parse().unwrap_or(0);
                ui.set_next_run_at(now + cfg.interval.as_secs());
                tokio::time::sleep(cfg.interval).await;
            }
        }
    });

    log::info!("listening on {addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
