#!/usr/bin/env bash

set -euo pipefail
IFS=$'\n\t'

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
test_root=$(mktemp -d "${TMPDIR:-/tmp}/xana-release-sha-test.XXXXXXXX")
case "${test_root}" in "${TMPDIR:-/tmp}"/xana-release-sha-test.*) ;; *) exit 1 ;; esac
trap 'rm -rf -- "${test_root}"' EXIT INT TERM HUP

fixture=${test_root}/fixture
printf 'portable release checksum fixture' >"${fixture}"
if command -v sha256sum >/dev/null 2>&1; then
  expected=$(sha256sum "${fixture}" | awk '{print $1}')
elif command -v shasum >/dev/null 2>&1; then
  expected=$(shasum -a 256 "${fixture}" | awk '{print $1}')
else
  expected=$(openssl dgst -sha256 "${fixture}" | awk '{print $NF}')
fi

bash "${repo_root}/scripts/verify-sha256.sh" "${fixture}" "${expected}"

fake_bin=${test_root}/bin
mkdir -p -- "${fake_bin}"
cat >"${fake_bin}/sha256sum" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == "--check" ]]; then
  printf 'usage: sha256sum [-bctwz] [files ...]\n' >&2
  exit 1
fi
printf '%s  %s\n' "${FAKE_SHA256}" "$1"
EOF
chmod 755 "${fake_bin}/sha256sum"
PATH="${fake_bin}:${PATH}" FAKE_SHA256="${expected}" \
  bash "${repo_root}/scripts/verify-sha256.sh" "${fixture}" "${expected}"

if bash "${repo_root}/scripts/verify-sha256.sh" "${fixture}" "$(printf '0%.0s' {1..64})" \
    >"${test_root}/mismatch.out" 2>&1; then
  printf 'checksum mismatch was accepted\n' >&2
  exit 1
fi
if bash "${repo_root}/scripts/verify-sha256.sh" "${fixture}" invalid \
    >"${test_root}/invalid.out" 2>&1; then
  printf 'invalid expected checksum was accepted\n' >&2
  exit 1
fi

installer_call='bash ./scripts/install-cargo-dist.sh'
if [[ $(grep -Fc "${installer_call}" "${repo_root}/.github/workflows/release.yml") -ne 2 ]]; then
  printf 'release workflow does not use the shared cargo-dist installer in both jobs\n' >&2
  exit 1
fi

verifier_call='bash ./scripts/verify-sha256.sh "$installer" "$installer_sha256"'
if [[ $(grep -Fc "${verifier_call}" "${repo_root}/scripts/install-cargo-dist.sh") -ne 1 ]]; then
  printf 'shared cargo-dist installer does not use the portable verifier exactly once\n' >&2
  exit 1
fi

printf 'release SHA-256 verification verified: portable tool behavior and mismatch rejection\n'
