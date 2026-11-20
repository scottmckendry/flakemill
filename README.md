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

## how it works

On every tick (or manual trigger), flakemill runs:

1. **Sync** - `git fetch` + `reset --hard` to the tip of `BRANCH`
2. **Update** - `nix flake update --refresh`
3. **Build** - one `nix build` per entry in `SYSTEMS`, skipping any toplevel already present on the cache
4. **Push** - publish the new toplevels to `CACHIX_CACHE`
5. **Serve** - only if every step succeeded: atomically publish `flake.lock` + minisign signature

> A failed build never publishes a new lock. Hosts continue to use the last successfully published result.

## endpoints

| Path                      | Contents                                     |
| ------------------------- | -------------------------------------------- |
| `GET /`                   | read-only fleet dashboard                    |
| `GET /ui.json`            | dashboard data (manifest + runtime state)    |
| `GET /flake.lock`         | latest published (built + pushed) lock       |
| `GET /flake.lock.minisig` | minisign signature over the lock             |
| `GET /manifest.json`      | status of the last run (rev, systems, error) |
| `GET /logs/{host}.log`    | latest captured build log for a system       |
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

`packages.<system>.flakemill` is the shared entry point. It wraps every command the binary shells out to (`git`, `nix`, `cachix`, `minisign`, `curl`) into its `PATH`, so `nix run` needs nothing else installed.

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
