#!/usr/bin/env bash
# Deliberately refuses ordinary workstations and self-hosted runners.
set -euo pipefail
set +x
umask 077
if [[ ${GITHUB_ACTIONS:-} != true || ${RUNNER_ENVIRONMENT:-} != github-hosted ||
      ${GITHUB_EVENT_NAME:-} != workflow_dispatch || $(id -u) == 0 ]]; then
    echo 'native custody requires an explicitly dispatched, disposable, non-root GitHub-hosted runner' >&2
    exit 1
fi
case ${RUNNER_OS:-} in
    Linux)
        if [[ $# == 0 ]]; then
            unset DBUS_SESSION_BUS_ADDRESS DBUS_STARTER_ADDRESS DBUS_STARTER_BUS_TYPE GNOME_KEYRING_CONTROL
            dbus-run-session -- bash "$0" --secret-session
            exit 0
        fi
        [[ $# == 1 && $1 == --secret-session && -n ${DBUS_SESSION_BUS_ADDRESS:-} ]]
        ;;
    macOS) [[ $# == 0 ]] ;;
    *) echo 'unsupported native custody runner' >&2; exit 1 ;;
esac

runner_temp=$(cd "${RUNNER_TEMP:?}" && pwd -P)
scratch=$(mktemp -d "$runner_temp/xana-native-custody.XXXXXXXX")
keychain="$scratch/custody.keychain-db"
keyring_pid=''
previous_default=''
previous_search=()
restore_keychain=false

cleanup() {
    local result=$? cleanup_result=0
    trap - EXIT
    set +e
    if [[ -n $keyring_pid ]]; then
        kill "$keyring_pid" 2>/dev/null
        wait "$keyring_pid" 2>/dev/null
    fi
    if [[ $restore_keychain == true ]]; then
        security default-keychain -d user -s "$previous_default" || cleanup_result=1
        security list-keychains -d user -s "${previous_search[@]}" || cleanup_result=1
    fi
    # Only the generated keychain, never a service-wide or login-keychain delete.
    if [[ -f $keychain ]]; then security delete-keychain "$keychain" || cleanup_result=1; fi
    if [[ $cleanup_result == 0 && -d $scratch && ! -L $scratch &&
          $(cd "$scratch" && pwd -P) == "$scratch" &&
          $scratch == "$runner_temp"/xana-native-custody.* ]]; then
        rm -rf -- "$scratch" || cleanup_result=1
    else
        echo 'custody cleanup not verified; fixture retained on disposable runner (never uploaded)' >&2
        cleanup_result=1
    fi
    if [[ $cleanup_result != 0 ]]; then exit 1; fi
    exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

mkdir -p "$scratch/tmp" "$scratch/xana-home"
export TMPDIR="$scratch/tmp" XANA_HOME="$scratch/xana-home"
password=$(openssl rand -hex 32)
if [[ $RUNNER_OS == Linux ]]; then
    export XDG_DATA_HOME="$scratch/data" XDG_CONFIG_HOME="$scratch/config"
    export XDG_CACHE_HOME="$scratch/cache" XDG_RUNTIME_DIR="$scratch/runtime"
    mkdir -p "$XDG_DATA_HOME" "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME" "$XDG_RUNTIME_DIR/keyring"
    # A nonempty password creates encrypted storage. No eval of daemon output,
    # no real login session, and the test runs within this same private D-Bus.
    gnome-keyring-daemon --foreground --unlock --components=secrets \
        --control-directory "$XDG_RUNTIME_DIR/keyring" \
        < <(printf '%s' "$password") > "$scratch/keyring.log" 2>&1 &
    keyring_pid=$!
    ready=false
    for ((attempt=0; attempt<100; attempt++)); do
        kill -0 "$keyring_pid"
        if gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus \
            --method org.freedesktop.DBus.NameHasOwner org.freedesktop.secrets | grep -q true; then
            ready=true
            break
        fi
        sleep 0.1
    done
    [[ $ready == true ]] || { echo 'isolated Secret Service did not become ready' >&2; exit 1; }
else
    # The production store uses SecKeychain's user-domain default. Point only
    # this disposable runner at a fresh keychain and restore its settings later.
    previous_default=$(security default-keychain -d user)
    previous_default=${previous_default#*\"}; previous_default=${previous_default%\"*}
    search_output=$(security list-keychains -d user)
    while IFS= read -r entry; do
        entry=${entry#*\"}; entry=${entry%\"*}
        [[ -n $entry ]] && previous_search+=("$entry")
    done <<< "$search_output"
    [[ -n $previous_default && ${#previous_search[@]} -gt 0 ]]
    security create-keychain -p "$password" "$keychain"
    restore_keychain=true
    security list-keychains -d user -s "$keychain"
    security default-keychain -d user -s "$keychain"
    security unlock-keychain -p "$password" "$keychain"
    security set-keychain-settings -lut 3600 "$keychain"
fi
unset password

cargo test --locked -p xana --lib --all-features \
    storage::keys::native_tests::production_os_custody_unlock_loss_lock_and_independent_recovery \
    -- --ignored --exact --nocapture
