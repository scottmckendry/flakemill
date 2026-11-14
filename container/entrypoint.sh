#!/usr/bin/env bash
# Seed /nix from the pristine copy baked into the image when the runtime has
# mounted a volume over /nix (e.g. a k8s PVC for store persistence).
set -euo pipefail

if [ ! -e /nix/store ]; then
    echo "flakemill entrypoint: /nix is empty, seeding from /nix.seed..."
    cp -a /nix.seed/. /nix/
    echo "flakemill entrypoint: /nix seeded"
fi

exec "$@"
