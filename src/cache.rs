//! Local binary cache: serve the Nix store over flakemill's HTTP endpoint.
//!
//! A `nix-serve-ng` child listens on loopback on a port chosen at startup and
//! streams narinfos/NARs straight from the persistent `/nix` store (no second
//! on-disk copy). The axum router reverse-proxies `/cache/*` to it, so clients
//! configure a single substituter URL. Narinfos are signed with a persistent
//! Nix cache key, separate from the minisign lockfile key; its public half is
//! published at `/cache.pub`.
//!
//! This serves local store paths only, not arbitrary upstream requests.

use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use axum::{
    body::Body,
    extract::State,
    http::header::HeaderName,
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};

use crate::atomic::write_bytes;
use crate::config::Cfg;

/// Key name embedded in the signing key. Clients trust `flakemill-cache:…`.
///
/// The name is part of every narinfo signature, so changing it (or regenerating
/// the key) invalidates clients that trust the old name. It takes effect only
/// when a new key is generated; existing key files keep the name they were made
/// with.
const CACHE_KEY_NAME: &str = "flakemill-cache";

/// The cache prefix served by flakemill; the proxy target is everything after
/// it. Nix appends `/nix-cache-info`, `/<hash>.narinfo`, and `/nar/…` to the
/// substituter URL.
pub const CACHE_PREFIX: &str = "/cache";

/// How long to wait for a freshly spawned `nix-serve` to answer requests.
const READY_TIMEOUT: Duration = Duration::from_millis(3000);

/// How many ports to try before giving up on the local cache. Each attempt
/// binds a fresh ephemeral port, so this only matters when something else
/// wins the race for a port we just released.
const PORT_ATTEMPTS: u8 = 3;

/// Pause between readiness probes while `nix-serve` starts.
const PROBE_INTERVAL: Duration = Duration::from_millis(50);

/// Where the `nix-serve` child listens, and where the proxy forwards to.
///
/// The port comes from the kernel's ephemeral range rather than from config:
/// a stale `nix-serve` left behind by a hard-killed predecessor can then never
/// collide with, or masquerade as, this instance's cache.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    host: String,
    port: u16,
}

impl Endpoint {
    /// The argument for `nix-serve --listen`.
    pub fn addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// The proxy upstream: where `/cache/*` requests are forwarded.
    pub fn upstream(&self) -> String {
        format!("http://{}", self.addr())
    }

    fn from_socket(addr: &str) -> Option<Endpoint> {
        let (host, port) = addr.rsplit_once(':')?;
        Some(Endpoint {
            host: host.to_string(),
            port: port.parse().ok()?,
        })
    }
}

/// Reserve a free port on `host`: bind port 0, ask the kernel which port we
/// got, and release it again for `nix-serve` to claim. The window between
/// release and claim is tiny and retried, and flakemill is the only process
/// that ever talks to this port.
fn reserve_port(host: &str) -> Result<Endpoint> {
    let bind = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    let listener = std::net::TcpListener::bind((bind.as_str(), 0))
        .with_context(|| format!("reserving a cache port on {bind}"))?;
    let addr = listener
        .local_addr()
        .context("reading the reserved cache port")?;
    Endpoint::from_socket(&addr.to_string())
        .with_context(|| format!("unexpected cache address {addr}"))
}

/// Hop-by-hop headers that must not be forwarded through the proxy.
fn is_hop_by_hop(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "keep-alive"
            | "transfer-encoding"
            | "upgrade"
            | "te"
            | "trailer"
            | "proxy-authenticate"
            | "proxy-authorization"
    )
}

