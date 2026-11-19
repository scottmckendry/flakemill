<h1 align="center">❄️ flakemill</h1>

<p align="center">
  <a href="https://flakemill.axis.scottmckendry.tech/">live example</a> ·
  <a href="#how-it-works">how it works</a> ·
  <a href="#usage">usage</a> ·
  <a href="#configuration">configuration</a> ·
  <a href="#endpoints">endpoints</a>
</p>

---

<img width="1192" height="658" alt="1794867522400066767" src="https://github.com/user-attachments/assets/a4ef57e4-6b6c-47a6-a74a-68a0b211329c" />

An external update service for [Nix flakes](https://nixos.wiki/wiki/Flakes): refresh a lockfile, build your systems, and serve the verified result for hosts to apply on their own schedule.

- **Build before publishing.** A new lock is served only after all configured systems build and their results are pushed to [Cachix](https://cachix.org).
- **Verify before applying.** Hosts can check the lock's minisign signature against a pinned public key.
- **Keep control of your repo.** flakemill never pushes changes back; it only mills and serves.
- **Substitute from the mill.** Serve the local Nix store as a signed binary cache at `/cache/*`, so hosts can pull build results directly from flakemill.

## how it works

On every tick (or manual trigger), flakemill runs:

1. **Sync** - `git fetch` + `reset --hard` to the tip of `BRANCH`
2. **Update** - `nix flake update --refresh`
3. **Build** - one `nix build` per entry in `SYSTEMS`, skipping any toplevel already present on the cache
4. **Push** - publish the new toplevels to `CACHIX_CACHE`
5. **Serve** - only if every step succeeded: atomically publish `flake.lock` + minisign signature

> A failed build never publishes a new lock. Hosts continue to use the last successfully published result.

Separately, flakemill continuously serves the local Nix store as a signed binary cache at `/cache/*`, so hosts can substitute directly from the mill. Set `LOCAL_CACHE_ENABLED=false` to disable it.

## endpoints

| Path                      | Contents                                     |
| ------------------------- | -------------------------------------------- |
| `GET /`                   | read-only fleet dashboard                    |
| `GET /ui.json`            | dashboard data (manifest + runtime state)    |
| `GET /flake.lock`         | latest published (built + pushed) lock       |
| `GET /flake.lock.minisig` | minisign signature over the lock             |
| `GET /manifest.json`      | status of the last run (rev, systems, error) |
| `GET /logs/{host}.log`    | latest captured build log for a system       |
| `GET /cache/*`            | local binary cache (narinfo, NAR)            |
| `GET /cache.pub`          | public key for the local binary cache        |
| `GET /cache-info`         | cache setup instructions for hosts           |
| `GET /healthz`            | liveness                                     |

## usage

### run the container

The published [container image](https://github.com/scottmckendry/flakemill/pkgs/container/flakemill) includes Nix, Cachix and minisign. For example, with Docker (or Podman):

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

Replace the example values and put `CACHIX_AUTH_TOKEN` in the env file. The volumes persist the Nix store and service data; mount your minisign secret key read-only and distribute its public key to clients. See [configuration](#configuration) for more options, or take a look at the [example Kubernetes deployment](https://github.com/scottmckendry/axis/blob/main/kubernetes/apps/flakemill/deployment.yaml).

### run the service and use a lock

The same binary runs the mill and acts as its client:

```sh
flakemill serve

# Exit 0 when up to date, 1 when an update is available, 2 on error.
flakemill check --url https://flakemill.example --public-key flakemill.pub

# Fetch, verify, and atomically replace ./flake.lock.
flakemill apply --url https://flakemill.example --public-key flakemill.pub

# Write a verified lock somewhere else (or to stdout without --output).
flakemill fetch --url https://flakemill.example --public-key flakemill.pub --output /tmp/flake.lock
```

The URL, public key, and local lock path can also be set with `FLAKEMILL_URL`, `FLAKEMILL_PUBLIC_KEY`, and `FLAKEMILL_LOCK`. Client commands require `curl` and `minisign` on `PATH`; the Nix package supplies both. Unsigned locks are rejected unless `--allow-unsigned` is passed explicitly.

### run with nix

```sh
nix run github:scottmckendry/flakemill -- serve
nix run github:scottmckendry/flakemill -- check --url https://flakemill.example --public-key flakemill.pub
nix build github:scottmckendry/flakemill
```

`packages.<system>.flakemill` is the shared entry point. It wraps every command the binary shells out to (`git`, `nix`, `cachix`, `minisign`, `curl`, and `nix-serve` for the local cache) into its `PATH`, so `nix run` needs nothing else installed.

## configuration

Configure the server with environment variables. Set `SIGN_KEY` to publish signed locks; clients reject unsigned locks by default.

| Variable              | Default                         | Description                                                                    |
| --------------------- | ------------------------------- | ------------------------------------------------------------------------------ |
| `REPO_URL`            |                                 | Config repo to sync and build from                                             |
| `BRANCH`              | `main`                          | Branch to track                                                                |
| `SYSTEMS`             |                                 | Comma-separated NixOS hosts to build                                           |
| `CACHIX_CACHE`        |                                 | Build cache to push results to and substitute from                             |
| `CACHIX_AUTH_TOKEN`   |                                 | Cachix push token (`secrets/.env`)                                             |
| `SIGN_KEY`            |                                 | Path to the minisign secret key (unset publishes unsigned locks)               |
| `WORK_DIR`            | `/var/lib/flakemill/work`       | Repo checkout                                                                  |
| `SERVE_DIR`           | `/var/lib/flakemill/serve`      | Published lock + signature                                                     |
| `INTERVAL_SECS`       | `21600`                         | Seconds between update runs (must be greater than zero)                        |
| `LISTEN`              | `0.0.0.0:8080`                  | HTTP listen address                                                            |
| `LOCAL_CACHE_ENABLED` | `true`                          | Serve the local Nix store as a binary cache at `/cache/*`                      |
| `CACHE_KEY`           | `/var/lib/flakemill/keys/cache` | Persistent signing key for local cache narinfos                                |
| `CACHE_LISTEN_HOST`   | `127.0.0.1`                     | Host the private `nix-serve` child binds; its port is chosen at startup        |
| `PUBLIC_URL`          |                                 | Base URL hosts reach this instance at, e.g. `https://mill.example`             |
| `NIX_CONFIG`          |                                 | Extra nix.conf lines, e.g. `access-tokens` for private inputs (`secrets/.env`) |

> [!NOTE]
> Flakemill keeps the latest published systems in the Nix store and removes unused paths after successful updates. GC affects the whole store, so take care if sharing it with other services.

### cachix setup

The container configures `CACHIX_CACHE` as both a push target and a substituter at startup, so a first build on an empty store can reuse dependency closures that are already cached. The cache must already contain those paths; a brand-new cache still requires one cold build before it can help.

When running outside the container, configure the cache yourself, either with `cachix use <cache>` or via `NIX_CONFIG`:

```sh
export NIX_CONFIG='
extra-substituters = https://<cache>.cachix.org
extra-trusted-public-keys = <cache>.cachix.org-1:...
'
```

### local binary cache

Flakemill runs [`nix-serve-ng`](https://github.com/aristanetworks/nix-serve-ng) on loopback and reverse-proxies `/cache/*` to it, so hosts can substitute directly from the mill without a second on-disk copy of the store. The child binds a port chosen at startup (nothing to configure or collide with), is stopped when flakemill shuts down, and is quiet by default so cache traffic does not drown the update logs. The cache serves local store paths only; it is not a transparent proxy for upstream caches.

> [!WARNING]
> `/cache/*` is unauthenticated and exposes every path in the mill's store — including anything derived from a private config repo. Put it behind TLS and a network boundary (or set `LOCAL_CACHE_ENABLED=false`) unless you intend the store to be readable by whoever can reach the port.

Configure hosts with the substituter URL and the key from `/cache.pub`:

```sh
# print the key, or copy it into your NixOS configuration
curl -fsS https://<mill-url>/cache.pub

export NIX_CONFIG='
extra-substituters = https://<mill-url>/cache?priority=10
extra-trusted-public-keys = <contents of /cache.pub>
'
```

Set `PUBLIC_URL` and the dashboard shows both values with copy buttons, and links `/cache-info` for the same instructions in browser form.

Narinfos are signed with a key created at `CACHE_KEY` on first start and never regenerated — delete it and every host has to trust the new `/cache.pub` again.

### which cache answers first

Nix asks its substituters in ascending order of the priority each one advertises, and the lowest wins: `cache.nixos.org` publishes 40 and Cachix publishes 41. Flakemill tells `nix-serve` to advertise 30, so the mill is asked before either — which is what you want for a system published minutes ago, before it has settled into upstream caches. The `?priority=10` in the substituter URL above is the same decision made from the host's side, so the ordering holds however the rest of that host's caches are configured.

Priority says only where to *look*. Narinfos are still signature-checked, so a host that does not trust the key refuses everything the mill offers, and no priority can talk a host into accepting a path signed by someone else.

Clients still consult `cache.nixos.org` and Cachix, so a path the mill has already collected is fetched from whichever cache still has it. The local cache does not pin extra paths either: flakemill's existing GC policy keeps only the published systems, so paths outside that closure may be gone by the next run — Cachix remains the durable push target.

Serving the cache is enabled by default. Set `LOCAL_CACHE_ENABLED=false` to switch it off; if `nix-serve` is not on `PATH`, or it fails to come up on its reserved port, flakemill logs a warning and serves locks only. In every one of those cases `/cache/*` and `/cache.pub` are unavailable and the cache rows are hidden from the dashboard.
