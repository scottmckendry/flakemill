//! flakemill
//!
//! Pulls origin/main, updates flake.lock, builds all systems, pushes to
//! cachix, then serves a *signed* flake.lock over HTTP. Never pushes to
//! the source repo — hosts fetch and apply as they please.

mod cmd;
mod config;
mod job;
mod server;

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

    let app = server::build_router(&cfg.serve_dir);

    tokio::spawn({
        let cfg = cfg.clone();
        async move {
            loop {
                if let Err(e) = run_job(&cfg).await {
                    log::error!("update job failed: {e:#}");
                    let _ = write_manifest(
                        &cfg.serve_dir,
                        &Manifest {
                            status: "failed".into(),
                            updated_at: iso_now(),
                            error: Some(format!("{e:#}")),
                            ..Default::default()
                        },
                    );
                }
                tokio::time::sleep(cfg.interval).await;
            }
        }
    });

    let addr = std::env::var("LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    log::info!("listening on {addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
