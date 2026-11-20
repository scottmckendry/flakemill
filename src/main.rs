//! flakemill
//!
//! Pulls origin/main, updates flake.lock, builds all systems, pushes to
//! cachix, then serves a *signed* flake.lock over HTTP. Never pushes to
//! the source repo — hosts fetch and apply as they please.

use std::{
    future::{Future, IntoFuture},
    path::PathBuf,
    process::ExitCode,
    time::Duration,
};

mod atomic;
mod client;
mod clock;
mod cmd;
mod config;
mod job;
mod server;
#[cfg(test)]
mod testing;
mod ui;

use anyhow::{bail, Context, Result};
use clap::{CommandFactory, Parser, Subcommand, ValueHint};
use clap_complete::Shell;

use client::{Check, Client};
use clock::now_secs;
use config::load_cfg;
use job::{record_failure, run_job};

#[derive(Parser, Debug)]
#[command(
    name = "flakemill",
    version,
    about = "External update service for Nix flakes"
)]
struct Cli {
    /// Base URL of the flakemill service (client commands only).
    #[arg(long, env = "FLAKEMILL_URL", global = true)]
    url: Option<String>,

    /// Minisign public key file used to verify published locks.
    #[arg(long, env = "FLAKEMILL_PUBLIC_KEY", global = true, value_hint = ValueHint::FilePath)]
    public_key: Option<PathBuf>,

