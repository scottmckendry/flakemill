//! Helpers for running external commands with logged output.

use std::{
    io::{BufRead, BufReader, Read},
    path::Path,
    process::{Command, Stdio},
    sync::mpsc::{self, Sender},
};

use anyhow::{bail, Context, Result};

/// Receives `(stream, line)` pairs from child output. Used to tee build logs
/// to per-host files for the dashboard.
pub type LineSink<'a> = dyn FnMut(&str, &str) + 'a;

pub fn run(cmd: &str, args: &[&str], cwd: &Path) -> Result<()> {
    run_with_sink(cmd, args, cwd, None)
}

fn base_command(cmd: &str, args: &[&str], cwd: &Path) -> Command {
    let mut command = Command::new(cmd);
    command.args(args).current_dir(cwd).stdin(Stdio::null());
    command
}

fn forward_output<R: Read + Send + 'static>(
    stream: &'static str,
    reader: R,
    tx: Sender<(&'static str, String)>,
) {
    std::thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            let Ok(line) = line else { break };
            if tx.send((stream, line)).is_err() {
                break;
            }
        }
    });
}

pub fn run_with_sink(
    cmd: &str,
    args: &[&str],
    cwd: &Path,
    mut sink: Option<&mut LineSink>,
) -> Result<()> {
    log::info!("$ {cmd} {} (in {})", args.join(" "), cwd.display());
    let mut child = base_command(cmd, args, cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning {cmd}"))?;

    // Read stdout and stderr on separate threads while the child runs. This
    // avoids buffering the whole build in memory and prevents pipe deadlocks.
    let (tx, rx) = mpsc::channel();
    if let Some(stdout) = child.stdout.take() {
        forward_output("stdout", stdout, tx.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        forward_output("stderr", stderr, tx.clone());
    }
    drop(tx);

    for (stream, line) in rx {
        if line.is_empty() {
            continue;
        }
        log::info!("[{cmd} {stream}] {line}");
        if let Some(f) = sink.as_deref_mut() {
            f(stream, &line);
        }
    }

    let status = child.wait().with_context(|| format!("waiting for {cmd}"))?;
    if !status.success() {
        bail!("{cmd} {args:?} failed: {status}");
    }
    Ok(())
}

/// Like [`run`], but distinguishes "command ran and failed" (e.g. a binary
/// cache probe that missed) from "command could not run at all". Returns
/// stderr without logging it so callers can keep probes quiet.
pub fn run_probe(cmd: &str, args: &[&str], cwd: &Path) -> Result<(bool, String)> {
    log::info!("$ {cmd} {} (in {})", args.join(" "), cwd.display());
    let out = base_command(cmd, args, cwd)
        .output()
        .with_context(|| format!("spawning {cmd}"))?;
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    Ok((out.status.success(), stderr))
}

pub fn run_capture(cmd: &str, args: &[&str], cwd: &Path) -> Result<String> {
    let out = base_command(cmd, args, cwd)
        .output()
        .with_context(|| format!("spawning {cmd}"))?;
    if !out.status.success() {
        bail!(
            "{cmd} {args:?} failed: {}: {}",
            String::from_utf8_lossy(&out.stderr),
            out.status
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;

    #[test]
    fn run_captures_both_output_streams() {
        let dir = TempDir::new("cmd-sink");
        let mut lines = Vec::new();
        {
            let mut sink = |stream: &str, line: &str| {
                lines.push((stream.to_string(), line.to_string()));
            };
            run_with_sink(
                "sh",
                &["-c", "printf 'hello\\n'; printf 'oops\\n' >&2"],
                &dir,
                Some(&mut sink),
            )
            .unwrap();
        }

        assert!(lines.contains(&("stdout".to_string(), "hello".to_string())));
        assert!(lines.contains(&("stderr".to_string(), "oops".to_string())));
    }

    #[test]
    fn run_reports_non_zero_exit() {
        let dir = TempDir::new("cmd-fail");
        let err = run("sh", &["-c", "exit 7"], &dir).unwrap_err().to_string();
        assert!(err.contains("failed"), "{err}");
    }

    #[test]
    fn run_probe_distinguishes_failure_from_spawn_error() {
        let dir = TempDir::new("cmd-probe");
        let (ok, stderr) = run_probe("sh", &["-c", "printf nope >&2; exit 1"], &dir).unwrap();
        assert!(!ok);
        assert_eq!(stderr, "nope");

        let err = run_probe("flakemill-command-that-does-not-exist", &[], &dir).unwrap_err();
        assert!(err.to_string().contains("spawning"), "{err:#}");
    }

    #[test]
    fn run_capture_trims_stdout() {
        let dir = TempDir::new("cmd-capture");
        let out = run_capture("sh", &["-c", "printf '  value\\n'"], &dir).unwrap();
        assert_eq!(out, "value");
    }
}
