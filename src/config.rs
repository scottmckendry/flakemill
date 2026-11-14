//! Service configuration, loaded from environment variables.

use std::{path::PathBuf, time::Duration};

#[derive(Clone)]
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

pub fn load_cfg() -> Cfg {
    Cfg {
        repo_url: env_or("REPO_URL", "https://github.com/scottmckendry/nix"),
        branch: env_or("BRANCH", "main"),
        work_dir: PathBuf::from(env_or("WORK_DIR", "/var/lib/flakemill/work")),
        serve_dir: PathBuf::from(env_or("SERVE_DIR", "/var/lib/flakemill/serve")),
        systems: env_or("SYSTEMS", "eris")
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        cache_name: env_or("CACHIX_CACHE", "scottmckendry"),
        sign_key: std::env::var("SIGN_KEY").ok().map(PathBuf::from),
        interval: Duration::from_secs(
            std::env::var("INTERVAL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(6 * 3600),
        ),
    }
}
