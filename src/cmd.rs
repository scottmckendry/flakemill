//! Helpers for running external commands with logged output.

use std::path::Path;

use anyhow::{bail, Context, Result};

pub fn run(cmd: &str, args: &[&str], cwd: &Path) -> Result<()> {
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
            }
        }
    }
    if !out.status.success() {
        bail!("{cmd} {args:?} failed: {}", out.status);
    }
    Ok(())
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
