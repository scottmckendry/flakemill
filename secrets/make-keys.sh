#!/usr/bin/env bash
# Generate a minisign keypair for the service (dev convenience).
set -euo pipefail
mkdir -p "$(dirname "$0")/../keys"
cd "$(dirname "$0")/../keys"

if [[ -f sign.key ]]; then
  echo "keys/sign.key already exists; leaving it alone"
else
  minisign -G -s sign.key -p sign.pub -W
fi

echo
echo "Public key body for settings.fleet.updateService.publicKey:"
awk 'NR==2' sign.pub
