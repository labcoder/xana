#!/usr/bin/env bash
# Offline behavioral and failure matrix for the reviewed Bash installer.

set -euo pipefail
IFS=$'\n\t'
umask 077

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
test_root=$(mktemp -d "${TMPDIR:-/tmp}/xana-install-sh-test.XXXXXXXX")
case "${test_root}" in "${TMPDIR:-/tmp}"/xana-install-sh-test.*) ;; *) exit 1 ;; esac
trap 'rm -rf -- "${test_root}"' EXIT INT TERM HUP

fixture="${test_root}/fixture"
payload="${test_root}/payload"
home="${test_root}/home"
install_dir="${test_root}/in'stall"
mkdir -p -- "${fixture}" "${payload}" "${home}"

version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\([0-9][0-9.]*\)"/\1/p' "${repo_root}/Cargo.toml" | head -n 1)
[[ "${version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { printf 'could not read workspace version\n' >&2; exit 1; }

cat >"${payload}/xana" <<EOF
#!/usr/bin/env bash
case "\${1:-}" in
  --version) printf 'xana ${version}\\n' ;;
  --help) printf 'fake Xana help\\n' ;;
  setup)
    printf 'XANA_SETUP_RESULT {"version":1,"status":"pending","reason":"missing","next":"xana setup","diagnose":"xana doctor"}\\n'
    exit 10
    ;;
  *) exit 1 ;;
esac
EOF
chmod 755 "${payload}/xana"
cp -- "${repo_root}/LICENSE" "${payload}/LICENSE"
cp -- "${repo_root}/README.md" "${payload}/README.md"
cp -- "${repo_root}/docs/user/installation.md" "${payload}/installation.md"

make_archive() {
  local target=$1 include_extra=${2:-false}
  local archive_root="xana-${target}"
  local root_path="${payload}/${archive_root}"
  mkdir -p -- "${root_path}"
  cp -- "${payload}/LICENSE" "${root_path}/LICENSE"
  cp -- "${payload}/README.md" "${root_path}/README.md"
  cp -- "${payload}/installation.md" "${root_path}/installation.md"
  cp -- "${payload}/xana" "${root_path}/xana"
  rm -f -- "${root_path}/extra"

  local entries=(
    "${archive_root}/LICENSE"
    "${archive_root}/README.md"
    "${archive_root}/installation.md"
    "${archive_root}/xana"
  )
  if [[ "${include_extra}" == "true" ]]; then
    printf 'unexpected' >"${root_path}/extra"
    entries+=("${archive_root}/extra")
  fi
  tar -czf "${fixture}/xana-${target}.tar.gz" -C "${payload}" "${entries[@]}"
}

for target in aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-gnu; do
  make_archive "${target}"
done
printf 'fixture zip bytes' >"${fixture}/xana-x86_64-pc-windows-msvc.zip"

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

write_manifest() {
  {
    printf 'manifest-version 1\n'
    printf 'release-version %s\n' "${version}"
    for target in aarch64-apple-darwin x86_64-apple-darwin x86_64-pc-windows-msvc x86_64-unknown-linux-gnu; do
      if [[ "${target}" == "x86_64-pc-windows-msvc" ]]; then extension=.zip; else extension=.tar.gz; fi
      name="xana-${target}${extension}"
      size=$(wc -c <"${fixture}/${name}" | tr -d ' ')
      printf 'artifact %s %s %s %s\n' "${target}" "${name}" "$(sha256_file "${fixture}/${name}")" "${size}"
    done
  } >"${fixture}/xana-release-manifest.txt"
}
write_manifest

run_installer() {
  HOME="${home}" SHELL=/bin/bash PATH="${PATH}" \
    bash "${repo_root}/install/install.sh" \
      --allow-test-fixture \
      --test-fixture-root "${fixture}" \
      --test-target x86_64-unknown-linux-gnu \
      --install-dir "${install_dir}" \
      "$@"
}

first_output=$(run_installer --version "${version}" --no-setup --no-modify-path)
grep -F "Xana install: ${version}" <<<"${first_output}" >/dev/null
[[ -x "${install_dir}/xana" ]]
"${install_dir}/xana" --help >/dev/null
installed_hash=$(sha256_file "${install_dir}/xana")

second_output=$(run_installer --version "${version}" --no-setup --no-modify-path)
grep -F "Xana reinstall: ${version}" <<<"${second_output}" >/dev/null

path_output=$(run_installer --version "${version}" --no-setup --modify-path)
grep -F "xana-installer-path-v1" "${home}/.profile" >/dev/null
bash -n "${home}/.profile"
[[ "$(grep -Fc 'xana-installer-path-v1' "${home}/.profile")" -eq 1 ]]
run_installer --version "${version}" --no-setup --modify-path >/dev/null
[[ "$(grep -Fc 'xana-installer-path-v1' "${home}/.profile")" -eq 1 ]]
grep -F "start a new shell" <<<"${path_output}" >/dev/null

setup_output=$(run_installer --version "${version}" --no-modify-path)
grep -F "setup=pending" <<<"${setup_output}" >/dev/null
grep -F "XANA_SETUP_RESULT" <<<"${setup_output}" >/dev/null

printf 'corruption' >>"${fixture}/xana-x86_64-unknown-linux-gnu.tar.gz"
if run_installer --version "${version}" --no-setup --no-modify-path >"${test_root}/failure.out" 2>&1; then
  printf 'corrupt archive unexpectedly installed\n' >&2
  exit 1
fi
[[ "$(sha256_file "${install_dir}/xana")" == "${installed_hash}" ]]

make_archive x86_64-unknown-linux-gnu true
write_manifest
if run_installer --version "${version}" --no-setup --no-modify-path >"${test_root}/unsafe.out" 2>&1; then
  printf 'unexpected archive inventory installed\n' >&2
  exit 1
fi
[[ "$(sha256_file "${install_dir}/xana")" == "${installed_hash}" ]]

if HOME="${home}" bash "${repo_root}/install/install.sh" \
  --allow-test-fixture --test-fixture-root "${fixture}" --test-target aarch64-unknown-linux-gnu \
  --install-dir "${install_dir}" --no-setup --no-modify-path >"${test_root}/target.out" 2>&1; then
  printf 'unsupported target unexpectedly installed\n' >&2
  exit 1
fi
[[ "$(sha256_file "${install_dir}/xana")" == "${installed_hash}" ]]

if grep -R -F 'setup-secret-sentinel' "${test_root}" >/dev/null 2>&1; then
  printf 'seeded secret appeared in installer state\n' >&2
  exit 1
fi

printf 'Bash installer verified: install/reinstall, exact version, PATH idempotence, setup pending, checksum, archive inventory, unsupported target, rollback\n'
