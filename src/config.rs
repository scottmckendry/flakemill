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
    /// Whether to serve the local Nix store as a binary cache at `/cache/*`.
    /// Independent of the Cachix push target.
    pub local_cache_enabled: bool,
    /// Host the private `nix-serve` child binds. The port is always chosen at
    /// startup, and flakemill is the only client of it (proxied at `/cache/*`).
    pub cache_listen_host: String,
    /// Persistent Nix cache signing key, separate from the minisign key.
    pub cache_key: PathBuf,
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

fn parse_bool(raw: &str) -> Result<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        other => bail!("LOCAL_CACHE_ENABLED must be a boolean, got {other:?}"),
    }
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

/// `CACHE_LISTEN_HOST` is where the private `nix-serve` binds; the port is
/// picked at startup, so reject anything that smuggles one in (or is not a
/// single host). Bracketed IPv6 is accepted as `nix-serve --listen` wants it.
fn parse_cache_host(raw: &str) -> Result<String> {
    let host = raw.trim();
    let ipv6 = host.starts_with('[') && host.ends_with(']') && host.len() > 2;
    let plain = !host.is_empty() && !host.contains(['[', ']', ':', '/', ' ']);
    if ipv6 || plain {
        Ok(host.to_string())
    } else {
        bail!("CACHE_LISTEN_HOST must be a bare host or a bracketed [IPv6] address, got {raw:?}")
    }
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
        local_cache_enabled: parse_bool(&env_or("LOCAL_CACHE_ENABLED", "true"))?,
        cache_listen_host: parse_cache_host(&env_or("CACHE_LISTEN_HOST", "127.0.0.1"))?,
        cache_key: PathBuf::from(env_or("CACHE_KEY", "/var/lib/flakemill/keys/cache")),
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

    #[test]
    fn local_cache_enabled_accepts_common_booleans() {
        for value in ["1", "true", "TRUE", "yes", "on", " on "] {
            assert!(parse_bool(value).unwrap(), "{value}");
        }
        for value in ["0", "false", "False", "no", "off", " off "] {
            assert!(!parse_bool(value).unwrap(), "{value}");
        }
        let err = parse_bool("maybe").unwrap_err().to_string();
        assert!(err.contains("LOCAL_CACHE_ENABLED"), "{err}");
    }

    #[test]
    fn cache_listen_host_is_a_bare_host() {
        for valid in ["127.0.0.1", "localhost", "mill.example", "[::1]"] {
            assert!(!parse_cache_host(valid).unwrap().is_empty(), "{valid}");
        }
        assert_eq!(parse_cache_host(" 127.0.0.1 ").unwrap(), "127.0.0.1");
        assert_eq!(parse_cache_host("[::1]").unwrap(), "[::1]");
        // Ports belong to the kernel's choice, not to config.
        for invalid in [
            "",
            "127.0.0.1:5000",
            "::1",
            "[::1]:5000",
            "a b",
            "host/path",
        ] {
            let err = parse_cache_host(invalid).unwrap_err().to_string();
            assert!(err.contains("CACHE_LISTEN_HOST"), "{err}");
        }
    }
}
