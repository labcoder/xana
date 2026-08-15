#!/usr/bin/env bash
set -euo pipefail

readonly cargo_dist_version="0.32.0"
readonly installer_sha256="b657cf8c04a8b7bc28f39d220f7e6dd11bbd2bdb072c552262bd9ccf597261b5"
installer="$(mktemp)"
trap 'rm -f -- "$installer"' EXIT

curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error \
  "https://github.com/axodotdev/cargo-dist/releases/download/v${cargo_dist_version}/cargo-dist-installer.sh" \
  --output "$installer"
bash ./scripts/verify-sha256.sh "$installer" "$installer_sha256"
sh "$installer"
