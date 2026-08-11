#!/usr/bin/env bash

set -euo pipefail
IFS=$'\n\t'

if [[ $# -ne 2 ]]; then
  printf 'usage: verify-sha256.sh FILE EXPECTED_SHA256\n' >&2
  exit 2
fi

file=$1
expected=$2
[[ -f "${file}" ]] || { printf 'SHA-256 input is not a regular file\n' >&2; exit 1; }
[[ "${expected}" =~ ^[0-9a-fA-F]{64}$ ]] || { printf 'expected SHA-256 is invalid\n' >&2; exit 1; }

if command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum "${file}" | awk '{print $1}')
elif command -v shasum >/dev/null 2>&1; then
  actual=$(shasum -a 256 "${file}" | awk '{print $1}')
elif command -v openssl >/dev/null 2>&1; then
  actual=$(openssl dgst -sha256 "${file}" | awk '{print $NF}')
else
  printf 'sha256sum, shasum, or openssl is required\n' >&2
  exit 1
fi

actual=$(printf '%s' "${actual}" | tr 'A-F' 'a-f')
expected=$(printf '%s' "${expected}" | tr 'A-F' 'a-f')
if [[ "${actual}" != "${expected}" ]]; then
  printf 'SHA-256 mismatch\n' >&2
  exit 1
fi
