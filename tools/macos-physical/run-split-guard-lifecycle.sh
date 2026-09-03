#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
probe="$script_dir/target/release/nbreq-f6-split-guard"
worker="$script_dir/split-guard-worker.sh"
log_dir=${NBREQ_F6_LOG_DIR:-"$HOME/nbreq-f6-physical"}
stamp=$(date -u +%Y%m%dT%H%M%SZ)
baseline_log="$log_dir/split-guard-$stamp-baseline.log"
fixture_log="$log_dir/split-guard-$stamp-fixture.log"
restored_log="$log_dir/split-guard-$stamp-restored.log"

test "$(id -u)" -ne 0
test -x "$probe"
test -f "$worker"
mkdir -p "$log_dir"

expect_supported_baseline() {
    output=$1
    set +e
    "$probe" > "$output" 2>&1
    status=$?
    set -e
    test "$status" -eq 1
    grep -q SPLIT_DNS_WAS_ACCEPTED "$output"
}

expect_supported_baseline "$baseline_log"
sudo -v
sudo -n sh "$worker" "$probe" "$(id -un)" > "$fixture_log" 2>&1
expect_supported_baseline "$restored_log"

test ! -e /etc/resolver/nbreq-f64.test
printf 'SPLIT_GUARD_LIFECYCLE_PASS\n'
printf 'SPLIT_BASELINE_LOG %s\n' "$baseline_log"
printf 'SPLIT_FIXTURE_LOG %s\n' "$fixture_log"
printf 'SPLIT_RESTORED_LOG %s\n' "$restored_log"
