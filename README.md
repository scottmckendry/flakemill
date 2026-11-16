# flakemill

An external update service for [nix flakes](https://nixos.wiki/wiki/Flakes).

`flakemill` pulls the latest revision of your config repo, refreshes `flake.lock`, builds every configured system, pushes the results to a [cachix](https://cachix.org) cache, and serves a **minisign-signed** lockfile over HTTP.

Hosts in the fleet then fetch the lock, verify its signature against a pinned public key, and apply it on their own schedule.

The mill complements the source repo's update process but never controls it. Nothing is ever pushed back to the repo - it only ever mills and serves.

## How it works

On every tick (or manual trigger), flakemill runs:

1. **Sync** - `git fetch` + `reset --hard` to the tip of `BRANCH`
2. **Update** - `nix flake update --refresh`
3. **Build** - one `nix build` per entry in `SYSTEMS`, skipping any toplevel
   already present on the cache
4. **Push** - publish the new toplevels to `CACHIX_CACHE`
5. **Serve** - only if every step succeeded: atomically publish
   `flake.lock` + minisign signature

A failed build never publishes a lock, so hosts never pick up a broken fleet.

## Endpoints

| Path                      | Contents                                     |
| ------------------------- | -------------------------------------------- |
| `GET /`                   | read-only fleet dashboard                    |
| `GET /ui.json`            | dashboard data (manifest + runtime state)    |
| `GET /flake.lock`         | latest published (built + pushed) lock       |
| `GET /flake.lock.minisig` | minisign signature over the lock             |
| `GET /manifest.json`      | status of the last run (rev, systems, error) |
| `GET /logs/{host}.log`    | latest captured build log for a system       |
| `GET /healthz`            | liveness                                     |

## Usage

`flakemill` is a single entry point. Run the service with `serve`, or use the
same binary as a client:

```sh
flakemill serve

# Exit 0 when up to date, 1 when an update is available, 2 on error.
flakemill check --url https://flakemill.example --public-key flakemill.pub

# Fetch, verify, and atomically replace ./flake.lock.
flakemill apply --url https://flakemill.example --public-key flakemill.pub

# Write a verified lock somewhere else (or to stdout without --output).
flakemill fetch --url https://flakemill.example --public-key flakemill.pub --output /tmp/flake.lock
```

The URL, public key, and local lock path can also be set with
`FLAKEMILL_URL`, `FLAKEMILL_PUBLIC_KEY`, and `FLAKEMILL_LOCK`. Client commands
require `curl` and `minisign` on `PATH`; the Nix package supplies both.
Unsigned locks are rejected unless `--allow-unsigned` is passed explicitly.

## Flake outputs

```sh
nix run github:scottmckendry/flakemill -- serve
nix run github:scottmckendry/flakemill -- check --url https://flakemill.example --public-key flakemill.pub
nix build github:scottmckendry/flakemill
```

`packages.<system>.flakemill` is the shared entry point.

## Configuration

Server configuration is environment-based:

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
| `INTERVAL_SECS`     | `21600`                    | Seconds between update runs (must be greater than zero)                         |
| `LISTEN`            | `0.0.0.0:8080`             | HTTP listen address                                                            |
| `NIX_CONFIG`        |                            | Extra nix.conf lines, e.g. `access-tokens` for private inputs (`secrets/.env`) |

The container configures `CACHIX_CACHE` as both a push target and a substituter
at startup, so a first build on an empty store can reuse dependency closures
that are already cached. The cache must already contain those paths; a
brand-new cache still requires one cold build before it can help.

When running outside the container, configure the cache yourself, either with
`cachix use <cache>` or via `NIX_CONFIG`:

```sh
export NIX_CONFIG='
extra-substituters = https://<cache>.cachix.org
extra-trusted-public-keys = <cache>.cachix.org-1:...
'
```
