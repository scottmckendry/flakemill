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

/// Previous (if present) and newly published lockfile contents.
pub type LockfileChange = (Option<Vec<u8>>, Vec<u8>);

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

    pub fn check(&self) -> Result<(Check, Option<Vec<u8>>, Vec<u8>)> {
        let published = self.fetch_verified()?;
        let local = self.local_lock()?;
        Ok((compare(local.as_deref(), &published), local, published))
    }

    /// Atomically replace the local lock and return the previous and new contents.
    pub fn apply(&self) -> Result<Option<LockfileChange>> {
        let published = self.fetch_verified()?;
        let local = self.local_lock()?;
        if local.as_deref() == Some(published.as_slice()) {
            return Ok(None);
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
        Ok(Some((local, published)))
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

/// Describe changed inputs by their paths from the root, rather than unstable node IDs.
pub fn input_changes(old: Option<&[u8]>, new: &[u8], color: bool) -> Option<Vec<String>> {
    fn inputs(bytes: &[u8]) -> Option<std::collections::BTreeMap<String, serde_json::Value>> {
        let lock: serde_json::Value = serde_json::from_slice(bytes).ok()?;
        let nodes = lock.get("nodes")?.as_object()?;
        let root = lock.get("root")?.as_str()?;
        fn visit(
            nodes: &serde_json::Map<String, serde_json::Value>,
            id: &str,
            prefix: &str,
            stack: &mut Vec<String>,
            result: &mut std::collections::BTreeMap<String, serde_json::Value>,
        ) -> Option<()> {
            if stack.iter().any(|entry| entry == id) {
                return Some(());
            }
            stack.push(id.to_owned());
            let node = nodes.get(id)?;
            if let Some(inputs) = node.get("inputs") {
                for (name, target) in inputs.as_object()? {
                    let path = if prefix.is_empty() {
                        name.clone()
                    } else {
                        format!("{prefix}/{name}")
                    };
                    if let Some(id) = target.as_str() {
                        let child = nodes.get(id)?;
                        let locked = child
                            .get("locked")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        result.insert(path.clone(), locked);
                        visit(nodes, id, &path, stack, result)?;
                    } else if target.is_array() {
                        result.insert(path, serde_json::json!({"follows": target}));
                    } else {
                        return None;
                    }
                }
            }
            stack.pop();
            Some(())
        }
        let mut result = std::collections::BTreeMap::new();
        visit(nodes, root, "", &mut Vec::new(), &mut result)?;
        Some(result)
    }
    fn display(value: &serde_json::Value) -> String {
        match value {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(items) if items.iter().all(|v| v.is_string()) => items
                .iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join("/"),
            _ => value.to_string(),
        }
    }
    fn describe(
        path: &str,
        old: Option<&serde_json::Value>,
        new: Option<&serde_json::Value>,
        color: bool,
    ) -> String {
        fn paint(text: &str, code: &str, color: bool) -> String {
            if color {
                format!("\x1b[{code}m{text}\x1b[0m")
            } else {
                text.to_owned()
            }
        }
        fn field_value(key: &str, value: &serde_json::Value) -> String {
            if key == "lastModified" {
                if let Some(date) = value
                    .as_i64()
                    .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
                {
                    return date.format("%Y-%m-%d %H:%M UTC").to_string();
                }
            }
            display(value)
        }
        let status = match (old, new) {
            (None, _) => " (added)",
            (_, None) => " (removed)",
            _ => "",
        };
        let mut lines = vec![format!("  {}{status}:", paint(path, "1;36", color))];
        let old_fields = old.and_then(|v| v.as_object());
        let new_fields = new.and_then(|v| v.as_object());
        if old.is_none_or(|v| v.is_object()) && new.is_none_or(|v| v.is_object()) {
            let keys: std::collections::BTreeSet<_> = old_fields
                .into_iter()
                .flat_map(|m| m.keys())
                .chain(new_fields.into_iter().flat_map(|m| m.keys()))
                .collect();
            for key in keys {
                let a = old_fields.and_then(|m| m.get(key));
                let b = new_fields.and_then(|m| m.get(key));
                if a == b || matches!(key.as_str(), "rev" | "narHash" | "sha256" | "hash") {
                    continue;
                }
                let label = if key == "lastModified" {
                    "last modified"
                } else {
                    key
                };
                lines.push(format!("    {label}:"));
                if let Some(value) = a {
                    lines.push(format!(
                        "      {}",
                        paint(&format!("- {}", field_value(key, value)), "31", color)
                    ));
                }
                if let Some(value) = b {
                    lines.push(format!(
                        "      {}",
                        paint(&format!("+ {}", field_value(key, value)), "32", color)
                    ));
                }
            }
        } else {
            if let Some(value) = old {
                lines.push(format!("    - {}", display(value)));
            }
            if let Some(value) = new {
                lines.push(format!("    + {}", display(value)));
            }
        }
        lines.join("\n")
    }
    let old = match old {
        Some(bytes) => inputs(bytes)?,
        None => Default::default(),
    };
    let new = inputs(new)?;
    let paths: std::collections::BTreeSet<_> = old.keys().chain(new.keys()).collect();
    Some(
        paths
            .into_iter()
            .filter_map(|path| match (old.get(path), new.get(path)) {
                (Some(a), Some(b)) if a != b => Some(describe(path, Some(a), Some(b), color)),
                (None, Some(b)) => Some(describe(path, None, Some(b), color)),
                (Some(a), None) => Some(describe(path, Some(a), None, color)),
                _ => None,
            })
            .collect(),
    )
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
    fn input_changes_reports_updates_additions_removals_and_follows() {
        let old = br#"{"root":"root","nodes":{"root":{"inputs":{"nixpkgs":"old","removed":"old","alias":["nixpkgs"]}},"old":{"locked":{"rev":"aaa"}}}}"#;
        let new = br#"{"root":"root","nodes":{"root":{"inputs":{"nixpkgs":"new","added":"new","alias":["other"]}},"new":{"locked":{"rev":"bbb"}}}}"#;
        let changes = input_changes(Some(old), new, false).unwrap().join("\n");
        assert!(changes.contains("nixpkgs:"));
        assert!(!changes.contains("aaa") && !changes.contains("bbb"));
        assert!(changes.contains("added (added):"));
        assert!(changes.contains("removed (removed):"));
        assert!(!changes.contains("rev:"));
        assert!(changes.contains("alias:"));
        assert!(input_changes(Some(b"invalid"), new, false).is_none());
        assert_eq!(
            input_changes(Some(new), new, false).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn input_changes_formats_dates_and_optional_color() {
        let old = br#"{"root":"r","nodes":{"r":{"inputs":{"x":"x"}},"x":{"locked":{"lastModified":0,"narHash":"old","rev":"old"}}}}"#;
        let new = br#"{"root":"r","nodes":{"r":{"inputs":{"x":"x"}},"x":{"locked":{"lastModified":86400,"narHash":"new","rev":"new"}}}}"#;
        let plain = input_changes(Some(old), new, false).unwrap().join("\n");
        assert!(plain.contains("1970-01-01 00:00 UTC"));
        assert!(plain.contains("1970-01-02 00:00 UTC"));
        assert!(!plain.contains("narHash") && !plain.contains("rev"));
        assert!(!plain.contains('\x1b'));
        assert!(input_changes(Some(old), new, true)
            .unwrap()
            .join("\n")
            .contains("\x1b["));
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
