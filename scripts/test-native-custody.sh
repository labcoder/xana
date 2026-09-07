#!/usr/bin/env bash
# Shell-boundary tests only: every credential/process-service command below is
# a function stub. No OS key store, D-Bus daemon or model is touched.
set -euo pipefail
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
test_root=$(mktemp -d)
test_root=$(cd "$test_root" && pwd -P)
cleanup_test() {
    if [[ -d $test_root && ! -L $test_root && $(cd "$test_root" && pwd -P) == "$test_root" &&
          $(basename "$test_root") == tmp.* ]]; then
        rm -rf -- "$test_root"
    else
        echo 'refusing unsafe native-custody test cleanup' >&2
        return 1
    fi
}
trap cleanup_test EXIT

id() { [[ $1 == -u ]]; printf '1000\n'; }
openssl() { [[ $* == 'rand -hex 32' ]]; printf 'synthetic-not-a-secret\n'; }
dbus-run-session() {
    [[ $1 == -- ]]; shift
    export DBUS_SESSION_BUS_ADDRESS=synthetic-private-bus
    "$@"
}
gdbus() { [[ $DBUS_SESSION_BUS_ADDRESS == synthetic-private-bus ]]; printf '(true,)\n'; }
gnome-keyring-daemon() {
    [[ $* == *--foreground* && $* == *--unlock* && $* != *--replace* ]]
    [[ $XDG_DATA_HOME == "$RUNNER_TEMP"/xana-native-custody.*/data ]]
    [[ $XDG_RUNTIME_DIR == "$RUNNER_TEMP"/xana-native-custody.*/runtime ]]
    local value
    value=$(cat)
    [[ $value == synthetic-not-a-secret ]]
    trap 'exit 0' TERM
    while true; do sleep 0.1; done
}
security() {
    # Read/query only synthetic state, with paths containing spaces. The mock
    # never delegates to the host's security executable, including on macOS.
    local command=$1
    shift
    printf '%s\n' "$command" >> "$MOCK_STATE/commands"
    case $command in
        default-keychain)
            if [[ $# == 2 ]]; then printf '    "/synthetic/original login.keychain-db"\n'
            elif [[ $4 == '/synthetic/original login.keychain-db' ]]; then
                printf 'default restored\n' >> "$MOCK_STATE/restored"
                [[ $MOCK_CLEANUP_FAILURE != true ]]
            else [[ $4 == "$RUNNER_TEMP"/xana-native-custody.*/custody.keychain-db ]]; fi
            ;;
        list-keychains)
            if [[ $# == 2 ]]; then printf '    "/synthetic/original login.keychain-db"\n    "/synthetic/second.keychain-db"\n'
            elif [[ $4 == '/synthetic/original login.keychain-db' ]]; then
                [[ $5 == /synthetic/second.keychain-db ]]
                printf 'search restored\n' >> "$MOCK_STATE/restored"
            else [[ $4 == "$RUNNER_TEMP"/xana-native-custody.*/custody.keychain-db ]]; fi
            ;;
        create-keychain) [[ $2 == synthetic-not-a-secret ]]; touch "$3" ;;
        delete-keychain) [[ $1 == "$RUNNER_TEMP"/xana-native-custody.*/custody.keychain-db ]]; rm -- "$1" ;;
        unlock-keychain) [[ $2 == synthetic-not-a-secret ]] ;;
        set-keychain-settings) [[ $* == '-lut 3600 '* ]] ;;
        *) return 91 ;;
    esac
}
cargo() {
    [[ $* == 'test --locked -p xana --lib --all-features storage::keys::native_tests::production_os_custody_unlock_loss_lock_and_independent_recovery -- --ignored --exact --nocapture' ]]
    [[ -d $TMPDIR && -d $XANA_HOME && $XANA_HOME == "$RUNNER_TEMP"/xana-native-custody.*/xana-home ]]
    printf '%s\n' "$XANA_HOME" > "$MOCK_STATE/fixture-home"
    return "$MOCK_CARGO_EXIT"
}
export -f id openssl dbus-run-session gdbus gnome-keyring-daemon security cargo
export GITHUB_ACTIONS=true RUNNER_ENVIRONMENT=github-hosted GITHUB_EVENT_NAME=workflow_dispatch
export RUNNER_TEMP="$test_root/runner space"
mkdir -p "$RUNNER_TEMP"
for platform in Linux macOS; do
    for outcome in success failure cleanup-failure; do
        [[ $platform != Linux || $outcome != cleanup-failure ]] || continue
        export RUNNER_OS="$platform" MOCK_STATE="$test_root/$platform-$outcome"
        export MOCK_CARGO_EXIT=0 MOCK_CLEANUP_FAILURE=false
        mkdir -p "$MOCK_STATE"
        [[ $outcome != failure ]] || MOCK_CARGO_EXIT=23
        [[ $outcome != cleanup-failure ]] || MOCK_CLEANUP_FAILURE=true
        result=0
        bash "$script_dir/qualify-native-custody.sh" || result=$?
        case $outcome in
            success) [[ $result == 0 ]] ;;
            failure) [[ $result == 23 ]] ;;
            cleanup-failure) [[ $result == 1 ]] ;;
        esac
        fixture_home=$(cat "$MOCK_STATE/fixture-home")
        if [[ $outcome != cleanup-failure ]]; then [[ ! -e $(dirname "$fixture_home") ]]; fi
        if [[ $platform == macOS ]]; then
            [[ $(cat "$MOCK_STATE/restored") == $'default restored\nsearch restored' ]]
            [[ $(tail -n 1 "$MOCK_STATE/commands") == delete-keychain ]]
        fi
    done
done
printf 'native custody shell fixtures passed: both platforms, failure exit, exact restore/cleanup, cleanup failure\n'
