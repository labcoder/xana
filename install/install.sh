#!/usr/bin/env bash
# Xana Release Preview installer for macOS and glibc x86_64 Linux.

set -euo pipefail
IFS=$'\n\t'
umask 077

readonly REPOSITORY="labcoder/xana"
readonly MANIFEST_NAME="xana-release-manifest.txt"
readonly MANIFEST_LIMIT=65536
readonly ARCHIVE_LIMIT=134217728
readonly PATH_MARKER="xana-installer-path-v1"

requested_version="latest"
install_dir="${HOME}/.local/bin"
setup_mode="run"
path_mode="ask"
test_fixture_root=""
test_authority="false"
test_target=""
temporary_dir=""
stage_dir=""
backup_path=""
created_install_dir="false"
activation_committed="false"
activation_started="false"
had_previous="false"
final_path=""

usage() {
  cat <<'EOF'
Install or update Xana from a verified Release Preview archive.

Usage: install.sh [options]

  --version VERSION       Install an exact X.Y.Z release (default: latest)
  --install-dir DIR       Install xana in DIR (default: ~/.local/bin)
  --no-setup              Do not run `xana setup --if-needed`
  --modify-path           Add the install directory to a user shell profile
  --no-modify-path        Never change a shell profile
  -h, --help              Show this help

The installer never uses sudo, never treats XANA_HOME as an executable path,
and never installs Rust, Node, or Python. Preview binaries are not signed or
notarized. Re-run this installer to update, reinstall, or select an older
release.
EOF
}

fail() {
  printf 'Xana installer error: %s\n' "$*" >&2
  exit 1
}

cleanup() {
  local status=$?
  trap - EXIT INT TERM HUP
  if [[ "${activation_started}" == "true" && "${activation_committed}" != "true" && -n "${final_path}" ]]; then
    if [[ "${had_previous}" == "true" && -n "${backup_path}" && -f "${backup_path}" ]]; then
      mv -f -- "${backup_path}" "${final_path}" 2>/dev/null || true
      backup_path=""
    elif [[ "${had_previous}" != "true" ]]; then
      rm -f -- "${final_path}" 2>/dev/null || true
    fi
  fi
  if [[ -n "${stage_dir}" && -n "${install_dir}" && "${stage_dir}" == "${install_dir}/.xana-stage."* && -d "${stage_dir}" ]]; then
    rm -rf -- "${stage_dir}"
  fi
  case "${temporary_dir}" in
    "${TMPDIR:-/tmp}"/xana-install.*)
      [[ ! -d "${temporary_dir}" ]] || rm -rf -- "${temporary_dir}"
      ;;
  esac
  if [[ -n "${backup_path}" && -n "${install_dir}" && "${backup_path}" == "${install_dir}/.xana-backup."* && -f "${backup_path}" ]]; then
    rm -f -- "${backup_path}"
  fi
  if [[ "${created_install_dir}" == "true" && -d "${install_dir}" ]]; then
    rmdir -- "${install_dir}" 2>/dev/null || true
  fi
  exit "${status}"
}

trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

