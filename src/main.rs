//! flakemill
//!
//! Pulls origin/main, updates flake.lock, builds all systems, pushes to
//! cachix, then serves a *signed* flake.lock over HTTP. Never pushes to
//! the source repo — hosts fetch and apply as they please.

mod cmd;
mod config;
mod job;
mod server;
#[cfg(test)]
mod test_support;
mod ui;

use anyhow::Result;

use config::load_cfg;
use job::{record_failure, run_job, unix_now};

#[tokio::main]
async fn main() -> Result<()> {
    use tracing_subscriber::EnvFilter;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,tower_http=debug")),
        )
        .init();
    let cfg = load_cfg()?;
    std::fs::create_dir_all(&cfg.serve_dir)?;

    let addr = std::env::var("LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let ui = ui::UiState::new(&cfg.serve_dir);
    let app = server::build_router(&cfg.serve_dir, &cfg, ui.clone());

    tokio::spawn({
        let cfg = cfg.clone();
        async move {
            loop {
                ui.set_running(true);

                // The pipeline shells out to git/nix/cachix, so keep all of
                // that blocking work off the async runtime's worker threads.
                let job_cfg = cfg.clone();
                match tokio::task::spawn_blocking(move || run_job(&job_cfg)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        log::error!("update job failed: {e:#}");
                        if let Err(record_err) = record_failure(&cfg.serve_dir, format!("{e:#}")) {
                            log::error!("recording failed manifest: {record_err:#}");
                        }
                    }
                    Err(join_err) => {
                        let message = if join_err.is_panic() {
                            format!("update job panicked: {join_err}")
                        } else {
                            format!("update job was cancelled: {join_err}")
                        };
                        log::error!("{message}");
                        if let Err(record_err) = record_failure(&cfg.serve_dir, message) {
                            log::error!("recording failed manifest: {record_err:#}");
                        }
                    }
                }

                ui.set_running(false);
                ui.set_next_run_at(unix_now().saturating_add(cfg.interval.as_secs()));
                tokio::time::sleep(cfg.interval).await;
            }
        }
    });

    log::info!("listening on {addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
