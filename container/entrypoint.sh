#!/usr/bin/env bash
# Seed /nix from the pristine copy baked into the image when the runtime has
# mounted a volume over /nix (e.g. a k8s PVC for store persistence).
#
# nix installs into /nix, and a volume mounted there hides the image's
# preinstalled store. Container runtimes differ in seeding behaviour: docker/
# podman named volumes are populated from image content on first use, but
# Kubernetes PVCs are not — resulting in a bare, nix-less mount. Keeping
# /nix.seed in the image and copying it out at startup makes volume-backed
# stores work everywhere, and costs nothing when /nix is already populated.
set -euo pipefail

if [ ! -e /nix/store ]; then
    echo "flakemill entrypoint: /nix is empty, seeding from /nix.seed..."
    cp -a /nix.seed/. /nix/
    echo "flakemill entrypoint: /nix seeded"
fi

exec "$@"