    /// Local flake.lock to compare against or update.
    #[arg(
        long,
        env = "FLAKEMILL_LOCK",
        default_value = "flake.lock",
        global = true,
        value_hint = ValueHint::FilePath
    )]
    lock: PathBuf,

    /// HTTP timeout in seconds.
    #[arg(long, default_value_t = 60, global = true)]
    timeout: u64,

    /// Accept an unsigned lock. Only intended for local development.
    #[arg(long, global = true)]
    allow_unsigned: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the update service.
    Serve,

    /// Report whether the published lock differs from the local lock.
    ///
    /// Exits 0 when up to date, 1 when an update is available, and 2 on error.
    Check {
        /// Print nothing unless there is an error.
        #[arg(long)]
        quiet: bool,
    },

    /// Fetch, verify, and atomically replace the local lock.
    Apply {
        /// Print nothing unless there is an error.
        #[arg(long)]
        quiet: bool,
    },

    /// Fetch and verify a lock, writing it to stdout or a file.
    Fetch {
        /// Write the verified lock here instead of stdout.
        #[arg(long, value_hint = ValueHint::FilePath)]
        output: Option<PathBuf>,
    },

    /// Generate a shell completion script on stdout.
    #[command(override_usage = "flakemill completions <SHELL>")]
    Completions {
        /// Shell to generate completions for.
        #[arg(value_parser = ["bash", "elvish", "fish", "powershell", "zsh", "nushell", "nu"])]
        shell: String,
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("flakemill: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    match &cli.command {
        Command::Serve => {
            serve()?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Completions { shell } => {
            let mut command = Cli::command();
            let mut stdout = std::io::stdout();
            if matches!(shell.as_str(), "nushell" | "nu") {
                clap_complete::generate(
                    clap_complete_nushell::Nushell,
                    &mut command,
                    "flakemill",
                    &mut stdout,
                );
            } else {
                let shell = shell.parse::<Shell>().map_err(anyhow::Error::msg)?;
                clap_complete::generate(shell, &mut command, "flakemill", &mut stdout);
            }
            Ok(ExitCode::SUCCESS)
        }
        command => run_client(&cli, command),
    }
}

fn run_client(cli: &Cli, command: &Command) -> Result<ExitCode> {
    let url = cli
        .url
        .clone()
        .context("--url (or FLAKEMILL_URL) is required for client commands")?;
    if cli.public_key.is_none() && !cli.allow_unsigned {
        bail!("--public-key (or FLAKEMILL_PUBLIC_KEY) is required; pass --allow-unsigned for development");
    }

    let client = Client {
        url,
        public_key: cli.public_key.clone(),
        lock: cli.lock.clone(),
        timeout: Duration::from_secs(cli.timeout.max(1)),
    };

    match command {
        Command::Check { quiet } => {
            let (check, old, new) = client.check().context("checking published lock")?;
            match check {
                Check::UpToDate => {
                    if !*quiet {
                        println!("up to date");
                    }
                    Ok(ExitCode::SUCCESS)
                }
                Check::UpdateAvailable => {
                    if !*quiet {
                        println!("update available");
                        print_input_changes(old.as_deref(), &new);
                    }
                    Ok(ExitCode::from(1))
                }
            }
        }
        Command::Apply { quiet } => {
            let changed = client.apply().context("applying published lock")?;
            if !*quiet {
                if let Some((old, new)) = changed {
                    println!("updated {}", client.lock.display());
                    print_input_changes(old.as_deref(), &new);
                } else {
                    println!("already up to date");
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Fetch { output } => {
            client
                .fetch_to(output.as_deref())
                .context("fetching published lock")?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Serve | Command::Completions { .. } => {
            unreachable!("non-client commands are handled before client dispatch")
        }
    }
}

fn print_input_changes(old: Option<&[u8]>, new: &[u8]) {
    use std::io::IsTerminal;
    let color = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    if let Some(changes) = client::input_changes(old, new, color) {
        for change in changes {
            println!("{change}");
        }
    } else {
        eprintln!("could not summarize lockfile input changes");
    }
}

fn serve() -> Result<()> {
    use tracing_subscriber::EnvFilter;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,tower_http=debug")),
        )
        .init();

    tokio::runtime::Runtime::new()?.block_on(async {
        let cfg = load_cfg()?;
        std::fs::create_dir_all(&cfg.serve_dir)?;
        let ui = ui::UiState::new(&cfg.serve_dir);

        let addr = std::env::var("LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
        let app = server::build_router(&cfg, ui.clone());

        tokio::spawn({
            let cfg = cfg.clone();
            async move {
                loop {
                    ui.set_running(true);

                    // The pipeline shells out to git/nix/cachix, so keep all of
                    // that blocking work off the async runtime's worker threads.
                    let job_cfg = cfg.clone();
                    let job_ui = ui.clone();
                    match tokio::task::spawn_blocking(move || run_job(&job_cfg, &job_ui)).await {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            log::error!("update job failed: {e:#}");
                            if let Err(record_err) = record_failure(
                                &cfg.serve_dir,
                                format!("{e:#}"),
                                e.downcast_ref::<job::RunFailure>().cloned(),
                            ) {
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
                            if let Err(record_err) = record_failure(&cfg.serve_dir, message, None) {
                                log::error!("recording failed manifest: {record_err:#}");
                            }
                        }
                    }

                    ui.set_running(false);
                    ui.set_next_run_at(now_secs().saturating_add(cfg.interval.as_secs()));
                    tokio::time::sleep(cfg.interval).await;
                }
            }
        });

        log::info!("listening on {addr}");
        let listener = tokio::net::TcpListener::bind(&addr).await?;
        serve_http(listener, app, shutdown_signal(), SHUTDOWN_GRACE).await?;
        Ok(())
    })
}

/// Serve HTTP until `shutdown` resolves, then allow in-flight requests at most
/// `grace` to drain before closing the listener.
async fn serve_http<F>(
    listener: tokio::net::TcpListener,
    app: axum::Router,
    shutdown: F,
    grace: Duration,
) -> std::io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    // The grace period only applies *after* a shutdown signal has started the
    // graceful drain. Don't wrap the whole serving future in the timeout: it
    // never resolves until shutdown, so doing that would close the listener
    // `grace` after startup even with no signal.
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
    let serving = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown.await;
            let _ = shutdown_tx.send(true);
        })
        .into_future();
    tokio::pin!(serving);

    tokio::select! {
        result = &mut serving => result,
        _ = shutdown_rx.changed() => {
            // Graceful shutdown waits for in-flight requests to finish, then
            // gives any stragglers a moment before closing the listener so a
            // container stop doesn't cut a response off mid-flight.
            if tokio::time::timeout(grace, &mut serving).await.is_err() {
                log::warn!("requests still in flight after {grace:?}; closing connections");
            }
            Ok(())
        }
    }
}

/// How long `serve` lets in-flight requests drain after a shutdown signal
/// before closing the listener.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// Resolve on SIGINT/SIGTERM so `serve` exits its normal path and drains
/// in-flight requests before stopping.
async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = match signal(SignalKind::terminate()) {
        Ok(term) => term,
        Err(e) => {
            log::warn!("cannot listen for SIGTERM; ctrl-c only: {e}");
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
    log::info!("shutdown signal received; stopping");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::pending;

    #[tokio::test]
    async fn serving_outlives_grace_without_a_shutdown_signal() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let app = axum::Router::new().route("/healthz", axum::routing::get(|| async { "ok" }));
        let task = tokio::spawn(serve_http(
            listener,
            app,
            pending::<()>(),
            Duration::from_millis(50),
        ));

        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            !task.is_finished(),
            "HTTP server exited without a shutdown signal"
        );
        task.abort();
    }
}
