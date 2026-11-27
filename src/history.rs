//! Run history: a capped ring of recent runs persisted as `history.json` in
//! `serve_dir`. Feeds the dashboard's per-host streak strips (and, later,
//! duration sparklines). Recording is best-effort: a history write failure
//! must never fail an otherwise successful update run.

use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::atomic::write_bytes;
use crate::clock::now_secs;
use crate::job::{HostStat, Manifest, Status};

/// How many runs to keep. Older entries are dropped on append.
pub const CAP: usize = 50;

/// One host's outcome within a run. Only actual attempts are recorded:
/// hosts skipped by an up-to-date run or never reached before a failure
/// are absent, so per-host streaks reflect real build attempts.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct HostOutcome {
    pub name: String,
    pub ok: bool,
    /// Duration in seconds, when known (failed builds don't report one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secs: Option<u64>,
    /// Log filename within /logs/, when one was captured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct RunRecord {
    /// Unix epoch seconds when the run finished.
    pub at: u64,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<HostOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl RunRecord {
    /// Derive a record from the manifest a run just wrote. Up-to-date runs
    /// carry the *previous* publication's host stats, so they record no
    /// attempts; failed runs record the failure's completed hosts plus the
    /// host that failed.
    fn from_manifest(m: &Manifest) -> Self {
        let success = |h: &HostStat| HostOutcome {
            name: h.name.clone(),
            ok: true,
            secs: Some(h.secs),
            log: h.log.clone(),
        };
        let hosts = match m.status {
            Status::Ok => m.hosts.iter().map(success).collect(),
            Status::Failed => m
                .failure
                .as_ref()
                .map(|f| {
                    let mut hosts: Vec<_> = f.completed.iter().map(success).collect();
                    hosts.push(HostOutcome {
                        name: f.host.clone(),
                        ok: false,
                        secs: None,
                        log: f.log.clone(),
                    });
                    hosts
                })
                .unwrap_or_default(),
            Status::UpToDate => Vec::new(),
        };
        Self {
            at: m.updated_at.parse().unwrap_or_else(|_| now_secs()),
            status: m.status,
            revision: m.revision.clone(),
            hosts,
            error: m.error.clone(),
        }
    }
}

/// Read the persisted history, oldest first. Missing or unreadable files
/// yield an empty history rather than an error: history is decorative and
/// must never take the dashboard down.
pub fn read(serve_dir: &Path) -> Vec<RunRecord> {
    let path = serve_dir.join("history.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            log::warn!("reading {}: {e}", path.display());
            return Vec::new();
        }
    };
    match serde_json::from_slice::<Vec<RunRecord>>(&bytes) {
        Ok(mut runs) => {
            // Tolerate an oversized file (e.g. written by a newer version).
            let excess = runs.len().saturating_sub(CAP);
            runs.drain(..excess);
            runs
        }
        Err(e) => {
            log::warn!("parsing {}: {e}", path.display());
            Vec::new()
        }
    }
}

