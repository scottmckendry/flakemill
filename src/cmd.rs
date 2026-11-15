//! Helpers for running external commands with logged output.

use std::path::Path;

use anyhow::{bail, Context, Result};

/// Streams `(stream, line)` pairs from child output. Used to tee build logs
/// to per-host files for the dashboard.
pub type LineSink<'a> = dyn FnMut(&str, &str) + 'a;

pub fn run(cmd: &str, args: &[&str], cwd: &Path) -> Result<()> {
    run_with_sink(cmd, args, cwd, None)
}

pub fn run_with_sink(
    cmd: &str,
    args: &[&str],
    cwd: &Path,
    mut sink: Option<&mut LineSink>,
) -> Result<()> {
    log::info!("$ {cmd} {} (in {})", args.join(" "), cwd.display());
    let out = std::process::Command::new(cmd)
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .output()
        .with_context(|| format!("spawning {cmd}"))?;
    // Route child output through the logger so it gets level/timestamps
    // like the rest of the service logs.
    for (stream, text) in [
        ("stdout", String::from_utf8_lossy(&out.stdout)),
        ("stderr", String::from_utf8_lossy(&out.stderr)),
    ] {
        for line in text.lines() {
            if !line.is_empty() {
                log::info!("[{cmd} {stream}] {line}");
                if let Some(f) = sink.as_deref_mut() {
                    f(stream, line);
                }
            }
        }
    }
    if !out.status.success() {
        bail!("{cmd} {args:?} failed: {}", out.status);
    }
    Ok(())
}

/// Like [`run`], but distinguishes "command ran and failed" (e.g. a binary
/// cache probe that missed) from "command could not run at all". Returns
/// stderr without logging it so callers can keep probes quiet.
pub fn run_probe(cmd: &str, args: &[&str], cwd: &Path) -> Result<(bool, String)> {
    log::info!("$ {cmd} {} (in {})", args.join(" "), cwd.display());
    let out = std::process::Command::new(cmd)
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .output()
        .with_context(|| format!("spawning {cmd}"))?;
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    Ok((out.status.success(), stderr))
}

pub fn run_capture(cmd: &str, args: &[&str], cwd: &Path) -> Result<String> {
    let out = std::process::Command::new(cmd)
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
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
