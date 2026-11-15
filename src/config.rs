//! Service configuration, loaded from environment variables.

use std::{path::PathBuf, time::Duration};

use anyhow::{bail, Context, Result};

const DEFAULT_INTERVAL_SECS: u64 = 6 * 3600;

#[derive(Clone, Debug)]
pub struct Cfg {
    pub repo_url: String,
    pub branch: String,
    pub work_dir: PathBuf,
    pub serve_dir: PathBuf,
    pub systems: Vec<String>,
    pub cache_name: String,
    pub sign_key: Option<PathBuf>,
    pub interval: Duration,
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_req(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("required env var {key} is not set"))
}

fn parse_systems(raw: &str) -> Result<Vec<String>> {
    let mut systems = Vec::new();
    for system in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if !systems.iter().any(|existing| existing == system) {
            systems.push(system.to_string());
        }
    }
    if systems.is_empty() {
        bail!("SYSTEMS must contain at least one system");
    }
    Ok(systems)
}

fn parse_interval(raw: Option<&str>) -> Result<Duration> {
    let secs = match raw {
        Some(value) => value
            .trim()
            .parse::<u64>()
            .with_context(|| format!("INTERVAL_SECS must be a positive integer, got {value:?}"))?,
        None => DEFAULT_INTERVAL_SECS,
    };
    if secs == 0 {
        bail!("INTERVAL_SECS must be greater than zero");
    }
    Ok(Duration::from_secs(secs))
}

pub fn load_cfg() -> Result<Cfg> {
    Ok(Cfg {
        repo_url: env_req("REPO_URL")?,
        branch: env_or("BRANCH", "main"),
        work_dir: PathBuf::from(env_or("WORK_DIR", "/var/lib/flakemill/work")),
        serve_dir: PathBuf::from(env_or("SERVE_DIR", "/var/lib/flakemill/serve")),
        systems: parse_systems(&env_req("SYSTEMS")?)?,
        cache_name: env_req("CACHIX_CACHE")?,
        sign_key: std::env::var_os("SIGN_KEY")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
        interval: parse_interval(std::env::var("INTERVAL_SECS").ok().as_deref())?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systems_are_trimmed_and_empties_removed() {
        assert_eq!(
            parse_systems(" eris, ,luna , eris ").unwrap(),
            vec!["eris".to_string(), "luna".to_string()]
        );
    }

    #[test]
    fn systems_must_not_be_empty() {
        let err = parse_systems(" , , ").unwrap_err().to_string();
        assert!(err.contains("at least one system"), "{err}");
    }

    #[test]
    fn interval_defaults_to_six_hours() {
        assert_eq!(
            parse_interval(None).unwrap(),
            Duration::from_secs(DEFAULT_INTERVAL_SECS)
        );
    }

    #[test]
    fn interval_is_parsed_from_seconds() {
        assert_eq!(
            parse_interval(Some(" 300 ")).unwrap(),
            Duration::from_secs(300)
        );
    }

    #[test]
    fn interval_rejects_zero_and_garbage() {
        let zero = parse_interval(Some("0")).unwrap_err().to_string();
        assert!(zero.contains("greater than zero"), "{zero}");

        let garbage = parse_interval(Some("soon")).unwrap_err().to_string();
        assert!(garbage.contains("positive integer"), "{garbage}");
    }
}
