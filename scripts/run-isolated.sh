#!/usr/bin/env bash

set -euo pipefail

usage() {
    cat >&2 <<'USAGE'
Usage: scripts/run-isolated.sh INSTALL_ROOT [XANA_ARGS...]

Build and install Xana into INSTALL_ROOT, then run the installed binary from
a temporary workspace with a temporary XANA_HOME. Temporary state is removed
when Xana exits. The first argument is never passed to Xana.

Example:
  scripts/run-isolated.sh /tmp/xana-install --help
USAGE
}

if [ "$#" -lt 1 ]; then
    usage
    exit 64
fi

install_root=$1
shift

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
mkdir -p -- "$install_root"
install_root=$(cd -- "$install_root" && pwd -P)

cargo install --path "$repo_root/crates/xana-cli" --locked --root "$install_root"

state_root=$(mktemp -d)
workspace=$(mktemp -d)

cleanup() {
    rm -rf -- "$state_root" "$workspace"
}
trap cleanup EXIT INT TERM

binary="$install_root/bin/xana"
if [ ! -x "$binary" ]; then
    printf 'installed Xana binary was not found at %s\n' "$binary" >&2
    exit 1
fi

(
    cd -- "$workspace"
    XANA_HOME="$state_root" exec "$binary" "$@"
)