/// Ensure a persistent cache signing key exists at `key_path` (mode 0600) and
/// return its public half (`flakemill-cache:…`). Existing keys are never
/// regenerated: hosts that already trust the public key must keep verifying.
pub fn ensure_signing_key(key_path: &Path) -> Result<String> {
    if let Some(parent) = key_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating key directory {}", parent.display()))?;
    }
    if !key_path.exists() {
        let secret = nix_output(
            &["key", "generate-secret", "--key-name", CACHE_KEY_NAME],
            None,
        )
        .context("generating cache signing key (is nix on PATH?)")?;
        write_bytes(key_path, secret.as_bytes())
            .with_context(|| format!("writing {}", key_path.display()))?;
        set_private(key_path).with_context(|| format!("chmod {}", key_path.display()))?;
        log::info!("generated cache signing key at {}", key_path.display());
    }
    // Derive the public key from the file on disk, not from what generate
    // printed, so an operator-replaced key is picked up as-is.
    let secret =
        std::fs::read(key_path).with_context(|| format!("reading {}", key_path.display()))?;
    nix_output(&["key", "convert-secret-to-public"], Some(&secret))
        .map_err(|e| e.context("deriving cache public key"))
}

/// Run a `nix` nix-command subcommand, optionally feeding `stdin`, and return
/// trimmed stdout. `cmd::run_capture` pins stdin to null, which doesn't work
/// for `key convert-secret-to-public`.
fn nix_output(args: &[&str], stdin: Option<&[u8]>) -> Result<String> {
    use std::io::Write;
    let mut child = Command::new("nix")
        .arg("--extra-experimental-features")
        .arg("nix-command")
        .args(args)
        .current_dir("/")
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning nix {args:?}"))?;
    if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
        pipe.write_all(input).context("writing to nix stdin")?;
        // Dropping `pipe` closes stdin so nix sees EOF.
    }
    let out = child
        .wait_with_output()
        .with_context(|| format!("waiting for nix {args:?}"))?;
    if !out.status.success() {
        anyhow::bail!(
            "nix {args:?} failed: {}: {}",
            String::from_utf8_lossy(&out.stderr),
            out.status
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(unix)]
fn set_private(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// The running `nix-serve` child. Killed on drop so exiting `flakemill serve`
/// never orphans the cache process.
pub struct CacheService {
    child: Child,
}

impl Drop for CacheService {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawn `nix-serve` on `listen` (host:port), signing narinfos with the key at
/// `key_path`. Returns `None` when the binary is not installed; the rest of
/// flakemill works fine without the local cache.
///
/// nix-serve-ng takes its signing key from `NIX_SECRET_KEY_FILE`, *not* from
/// nix.conf's `secret-key-files` (which it ignores), and replaces any
/// inherited signatures with one made by that key. It also refrains from
/// signing at all when the variable is unset, so clients with `require-sigs`
/// would reject every path.
///
/// Access logging is off (`--quiet`): the cache answers many requests per
/// build and would otherwise drown flakemill's own update logs.
///
/// `--priority 30` is what makes the mill the first cache hosts consult: nix
/// tries substituters in ascending order of the priority each one advertises,
/// and `cache.nixos.org` publishes 40 while Cachix publishes 41. That is also
/// nix-serve-ng's default, but saying it out loud keeps the intent if the
/// default ever moves and puts it in the served `nix-cache-info`.
pub fn spawn_nix_serve(listen: &str, key_path: &Path) -> Result<Option<CacheService>> {
    let key_str = key_path
        .to_str()
        .with_context(|| format!("cache key path is not valid UTF-8: {}", key_path.display()))?;
    let mut command = Command::new("nix-serve");
    command
        .args(["--listen", listen, "--priority", "30", "--quiet"])
        .env("NIX_SECRET_KEY_FILE", key_str)
        .stdin(Stdio::null());
    die_with_us(&mut command);
    let spawned = command.spawn();
    match spawned {
        Ok(child) => {
            log::debug!("spawned nix-serve on {listen}, signing with {key_str}");
            Ok(Some(CacheService { child }))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            log::warn!("nix-serve not found on PATH; local binary cache disabled");
            Ok(None)
        }
        Err(e) => Err(e).context("spawning nix-serve"),
    }
}

/// Ask the kernel to kill the child when flakemill dies, however it dies. A
/// plain `SIGKILL` skips `CacheService::drop`, and an unreaped `nix-serve`
/// keeps its store views (and its port) alive behind us.
#[cfg(unix)]
fn die_with_us(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    // `prctl` is fork-safe and the only thing we do before exec, but `pre_exec`
    // is still `unsafe`: the child must not touch anything but the syscall.
    unsafe {
        command.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM, 0, 0, 0);
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn die_with_us(_command: &mut Command) {}

/// Wait until `nix-serve` answers HTTP on `endpoint`, giving up after
/// `timeout` or the moment `alive` reports the child exited.
///
/// `spawn` only proves the binary exists: it exits immediately when it cannot
/// bind its port or read the store. Without this check flakemill would
/// advertise a cache — key on the dashboard, substituter in the logs — that
/// answers nothing. Checking `alive` between probes is what keeps that honest
/// and startup quick: a child that already died is known in one round instead
/// of after the full timeout, on every retry.
fn wait_ready(
    endpoint: &Endpoint,
    timeout: Duration,
    mut alive: impl FnMut() -> std::io::Result<bool>,
) -> bool {
    let addr = endpoint.addr();
    let deadline = Instant::now() + timeout;
    loop {
        match std::net::TcpStream::connect(&addr).and_then(probe) {
            Ok(()) => return true,
            Err(e) => log::debug!("cache probe of {addr} failed: {e}"),
        }
        match alive() {
            Ok(true) => {}
            Ok(false) => return false,
            Err(e) => {
                log::warn!("checking on nix-serve: {e}");
                return false;
            }
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(PROBE_INTERVAL);
    }
}

/// Send a minimal `GET /nix-cache-info` and accept any HTTP response: reaching
/// the server at all is what we are checking, not what it has in the store.
fn probe(mut stream: std::net::TcpStream) -> std::io::Result<()> {
    let timeout = Duration::from_millis(500);
    stream.set_write_timeout(Some(timeout))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.write_all(
        b"GET /nix-cache-info HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )?;
    stream.flush()?;
    // Half-close: a server waiting for the end of the request must still
    // answer within the read timeout rather than sit on an open write half.
    stream.shutdown(std::net::Shutdown::Write)?;
    // Read the status line, then drain the rest: hanging up mid-body would
    // make the server log a broken pipe for a probe that already succeeded.
    let mut reader = std::io::BufReader::new(&mut stream);
    let mut status = String::new();
    let read = reader.read_line(&mut status)?;
    if read == 0 || !status.starts_with("HTTP/") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "upstream is not an HTTP server",
        ));
    }
    let mut sink = [0u8; 512];
    loop {
        // A server that closes early or slowly is still a server that spoke
        // HTTP: the status line already decided that, so stop draining.
        match reader.read(&mut sink) {
            Ok(0) | Err(_) => return Ok(()),
            Ok(_) => {}
        }
    }
}

/// Spawn `nix-serve` and confirm it is serving, trying fresh ports when the
/// reserved one comes up dead. Returns `None` when no attempt answers, after
/// logging why; the caller treats that like a missing binary.
fn spawn_ready(key_path: &Path, host: &str) -> Result<Option<(CacheService, Endpoint)>> {
    // Suffix for a failed attempt: the last one does not retry.
    let retry = |attempt: u8| {
        if attempt < PORT_ATTEMPTS {
            format!("; retrying on a new port (attempt {attempt}/{PORT_ATTEMPTS})")
        } else {
            String::new()
        }
    };
    for attempt in 1..=PORT_ATTEMPTS {
        let endpoint = reserve_port(host)?;
        match spawn_nix_serve(&endpoint.addr(), key_path)? {
            None => return Ok(None),
            Some(mut service) => {
                let alive = || service.child.try_wait().map(|status| status.is_none());
                if wait_ready(&endpoint, READY_TIMEOUT, alive) {
                    log::info!(
                        "nix-serve listening on {}, signing with {}",
                        endpoint.addr(),
                        key_path.display()
                    );
                    return Ok(Some((service, endpoint)));
                }
                let status = service.child.try_wait()?;
                // Say what actually happened: a child that exited is a
                // different (and louder) problem than one that never answers.
                match status {
                    Some(status) => log::warn!(
                        "nix-serve on {} died ({status}) before answering{}",
                        endpoint.addr(),
                        retry(attempt),
                    ),
                    None => log::warn!(
                        "nix-serve on {} did not answer within {READY_TIMEOUT:?}{}",
                        endpoint.addr(),
                        retry(attempt),
                    ),
                }
            }
        }
    }
    log::warn!(
        "local binary cache disabled: nix-serve never came up on {host} \
         (is a store view readable?)"
    );
    Ok(None)
}

/// Start the local binary cache: ensure the signing key, publish its public
/// half to `serve_dir/cache.pub`, and spawn a serving `nix-serve`. All
/// failures are reported to the caller, which degrades gracefully (the lock
/// service does not depend on the cache). On success returns the child (stop
/// it by dropping) and where it listens, for the proxy upstream.
pub fn start(cfg: &Cfg, host: &str) -> Result<Option<(CacheService, Endpoint)>> {
    let public = ensure_signing_key(&cfg.cache_key)?;
    write_bytes(&cfg.serve_dir.join("cache.pub"), public.as_bytes())
        .context("publishing cache public key")?;
    spawn_ready(&cfg.cache_key, host)
}

#[derive(Clone)]
struct ProxyState {
    client: Client<hyper_util::client::legacy::connect::HttpConnector, Body>,
    upstream: String,
}

/// Router serving `/cache/*` by stripping the prefix and forwarding the raw
/// (still percent-encoded) path and query to nix-serve. Streaming bodies in
/// both directions keeps large NAR downloads out of memory.
pub fn proxy_router(upstream: &str) -> Router {
    let state = ProxyState {
        client: Client::builder(TokioExecutor::new()).build_http(),
        upstream: upstream.trim_end_matches('/').to_string(),
    };
    Router::new()
        .route(
            "/cache/{*rest}",
            get(proxy_request)
                .head(proxy_request)
                .fallback(proxy_request),
        )
        .route(
            "/cache",
            get(proxy_root).head(proxy_root).fallback(proxy_root),
        )
        .with_state(state)
}

/// `/cache` itself (and `/cache/`): forward to nix-serve's root.
async fn proxy_root(State(state): State<ProxyState>, req: http::Request<Body>) -> Response {
    forward(&state, "/", req).await
}

async fn proxy_request(State(state): State<ProxyState>, req: http::Request<Body>) -> Response {
    // Use the raw path+query, not the decoded Path extractor: narinfo and NAR
    // URLs must reach nix-serve byte-identical to what the client requested.
    let raw = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or(CACHE_PREFIX);
    let suffix = raw.strip_prefix(CACHE_PREFIX).unwrap_or(raw);
    let target = if suffix.starts_with('/') {
        suffix.to_string()
    } else {
        format!("/{suffix}")
    };
    forward(&state, &target, req).await
}

async fn forward(state: &ProxyState, target: &str, req: http::Request<Body>) -> Response {
    let (parts, body) = req.into_parts();
    let uri = format!("{}{}", state.upstream, target);
    let mut builder = http::Request::builder().method(parts.method).uri(uri);
    for (name, value) in parts.headers.iter() {
        if name == http::header::HOST || is_hop_by_hop(name) {
            continue;
        }
        builder = builder.header(name, value);
    }
    let request = match builder.body(body) {
        Ok(request) => request,
        Err(e) => return bad_gateway(format!("building proxied request: {e}")),
    };
    match state.client.request(request).await {
        Ok(response) => {
            let (parts, incoming) = response.into_parts();
            Response::from_parts(parts, Body::new(incoming))
        }
        Err(e) => bad_gateway(format!("cache upstream {e}")),
    }
}

fn bad_gateway(detail: String) -> Response {
    log::warn!("proxying to nix-serve failed: {detail}");
    (
        axum::http::StatusCode::BAD_GATEWAY,
        format!("local binary cache unavailable: {detail}\n"),
    )
        .into_response()
}

/// Absolute substituter URL clients should configure, given the public base
/// URL of this flakemill instance (from `PUBLIC_URL`, e.g.
/// `https://flakemill.example/cache`).
pub fn substituter_url(base: &str) -> String {
    format!("{}{CACHE_PREFIX}", base.trim_end_matches('/'))
}

/// How to use the local binary cache, for logs and the dashboard.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct CacheInfo {
    /// Endpoint of the cache, e.g. `https://mill.example/cache`.
    pub url: String,
    /// The same endpoint as a substituter, pinned ahead of the public caches
    /// (see `CACHE_PRIORITY` in the README's cache docs).
    pub substituter: String,
    /// Cache signing key, the same text `GET /cache.pub` serves.
    pub public_key: String,
}

/// Query that pins the mill ahead of `cache.nixos.org` (priority 40) and
/// Cachix (41), so nix asks the mill for a path before anywhere else.
const SUBSTITUTER_PRIORITY: &str = "?priority=10";

/// Describe the running cache, or `None` when it is off, its public key is
/// missing, or no `PUBLIC_URL` was configured (flakemill cannot then build a
/// URL clients can reach, and must not guess one).
pub fn cache_info(cfg: &Cfg, active: bool) -> Option<CacheInfo> {
    if !active || cfg.public_url.trim().is_empty() {
        return None;
    }
    let public_key = std::fs::read_to_string(public_key_path(&cfg.serve_dir)).ok()?;
    let public_key = public_key.trim().to_string();
    if public_key.is_empty() {
        return None;
    }
    let url = substituter_url(&cfg.public_url);
    Some(CacheInfo {
        substituter: format!("{url}{SUBSTITUTER_PRIORITY}"),
        url,
        public_key,
    })
}

/// Where the public half of the cache signing key is published within
/// `serve_dir`.
pub fn public_key_path(serve_dir: &Path) -> PathBuf {
    serve_dir.join("cache.pub")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{test_cfg, TempDir};
    use axum::body::Body;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// Mock upstream that echoes what it received, standing in for nix-serve.
    fn echo_router() -> Router {
        Router::new().fallback(|req: http::Request<Body>| async move {
            let path_and_query = req
                .uri()
                .path_and_query()
                .map(|pq| pq.as_str().to_string())
                .unwrap_or_default();
            format!("echo:{path_and_query}")
        })
    }

    async fn spawn_echo() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, echo_router()).await.unwrap();
        });
        format!("http://{addr}")
    }

    fn request(uri: &str) -> http::Request<Body> {
        http::Request::get(uri).body(Body::empty()).unwrap()
    }

    async fn body_string(response: Response) -> String {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[test]
    fn endpoint_formats_listen_addr_and_upstream() {
        let endpoint = Endpoint {
            host: "127.0.0.1".into(),
            port: 4321,
        };
        assert_eq!(endpoint.addr(), "127.0.0.1:4321");
        assert_eq!(endpoint.upstream(), "http://127.0.0.1:4321");
        let v6 = Endpoint::from_socket("[::1]:4321");
        assert_eq!(
            v6,
            Some(Endpoint {
                host: "[::1]".into(),
                port: 4321,
            })
        );
        assert_eq!(Endpoint::from_socket("nonsense"), None);
    }

    #[test]
    fn reserve_port_claims_a_fresh_ephemeral_port() {
        let first = reserve_port("127.0.0.1").unwrap();
        let second = reserve_port("127.0.0.1").unwrap();
        assert_eq!(first.host, "127.0.0.1");
        assert_ne!(first.port, 0);
        assert_ne!(first.port, second.port, "each start claims its own port");
    }

    #[test]
    fn probe_sends_a_cache_info_request_and_wants_http_back() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            stream.read_to_string(&mut request).unwrap();
            stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").unwrap();
            request
        });
        let stream = std::net::TcpStream::connect(addr).unwrap();
        assert!(probe(stream).is_ok());
        assert!(
            server.join().unwrap().starts_with("GET /nix-cache-info"),
            "probe must ask for nix-cache-info"
        );
    }

    #[test]
    fn probe_rejects_an_upstream_that_is_not_http() {
        // A stale process on the port must not pass as our cache.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(b"ssh-2.0-libssh").unwrap();
        });
        let stream = std::net::TcpStream::connect(addr).unwrap();
        assert!(probe(stream).is_err());
        let _ = server.join();
    }

    #[test]
    fn wait_ready_fails_fast_when_the_child_is_gone() {
        let endpoint = reserve_port("127.0.0.1").unwrap();
        let started = Instant::now();
        // Nothing answers and the child already exited: no reason to spend the
        // remaining timeout discovering that again.
        assert!(!wait_ready(&endpoint, Duration::from_secs(30), || Ok(
            false
        )));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "should not wait out the timeout"
        );
    }

    #[test]
    fn wait_ready_gives_up_when_nothing_listens() {
        let endpoint = reserve_port("127.0.0.1").unwrap();
        let started = Instant::now();
        assert!(!wait_ready(&endpoint, Duration::from_millis(150), || Ok(
            true
        )));
        assert!(started.elapsed() < Duration::from_secs(5), "must not hang");
    }

    #[test]
    fn wait_ready_tolerates_a_slow_to_bind_child() {
        let reserved = reserve_port("127.0.0.1").unwrap();
        let endpoint = reserved.clone();
        let server = std::thread::spawn(move || {
            // Stand in for nix-serve taking a moment to claim its port.
            std::thread::sleep(Duration::from_millis(120));
            let listener =
                std::net::TcpListener::bind((endpoint.host.as_str(), endpoint.port)).unwrap();
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").unwrap();
        });
        assert!(wait_ready(&reserved, Duration::from_secs(5), || Ok(true)));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn proxy_strips_prefix_and_preserves_raw_path_and_query() {
        let upstream = spawn_echo().await;
        let app = proxy_router(&upstream);

        let response = app
            .clone()
            .oneshot(request("/cache/nix-cache-info"))
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(body_string(response).await, "echo:/nix-cache-info");

        // Raw percent-encoding and query must reach the upstream unchanged.
        let response = app
            .clone()
            .oneshot(request("/cache/abc%2Fdef.narinfo?x=1&y=%20"))
            .await
            .unwrap();
        assert_eq!(
            body_string(response).await,
            "echo:/abc%2Fdef.narinfo?x=1&y=%20"
        );

        let response = app.oneshot(request("/cache")).await.unwrap();
        assert_eq!(body_string(response).await, "echo:/");
    }

    #[tokio::test]
    async fn proxy_forwards_method_and_filters_hop_by_hop_headers() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let app = Router::new().fallback(|req: http::Request<Body>| async move {
                format!(
                    "{} {} connection={:?} host={:?}",
                    req.method(),
                    req.uri().path(),
                    req.headers().get("connection"),
                    req.headers().get("host"),
                )
            });
            axum::serve(listener, app).await.unwrap();
        });
        let app = proxy_router(&format!("http://{addr}"));

        let post = http::Request::post("/cache/nix-cache-info")
            .header("connection", "keep-alive")
            .header("host", "flakemill.example")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(post).await.unwrap();
        let body = body_string(response).await;
        // The method and full path survive the proxy; hop-by-hop headers and
        // the inbound Host do not (hyper sets Host from the upstream URI).
        assert!(body.starts_with("POST /nix-cache-info"), "{body}");
        assert!(body.contains("connection=None"), "{body}");
        assert!(!body.contains("flakemill.example"), "{body}");
        assert!(body.contains(&format!("host=Some(\"{addr}\")")), "{body}");
    }

    #[tokio::test]
    async fn proxy_returns_502_when_upstream_is_down() {
        // Bind and immediately drop to grab a port nothing listens on.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let app = proxy_router(&format!("http://127.0.0.1:{port}"));

        let response = app.oneshot(request("/cache/nix-cache-info")).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn proxy_streams_large_bodies_without_buffering_errors() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let app = Router::new().fallback(|| async { vec![b'x'; 4 * 1024 * 1024] });
            axum::serve(listener, app).await.unwrap();
        });
        let app = proxy_router(&format!("http://{addr}"));
        let response = app.oneshot(request("/cache/nar/big.nar")).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .len(),
            4 * 1024 * 1024
        );
    }

    #[test]
    fn substituter_url_appends_cache_prefix_once() {
        assert_eq!(
            substituter_url("https://mill.example/"),
            "https://mill.example/cache"
        );
        assert_eq!(
            substituter_url("http://127.0.0.1:8080"),
            "http://127.0.0.1:8080/cache"
        );
    }

    #[test]
    fn cache_info_needs_an_active_cache_and_a_public_url() {
        let dir = TempDir::new("cache-info");
        std::fs::write(dir.join("cache.pub"), b"flakemill-cache:abc\n").unwrap();
        let cfg = Cfg {
            public_url: "https://mill.example/".into(),
            ..test_cfg(&dir)
        };
        let info = cache_info(&cfg, true).expect("active cache with a public url");
        assert_eq!(info.url, "https://mill.example/cache");
        assert_eq!(info.substituter, "https://mill.example/cache?priority=10");
        assert_eq!(info.public_key, "flakemill-cache:abc");

        // Inactive, or no PUBLIC_URL to build a reachable URL from: no info.
        assert_eq!(cache_info(&cfg, false), None);
        let no_base = Cfg {
            public_url: String::new(),
            ..cfg.clone()
        };
        assert_eq!(cache_info(&no_base, true), None);
        // A key file that is missing or empty is not a usable cache either.
        std::fs::write(dir.join("cache.pub"), b"").unwrap();
        assert_eq!(cache_info(&cfg, true), None);
    }

    #[test]
    fn public_key_lives_beneath_serve_dir() {
        let dir = TempDir::new("cache-pubkey-path");
        assert_eq!(public_key_path(&dir), dir.join("cache.pub"));
    }

    /// The real cache needs `nix` to make a signing key and `nix-serve` to
    /// serve the store; images with neither skip these paths.
    fn real_cache_tools() -> (bool, bool) {
        (which("nix").is_some(), which("nix-serve").is_some())
    }

    #[test]
    fn start_publishes_key_and_serves_on_its_own_port() {
        let (have_nix, have_nix_serve) = real_cache_tools();
        if !have_nix {
            return;
        }
        let dir = TempDir::new("cache-start");
        let cfg = Cfg {
            cache_key: dir.join("keys/cache"),
            ..test_cfg(&dir)
        };
        let started = start(&cfg, "127.0.0.1").unwrap();
        let public = std::fs::read_to_string(dir.join("cache.pub")).unwrap();
        assert!(public.starts_with("flakemill-cache:"), "{public}");
        assert!(!std::fs::read_to_string(&cfg.cache_key).unwrap().is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&cfg.cache_key)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "secret key must be private");
        }

        let Some((service, endpoint)) = started else {
            assert!(
                !have_nix_serve,
                "nix-serve is on PATH but never came up: start() reported no cache \
                 (run `nix-serve --help` to check the binary works here)"
            );
            return;
        };
        // The endpoint is the port the child actually answers on, so the proxy
        // can never point at a stale nix-serve left behind by an earlier run.
        assert!(wait_ready(&endpoint, Duration::from_secs(1), || Ok(true)));

        let again = start(&cfg, "127.0.0.1")
            .unwrap()
            .expect("second start also serves");
        assert_eq!(
            std::fs::read_to_string(dir.join("cache.pub")).unwrap(),
            public,
            "an existing key is never regenerated"
        );
        assert_ne!(again.1.port, endpoint.port, "each start claims a port");
        drop(again);
        drop(service);
    }

    #[test]
    fn dropping_the_service_stops_nix_serve() {
        let (have_nix, have_nix_serve) = real_cache_tools();
        if !have_nix || !have_nix_serve {
            return;
        }
        let dir = TempDir::new("cache-drop");
        let cfg = Cfg {
            cache_key: dir.join("keys/cache"),
            ..test_cfg(&dir)
        };
        let (service, endpoint) = start(&cfg, "127.0.0.1")
            .unwrap()
            .expect("nix-serve on PATH should serve; check `nix-serve --help`");
        drop(service);
        // Killed and reaped: the port is free again and nothing answers HTTP.
        std::net::TcpListener::bind((endpoint.host.as_str(), endpoint.port)).unwrap();
        assert!(
            !wait_ready(&endpoint, Duration::from_millis(150), || Ok(true)),
            "{} still answering after drop",
            endpoint.addr()
        );
    }

    fn which(binary: &str) -> Option<PathBuf> {
        std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths).find_map(|dir| {
                let candidate = dir.join(binary);
                candidate.is_file().then_some(candidate)
            })
        })
    }
}
