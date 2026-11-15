//! Fetch and verify flake.lock files published by a flakemill server.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};

use crate::cmd::run;

/// A flakemill endpoint plus the local lockfile it should be compared with.
#[derive(Clone, Debug)]
pub struct Client {
    pub url: String,
    pub public_key: Option<PathBuf>,
    pub lock: PathBuf,
    pub timeout: Duration,
}

/// Whether the published lock differs from the local lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    UpToDate,
    UpdateAvailable,
}

impl Client {
    fn endpoint(&self, path: &str) -> String {
        format!("{}/{}", self.url.trim_end_matches('/'), path)
    }

    fn download(&self, path: &str, dest: &Path) -> Result<()> {
        let url = self.endpoint(path);
        let max_time = self.timeout.as_secs().max(1).to_string();
        run(
            "curl",
            &[
                "-fsS",
                "--max-time",
                &max_time,
                "--retry",
                "2",
                "--retry-connrefused",
                &url,
                "-o",
                dest.to_str()
                    .with_context(|| format!("download path is not UTF-8: {}", dest.display()))?,
            ],
            &std::env::temp_dir(),
        )
        .with_context(|| format!("fetching {url}"))
    }

    /// Download the published lock and verify it against the configured
    /// minisign public key. Unsigned locks are rejected unless no public key
    /// was configured (the explicit dev/PoC escape hatch).
    pub fn fetch_verified(&self) -> Result<Vec<u8>> {
        let tmp = TempDir::new()?;
        let lock = tmp.path.join("flake.lock");
        self.download("flake.lock", &lock)?;

        if let Some(public_key) = &self.public_key {
            let signature = tmp.path.join("flake.lock.minisig");
            self.download("flake.lock.minisig", &signature)?;
            run(
                "minisign",
                &[
                    "-Vm",
                    path_str(&lock)?,
                    "-x",
                    path_str(&signature)?,
                    "-p",
                    path_str(public_key)?,
                ],
                &tmp.path,
            )
            .context("verifying flake.lock minisign signature")?;
        } else {
            log::warn!("no public key configured; accepting an UNSIGNED lock");
        }

        fs::read(&lock).with_context(|| format!("reading {}", lock.display()))
    }

    pub fn local_lock(&self) -> Result<Option<Vec<u8>>> {
        match fs::read(&self.lock) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", self.lock.display())),
        }
    }

    pub fn check(&self) -> Result<Check> {
        let published = self.fetch_verified()?;
        Ok(compare(self.local_lock()?.as_deref(), &published))
    }

    /// Atomically replace the local lock. Returns `false` when it already
    /// matched, so callers can report a no-op without racing a local edit.
    pub fn apply(&self) -> Result<bool> {
        let published = self.fetch_verified()?;
        if self.local_lock()?.as_deref() == Some(published.as_slice()) {
            return Ok(false);
        }

        let parent = self.lock.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        let tmp = self.lock.with_file_name(format!(
            ".{}.flakemill.tmp",
            self.lock
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("flake.lock")
        ));
        fs::write(&tmp, &published).with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, &self.lock)
            .with_context(|| format!("replacing {}", self.lock.display()))?;
        Ok(true)
    }

    /// Fetch and verify a lock, writing it to stdout or to `output`.
    pub fn fetch_to(&self, output: Option<&Path>) -> Result<()> {
        let published = self.fetch_verified()?;
        match output {
            Some(path) => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("creating {}", parent.display()))?;
                }
                fs::write(path, published)
                    .with_context(|| format!("writing {}", path.display()))?;
            }
            None => {
                use std::io::Write;
                std::io::stdout()
                    .write_all(&published)
                    .context("writing lock to stdout")?;
            }
        }
        Ok(())
    }
}

fn compare(local: Option<&[u8]>, published: &[u8]) -> Check {
    if local == Some(published) {
        Check::UpToDate
    } else {
        Check::UpdateAvailable
    }
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .with_context(|| format!("path is not valid UTF-8: {}", path.display()))
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Result<Self> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "flakemill-client-{}-{nanos}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).with_context(|| format!("creating {}", path.display()))?;
        Ok(Self { path })
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    fn no_network_client(dir: &TempDir) -> Client {
        Client {
            url: "http://127.0.0.1:1".into(),
            public_key: None,
            lock: dir.join("flake.lock"),
            timeout: Duration::from_millis(1),
        }
    }

    #[test]
    fn endpoint_handles_trailing_slashes() {
        let client = Client {
            url: "https://mill.example/".into(),
            public_key: None,
            lock: "flake.lock".into(),
            timeout: Duration::from_secs(1),
        };
        assert_eq!(
            client.endpoint("flake.lock"),
            "https://mill.example/flake.lock"
        );
    }

    #[test]
    fn local_lock_is_optional_but_other_io_errors_surface() {
        let dir = TempDir::new("client-local");
        let client = no_network_client(&dir);
        assert_eq!(client.local_lock().unwrap(), None);

        fs::write(dir.join("flake.lock"), b"lock").unwrap();
        assert_eq!(
            client.local_lock().unwrap().as_deref(),
            Some(b"lock".as_slice())
        );
    }

    #[test]
    fn check_reports_missing_matching_and_changed_local_locks() {
        assert_eq!(compare(None, b"published"), Check::UpdateAvailable);
        assert_eq!(compare(Some(b"old"), b"published"), Check::UpdateAvailable);
        assert_eq!(compare(Some(b"published"), b"published"), Check::UpToDate);
    }
}
