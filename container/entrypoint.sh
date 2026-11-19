#!/usr/bin/env bash
# Seed /nix from the pristine copy baked into the image when the runtime has
# mounted a volume over /nix (e.g. a k8s PVC for store persistence).
set -euo pipefail

# The volume can outlive the image that seeded it. A previous Nix install may
# have left /nix/store populated while its profile links point at store paths
# that are no longer present (or the volume may have been only partly seeded).
# Check that the commands we actually need are runnable, rather than just
# assuming that an existing /nix/store is complete. nix-serve backs the local
# binary cache: without it flakemill still runs, but serves locks only.
# NOTE: the image installs it into root's Nix profile, so `nix profile remove`
# or `wipe-history` inside the container takes it away again.
if [ ! -e /nix/store ] ||
    [ ! -x /nix/var/nix/profiles/default/bin/nix ] ||
    [ ! -x /root/.nix-profile/bin/cachix ] ||
    [ ! -x /nix/var/nix/profiles/default/bin/nix-serve ]; then
    echo "flakemill entrypoint: /nix is missing or incomplete, seeding from /nix.seed..."
    cp -a /nix.seed/. /nix/
    echo "flakemill entrypoint: /nix seeded"
fi

# Configure the target cache as a substituter before the first build. Without
# this, nix build only consults cache.nixos.org and a fresh store rebuilds
# dependencies that are already available on cachix. root-nixconf is correct
# inside the container; the default mode may try to edit NixOS configuration.
if [ -n "${CACHIX_CACHE:-}" ]; then
    if ! USER=root cachix use --mode root-nixconf "$CACHIX_CACHE"; then
        echo "flakemill entrypoint: warning: could not configure ${CACHIX_CACHE} as a substituter" >&2
    fi
fi

exec "$@"