/// Append the run described by `m` to the history and persist it, keeping at
/// most [`CAP`] entries. Idempotent per run: re-recording the same manifest
/// (e.g. after a failed `record_failure` left the previous one in place) does
/// not duplicate the entry.
pub fn record(serve_dir: &Path, m: &Manifest) -> Result<()> {
    let entry = RunRecord::from_manifest(m);
    let mut runs = read(serve_dir);
    if runs
        .last()
        .is_some_and(|last| last.at == entry.at && last.status == entry.status)
    {
        return Ok(());
    }
    runs.push(entry);
    let excess = runs.len().saturating_sub(CAP);
    runs.drain(..excess);
    write_bytes(&serve_dir.join("history.json"), &serde_json::to_vec(&runs)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::{HostAction, RunFailure};
    use crate::testing::TempDir;

    fn manifest(status: Status, updated_at: &str) -> Manifest {
        Manifest {
            status,
            revision: Some("abc123".into()),
            systems: vec!["eris".into(), "other".into()],
            toplevels: Vec::new(),
            hosts: vec![HostStat {
                name: "eris".into(),
                action: HostAction::Built,
                log: Some("eris.log".into()),
                secs: 42,
            }],
            updated_at: updated_at.into(),
            error: None,
            failure: None,
        }
    }

    #[test]
    fn successful_run_records_each_host_attempt() {
        let dir = TempDir::new("history-ok");
        record(&dir, &manifest(Status::Ok, "100")).unwrap();
        let runs = read(&dir);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].at, 100);
        assert_eq!(runs[0].status, Status::Ok);
        assert_eq!(
            runs[0].hosts,
            vec![HostOutcome {
                name: "eris".into(),
                ok: true,
                secs: Some(42),
                log: Some("eris.log".into()),
            }]
        );
    }

    #[test]
    fn failed_run_records_completed_hosts_and_the_failure() {
        let dir = TempDir::new("history-failed");
        let mut m = manifest(Status::Failed, "100");
        m.error = Some("build failed".into());
        m.failure = Some(RunFailure {
            host: "other".into(),
            stage: "build".into(),
            log: Some("other-attempt.log".into()),
            completed: m.hosts.clone(),
            systems: m.systems.clone(),
        });
        record(&dir, &m).unwrap();
        let runs = read(&dir);
        assert_eq!(runs[0].hosts.len(), 2);
        assert!(runs[0].hosts[0].ok);
        assert_eq!(runs[0].hosts[0].name, "eris");
        assert!(!runs[0].hosts[1].ok);
        assert_eq!(runs[0].hosts[1].name, "other");
        assert_eq!(runs[0].hosts[1].secs, None);
        assert_eq!(runs[0].error.as_deref(), Some("build failed"));
    }

    #[test]
    fn up_to_date_and_unknown_failures_record_no_attempts() {
        let dir = TempDir::new("history-no-attempts");
        // Up-to-date manifests carry the previous publication's hosts; those
        // are not new attempts.
        record(&dir, &manifest(Status::UpToDate, "100")).unwrap();
        record(&dir, &manifest(Status::Failed, "200")).unwrap();
        let runs = read(&dir);
        assert_eq!(runs.len(), 2);
        assert!(runs.iter().all(|r| r.hosts.is_empty()));
    }

    #[test]
    fn history_is_capped_keeping_the_newest_runs() {
        let dir = TempDir::new("history-cap");
        for i in 0..(CAP as u64 + 10) {
            record(&dir, &manifest(Status::Ok, &i.to_string())).unwrap();
        }
        let runs = read(&dir);
        assert_eq!(runs.len(), CAP);
        assert_eq!(runs[0].at, 10);
        assert_eq!(runs.last().unwrap().at, CAP as u64 + 9);
    }

    #[test]
    fn recording_the_same_manifest_twice_is_idempotent() {
        let dir = TempDir::new("history-dedupe");
        let m = manifest(Status::Ok, "100");
        record(&dir, &m).unwrap();
        record(&dir, &m).unwrap();
        assert_eq!(read(&dir).len(), 1);
    }

    #[test]
    fn missing_or_corrupt_history_reads_as_empty() {
        let dir = TempDir::new("history-corrupt");
        assert!(read(&dir).is_empty());
        std::fs::write(dir.join("history.json"), b"not json").unwrap();
        assert!(read(&dir).is_empty());
        // A corrupt file is replaced, not appended to.
        record(&dir, &manifest(Status::Ok, "100")).unwrap();
        assert_eq!(read(&dir).len(), 1);
    }

    #[test]
    fn oversized_history_files_are_trimmed_on_read() {
        let dir = TempDir::new("history-oversized");
        let runs: Vec<_> = (0..(CAP as u64 + 5))
            .map(|i| RunRecord::from_manifest(&manifest(Status::Ok, &i.to_string())))
            .collect();
        std::fs::write(dir.join("history.json"), serde_json::to_vec(&runs).unwrap()).unwrap();
        let trimmed = read(&dir);
        assert_eq!(trimmed.len(), CAP);
        assert_eq!(trimmed[0].at, 5);
    }
}