while (($# > 0)); do
  case "$1" in
    --version)
      (($# >= 2)) || fail "--version requires a value"
      requested_version=$2
      shift 2
      ;;
    --install-dir)
      (($# >= 2)) || fail "--install-dir requires a value"
      install_dir=$2
      shift 2
      ;;
    --no-setup)
      setup_mode="skip"
      shift
      ;;
    --modify-path)
      [[ "${path_mode}" != "never" ]] || fail "--modify-path conflicts with --no-modify-path"
      path_mode="always"
      shift
      ;;
    --no-modify-path)
      [[ "${path_mode}" != "always" ]] || fail "--no-modify-path conflicts with --modify-path"
      path_mode="never"
      shift
      ;;
    --allow-test-fixture)
      test_authority="true"
      shift
      ;;
    --test-fixture-root)
      (($# >= 2)) || fail "--test-fixture-root requires a value"
      test_fixture_root=$2
      shift 2
      ;;
    --test-target)
      (($# >= 2)) || fail "--test-target requires a value"
      test_target=$2
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail "unknown option: $1"
      ;;
  esac
done

if [[ "${requested_version}" != "latest" && ! "${requested_version}" =~ ^[0-9]{1,9}\.[0-9]{1,9}\.[0-9]{1,9}$ ]]; then
  fail "version must be 'latest' or an exact X.Y.Z semantic version"
fi
[[ -n "${install_dir}" ]] || fail "install directory cannot be empty"
[[ "${install_dir}" != *$'\n'* && "${install_dir}" != *$'\r'* ]] || fail "install directory contains a line break"
[[ "${install_dir}" != *:* ]] || fail "install directory cannot contain ':' on macOS or Linux"

if [[ -n "${test_fixture_root}" || -n "${test_target}" || "${test_authority}" == "true" ]]; then
  [[ "${test_authority}" == "true" && -n "${test_fixture_root}" && -n "${test_target}" ]] ||
    fail "test fixtures require --allow-test-fixture, --test-fixture-root, and --test-target together"
  [[ "${test_fixture_root}" == /* ]] || fail "test fixture root must be an absolute local path"
  printf 'TEST ONLY: using local fixture authority %s\n' "${test_fixture_root}"
fi

detect_target() {
  if [[ -n "${test_target}" ]]; then
    printf '%s\n' "${test_target}"
    return
  fi

  local kernel machine
  kernel=$(uname -s)
  machine=$(uname -m)
  case "${kernel}:${machine}" in
    Darwin:arm64|Darwin:aarch64)
      printf '%s\n' "aarch64-apple-darwin"
      ;;
    Darwin:x86_64)
      printf '%s\n' "x86_64-apple-darwin"
      ;;
    Linux:x86_64)
      if ! getconf GNU_LIBC_VERSION >/dev/null 2>&1; then
        fail "Linux Release Preview requires x86_64 glibc; musl and unknown libc hosts must use the locked Cargo fallback"
      fi
      printf '%s\n' "x86_64-unknown-linux-gnu"
      ;;
    *)
      fail "unsupported host ${kernel}/${machine}; supported targets: macOS arm64, macOS x86_64, Linux x86_64 glibc"
      ;;
  esac
}

target=$(detect_target)
case "${target}" in
  aarch64-apple-darwin|x86_64-apple-darwin|x86_64-unknown-linux-gnu) ;;
  *) fail "unsupported target ${target}; supported targets: aarch64-apple-darwin, x86_64-apple-darwin, x86_64-unknown-linux-gnu" ;;
esac
archive_name="xana-cli-${target}.tar.gz"

temporary_dir=$(mktemp -d "${TMPDIR:-/tmp}/xana-install.XXXXXXXX") || fail "could not create a temporary directory"
chmod 700 "${temporary_dir}"
manifest_path="${temporary_dir}/${MANIFEST_NAME}"
archive_path="${temporary_dir}/${archive_name}"

download() {
  local source=$1 destination=$2 limit=$3
  if [[ -n "${test_fixture_root}" ]]; then
    [[ -f "${test_fixture_root}/${source}" ]] || fail "fixture asset is missing: ${source}"
    cp -- "${test_fixture_root}/${source}" "${destination}"
  else
    local url=$source
    [[ "${url}" == https://* ]] || fail "production downloads require HTTPS"
    if command -v curl >/dev/null 2>&1; then
      curl --proto '=https' --tlsv1.2 --fail --location --max-redirs 5 \
        --silent --show-error --connect-timeout 15 --max-time 300 \
        --max-filesize "${limit}" --output "${destination}" "${url}"
    elif command -v wget >/dev/null 2>&1; then
      (
        ulimit -f $(((limit + 511) / 512))
        wget --https-only --max-redirect=5 --timeout=30 --tries=2 \
          --output-document="${destination}" "${url}"
      )
    else
      fail "curl or wget is required to download Xana"
    fi
  fi
  [[ -f "${destination}" ]] || fail "download did not create the expected file"
  local size
  size=$(wc -c <"${destination}" | tr -d ' ')
  [[ "${size}" =~ ^[0-9]+$ && "${size}" -gt 0 && "${size}" -le "${limit}" ]] ||
    fail "downloaded asset has an invalid or oversized length"
}

if [[ -n "${test_fixture_root}" ]]; then
  manifest_source=${MANIFEST_NAME}
elif [[ "${requested_version}" == "latest" ]]; then
  manifest_source="https://github.com/${REPOSITORY}/releases/latest/download/${MANIFEST_NAME}"
else
  manifest_source="https://github.com/${REPOSITORY}/releases/download/v${requested_version}/${MANIFEST_NAME}"
fi
download "${manifest_source}" "${manifest_path}" "${MANIFEST_LIMIT}"

release_version=""
selected_hash=""
selected_size=""
seen_targets=" "
artifact_count=0
line_number=0
manifest_version_seen="false"
while IFS=' ' read -r field first second third fourth extra || [[ -n "${field:-}" ]]; do
  line_number=$((line_number + 1))
  fourth=${fourth%$'\r'}
  [[ -z "${extra:-}" ]] || fail "release manifest line ${line_number} has extra fields"
  case "${field:-}" in
    manifest-version)
      [[ "${manifest_version_seen}" == "false" && "${first:-}" == "1" && -z "${second:-}" ]] ||
        fail "unsupported or duplicate release manifest version"
      manifest_version_seen="true"
      ;;
    release-version)
      [[ -z "${release_version}" && "${first:-}" =~ ^[0-9]{1,9}\.[0-9]{1,9}\.[0-9]{1,9}$ && -z "${second:-}" ]] ||
        fail "release manifest has an invalid release version"
      release_version=${first}
      ;;
    artifact)
      manifest_target=${first:-}
      manifest_name=${second:-}
      manifest_hash=${third:-}
      manifest_size=${fourth:-}
      case "${manifest_target}" in
        aarch64-apple-darwin|x86_64-apple-darwin|x86_64-unknown-linux-gnu)
          expected_name="xana-cli-${manifest_target}.tar.gz"
          ;;
        x86_64-pc-windows-msvc)
          expected_name="xana-cli-${manifest_target}.zip"
          ;;
        *) fail "release manifest contains an unsupported target" ;;
      esac
      [[ "${manifest_name}" == "${expected_name}" ]] || fail "release manifest contains an unexpected artifact name"
      [[ "${manifest_hash}" =~ ^[0-9a-f]{64}$ ]] || fail "release manifest contains an invalid SHA-256"
      [[ "${manifest_size}" =~ ^[0-9]+$ && "${manifest_size}" -gt 0 && "${manifest_size}" -le "${ARCHIVE_LIMIT}" ]] ||
        fail "release manifest contains an invalid artifact size"
      [[ "${seen_targets}" != *" ${manifest_target} "* ]] || fail "release manifest contains a duplicate target"
      seen_targets+="${manifest_target} "
      artifact_count=$((artifact_count + 1))
      if [[ "${manifest_target}" == "${target}" ]]; then
        selected_hash=${manifest_hash}
        selected_size=${manifest_size}
      fi
      ;;
    '') ;;
    *) fail "release manifest line ${line_number} has an unknown record" ;;
  esac
done <"${manifest_path}"

[[ "${manifest_version_seen}" == "true" && "${artifact_count}" -eq 4 && "${seen_targets}" == *" aarch64-apple-darwin "* && "${seen_targets}" == *" x86_64-apple-darwin "* && "${seen_targets}" == *" x86_64-pc-windows-msvc "* && "${seen_targets}" == *" x86_64-unknown-linux-gnu "* ]] ||
  fail "release manifest does not contain the exact four-target inventory"
[[ -n "${release_version}" && -n "${selected_hash}" ]] || fail "release manifest does not select this target"
if [[ "${requested_version}" != "latest" && "${release_version}" != "${requested_version}" ]]; then
  fail "requested ${requested_version}, but the manifest describes ${release_version}"
fi

if [[ -n "${test_fixture_root}" ]]; then
  archive_source=${archive_name}
else
  archive_source="https://github.com/${REPOSITORY}/releases/download/v${release_version}/${archive_name}"
fi
download "${archive_source}" "${archive_path}" "${ARCHIVE_LIMIT}"
actual_size=$(wc -c <"${archive_path}" | tr -d ' ')
[[ "${actual_size}" == "${selected_size}" ]] || fail "archive length differs from the release manifest"

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$1" | awk '{print $NF}'
  else
    fail "sha256sum, shasum, or openssl is required to verify Xana"
  fi
}
actual_hash=$(sha256_file "${archive_path}" | tr 'A-F' 'a-f')
[[ "${actual_hash}" == "${selected_hash}" ]] || fail "archive SHA-256 differs from the release manifest"

inventory_path="${temporary_dir}/inventory"
verbose_path="${temporary_dir}/inventory.verbose"
tar -tzf "${archive_path}" >"${inventory_path}" || fail "could not inspect the Xana archive"
tar -tvzf "${archive_path}" >"${verbose_path}" || fail "could not inspect Xana archive entry types"
[[ "$(wc -l <"${inventory_path}" | tr -d ' ')" -eq 4 ]] || fail "Xana archive has an unexpected entry count"
while IFS= read -r entry; do
  entry=${entry%$'\r'}
  [[ -n "${entry}" && "${entry}" != /* && "${entry}" != *:* && "${entry}" != ".." && "${entry}" != ../* && "${entry}" != */../* && "${entry}" != */.. ]] ||
    fail "Xana archive contains an unsafe path"
done <"${inventory_path}"
expected_inventory=$'LICENSE\nREADME.md\ninstallation.md\nxana'
actual_inventory=$(LC_ALL=C sort "${inventory_path}")
[[ "${actual_inventory}" == "${expected_inventory}" ]] || fail "Xana archive contents differ from the release contract"
while IFS= read -r details; do
  [[ "${details:0:1}" == "-" ]] || fail "Xana archive contains a link or non-regular entry"
done <"${verbose_path}"

if [[ ! -d "${install_dir}" ]]; then
  mkdir -p -- "${install_dir}" || fail "could not create install directory"
  created_install_dir="true"
fi
[[ ! -L "${install_dir}" ]] || fail "install directory cannot be a symbolic link"
install_dir=$(cd -- "${install_dir}" && pwd -P) || fail "could not resolve install directory"
stage_dir=$(mktemp -d "${install_dir}/.xana-stage.XXXXXXXX") || fail "could not create same-filesystem staging"
chmod 700 "${stage_dir}"
tar -xzf "${archive_path}" -C "${stage_dir}" LICENSE README.md installation.md xana || fail "could not extract the verified Xana archive"
chmod 755 "${stage_dir}/xana"

staged_version=$(${stage_dir}/xana --version 2>/dev/null) || fail "staged Xana version smoke failed"
[[ "${staged_version}" == "xana ${release_version}" ]] || fail "staged Xana reports '${staged_version}', expected 'xana ${release_version}'"
"${stage_dir}/xana" --help >/dev/null 2>&1 || fail "staged Xana help smoke failed"

final_path="${install_dir}/xana"
[[ ! -L "${final_path}" ]] || fail "refusing to replace a symbolic-link destination"
existing_version=""
action="install"
if [[ -f "${final_path}" ]]; then
  existing_output=$(${final_path} --version 2>/dev/null || true)
  if [[ "${existing_output}" =~ ^xana\ ([0-9]{1,9}\.[0-9]{1,9}\.[0-9]{1,9})$ ]]; then
    existing_version=${BASH_REMATCH[1]}
  fi
  if [[ "${existing_version}" == "${release_version}" ]]; then
    action="reinstall"
  elif [[ -z "${existing_version}" ]]; then
    action="replace-unrecognized"
  else
    IFS=. read -r old_major old_minor old_patch <<<"${existing_version}"
    IFS=. read -r new_major new_minor new_patch <<<"${release_version}"
    if ((10#${new_major} > 10#${old_major} || (10#${new_major} == 10#${old_major} && 10#${new_minor} > 10#${old_minor}) || (10#${new_major} == 10#${old_major} && 10#${new_minor} == 10#${old_minor} && 10#${new_patch} > 10#${old_patch}))); then
      action="upgrade"
    else
      action="downgrade"
    fi
  fi
fi

path_contains_dir() {
  local entry
  local entries=()
  local previous_ifs=${IFS}
  IFS=:
  read -r -a entries <<<"${PATH:-}"
  IFS=${previous_ifs}
  for entry in "${entries[@]}"; do
    [[ "${entry}" != "${install_dir}" ]] || return 0
  done
  return 1
}

has_controlling_terminal() {
  (exec 3<>/dev/tty) 2>/dev/null
}

profile_path() {
  case "${SHELL##*/}" in
    zsh) printf '%s\n' "${HOME}/.zprofile" ;;
    bash)
      if [[ -f "${HOME}/.bash_profile" ]]; then
        printf '%s\n' "${HOME}/.bash_profile"
      else
        printf '%s\n' "${HOME}/.profile"
      fi
      ;;
    *) printf '%s\n' "${HOME}/.profile" ;;
  esac
}

modify_path_if_requested() {
  if path_contains_dir; then
    printf 'PATH: %s is already active.\n' "${install_dir}"
    return 0
  fi
  if [[ "${path_mode}" == "never" ]]; then
    printf 'PATH unchanged. Add %s to PATH before starting a new shell.\n' "${install_dir}"
    return 0
  fi

  local should_modify="false"
  if [[ "${path_mode}" == "always" ]]; then
    should_modify="true"
  elif has_controlling_terminal; then
    printf 'Add %s to your user PATH? [y/N]: ' "${install_dir}" >/dev/tty
    local answer=""
    IFS= read -r answer </dev/tty || true
    case "${answer}" in y|Y|yes|YES|Yes) should_modify="true" ;; esac
  fi
  if [[ "${should_modify}" != "true" ]]; then
    printf 'PATH unchanged. Add %s to PATH before starting a new shell.\n' "${install_dir}"
    return 0
  fi

  local profile temp_profile quoted_dir
  profile=$(profile_path)
  [[ ! -L "${profile}" ]] || { printf 'refusing to edit symbolic-link profile %s\n' "${profile}" >&2; return 1; }
  if [[ -f "${profile}" ]] && grep -F "# ${PATH_MARKER}" "${profile}" >/dev/null 2>&1; then
    printf 'PATH profile entry already exists in %s; start a new shell.\n' "${profile}"
    return 0
  fi
  mkdir -p -- "$(dirname -- "${profile}")" || return 1
  temp_profile=$(mktemp "${profile}.xana.XXXXXXXX") || return 1
  if [[ -f "${profile}" ]]; then
    cp -p -- "${profile}" "${temp_profile}" || { rm -f -- "${temp_profile}"; return 1; }
  else
    chmod 600 "${temp_profile}" || { rm -f -- "${temp_profile}"; return 1; }
  fi
  quoted_dir=$(printf '%s' "${install_dir}" | sed "s/'/'\\\\''/g")
  printf '\nexport PATH='"'"'%s'"'"':"$PATH" # %s\n' "${quoted_dir}" "${PATH_MARKER}" >>"${temp_profile}" || {
    rm -f -- "${temp_profile}"
    return 1
  }
  mv -f -- "${temp_profile}" "${profile}" || { rm -f -- "${temp_profile}"; return 1; }
  printf 'PATH: added %s in %s; start a new shell or source that file.\n' "${install_dir}" "${profile}"
}

if [[ -f "${final_path}" ]]; then
  had_previous="true"
  backup_path="${install_dir}/.xana-backup.$$"
  [[ ! -e "${backup_path}" ]] || fail "temporary activation path already exists"
  cp -p -- "${final_path}" "${backup_path}" || fail "could not preserve the existing Xana executable"
fi
if ! mv -f -- "${stage_dir}/xana" "${final_path}"; then
  fail "could not activate Xana; the previous executable was preserved"
fi
activation_started="true"
if ! modify_path_if_requested; then
  if [[ -n "${backup_path}" && -f "${backup_path}" ]]; then
    mv -f -- "${backup_path}" "${final_path}" || fail "PATH update failed and the prior executable could not be restored"
    backup_path=""
  else
    rm -f -- "${final_path}"
  fi
  fail "PATH update failed; executable activation was rolled back"
fi
activation_committed="true"
rm -f -- "${backup_path:-}"
backup_path=""
created_install_dir="false"

printf 'Xana %s: %s %s at %s (SHA-256 verified).\n' "${action}" "${release_version}" "${target}" "${final_path}"

setup_result="skipped"
if [[ "${setup_mode}" == "run" ]]; then
  set +e
  if has_controlling_terminal; then
    "${final_path}" setup --if-needed </dev/tty >/dev/tty
  else
    "${final_path}" setup --if-needed
  fi
  setup_status=$?
  set -e
  case "${setup_status}" in
    0) setup_result="ready-or-configured" ;;
    10)
      setup_result="pending"
      printf 'Xana configuration is pending. Run: xana setup\n'
      ;;
    *)
      printf 'Xana binary activation succeeded, but setup readiness failed. Run: %s doctor\n' "${final_path}" >&2
      exit 1
      ;;
  esac
fi
printf 'Install receipt: version=%s action=%s target=%s setup=%s\n' "${release_version}" "${action}" "${target}" "${setup_result}"
