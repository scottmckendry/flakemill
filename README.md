<h1 align="center">❄️ flakemill</h1>

<p align="center">
  <a href="https://flakemill.axis.scottmckendry.tech/">live example</a> ·
  <a href="#usage">usage</a> ·
  <a href="#how-it-works">how it works</a> ·
  <a href="#configuration">configuration</a> ·
  <a href="#endpoints">endpoints</a>
</p>

---

flakemill updates your NixOS `flake.lock` and publishes it once the configured hosts' builds are in [Cachix](https://cachix.org). Hosts can rebuild from the cache on their own schedule. flakemill doesn't push commits or open PRs.

<img width="1131" height="881" alt="flakemill dashboard showing build status for configured hosts" src="https://github.com/user-attachments/assets/45165ef1-2b87-4ba1-ba45-0b746834b993" />

_The flakemill dashboard shows the latest update run and build status for each host._

## usage

### run the container

Run the [container image](https://github.com/scottmckendry/flakemill/pkgs/container/flakemill) with Docker or Podman:

```sh
docker run -d --name flakemill \
  -p 8080:8080 \
  -v flakemill-nix:/nix \
  -v flakemill-data:/var/lib/flakemill \
  -v /path/to/minisign.key:/secrets/sign.key:ro \
  --env-file /path/to/flakemill.env \
  -e REPO_URL=https://github.com/example/nix-config \
  -e SYSTEMS=host1,host2 \
  -e CACHIX_CACHE=your-cache \
  -e SIGN_KEY=/secrets/sign.key \
  ghcr.io/scottmckendry/flakemill:latest
```

Replace the example values, put `CACHIX_AUTH_TOKEN` in the env file, and give clients the minisign public key. An [example Kubernetes deployment](https://github.com/scottmckendry/axis/blob/main/kubernetes/apps/flakemill/deployment.yaml) is also available.

### use a published lock

Once the server is running, run these commands on a client host:

```sh
# Exit 0 when up to date, 1 when an update is available, 2 on error.
flakemill check --url https://flakemill.example --public-key flakemill.pub

# Fetch, verify, and atomically replace ./flake.lock.
flakemill apply --url https://flakemill.example --public-key flakemill.pub

# Write a verified lock somewhere else (or to stdout without --output).
flakemill fetch --url https://flakemill.example --public-key flakemill.pub --output /tmp/flake.lock
```

`apply` only replaces the lockfile. To reuse cached builds, [configure Cachix](#cachix-setup) on each host and rebuild as usual.

The URL, public key, and local lock path can also be set with `FLAKEMILL_URL`, `FLAKEMILL_PUBLIC_KEY`, and `FLAKEMILL_LOCK`. Client commands require `curl` and `minisign` on `PATH`; the Nix package supplies both. Unsigned locks are rejected unless `--allow-unsigned` is passed explicitly.

### run with nix

To run the client without installing it:

```sh
nix run github:scottmckendry/flakemill -- check --url https://flakemill.example --public-key flakemill.pub
```

To run the server **instead of** the container, set the [server environment variables](#configuration), then run:

```sh
nix run github:scottmckendry/flakemill -- serve
```

To build the package locally:

```sh
nix build github:scottmckendry/flakemill
```

The Nix package supplies `git`, `nix`, `cachix`, `minisign`, and `curl`.

## how it works

On each update run, flakemill:

1. Fetches the repository and resets its checkout to the tip of `BRANCH`.
2. Runs `nix flake update --refresh`.
3. Builds the NixOS configuration for each host in `SYSTEMS`, skipping toplevels already in the cache.
4. Pushes new toplevels to `CACHIX_CACHE`.
5. Publishes `flake.lock` and, if `SIGN_KEY` is set, its minisign signature.

A failed build never publishes a new lock. The last successfully published lock stays available.

## configuration

Configure the server with environment variables. Set `SIGN_KEY` to publish signed locks; clients reject unsigned locks by default.

| Variable            | Default                    | Description                                                                    |
| ------------------- | -------------------------- | ------------------------------------------------------------------------------ |
| `REPO_URL`          |                            | Config repo to sync and build from                                             |
| `BRANCH`            | `main`                     | Branch to track                                                                |
| `SYSTEMS`           |                            | Comma-separated NixOS hosts to build                                           |
| `CACHIX_CACHE`      |                            | Build cache to push results to and substitute from                             |
| `CACHIX_AUTH_TOKEN` |                            | Cachix push token (`secrets/.env`)                                             |
| `SIGN_KEY`          |                            | Path to the minisign secret key (unset publishes unsigned locks)               |
| `WORK_DIR`          | `/var/lib/flakemill/work`  | Repo checkout                                                                  |
| `SERVE_DIR`         | `/var/lib/flakemill/serve` | Published lock + signature                                                     |
| `INTERVAL_SECS`     | `21600`                    | Seconds between update runs (must be greater than zero)                        |
| `LISTEN`            | `0.0.0.0:8080`             | HTTP listen address                                                            |
| `NIX_CONFIG`        |                            | Extra nix.conf lines, e.g. `access-tokens` for private inputs (`secrets/.env`) |

> [!NOTE]
> Flakemill keeps the latest published systems in the Nix store and removes unused paths after successful updates. GC affects the whole store, so take care if sharing it with other services.

### cachix setup

The container pushes to and substitutes from `CACHIX_CACHE`. On client hosts and servers running outside the container, run `cachix use <cache>` or set `NIX_CONFIG`:

```sh
export NIX_CONFIG='
extra-substituters = https://<cache>.cachix.org
extra-trusted-public-keys = <cache>.cachix.org-1:...
'
```

## endpoints

| Path                      | Contents                                    |
| ------------------------- | ------------------------------------------- |
| `GET /`                   | Read-only fleet dashboard                   |
| `GET /ui.json`            | Dashboard data (manifest and runtime state) |
| `GET /flake.lock`         | Latest published lock                       |
| `GET /flake.lock.minisig` | Minisign signature for the lock             |
| `GET /manifest.json`      | Last run status (revision, systems, error)  |
| `GET /logs/{host}.log`    | Latest captured build log for a host        |
| `GET /healthz`            | Liveness check                              |
