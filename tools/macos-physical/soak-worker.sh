#!/bin/sh
set -u

SOURCE_ROOT=$1
LOG=$2
EXIT_FILE=$3
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
export PATH="$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
exec > "$LOG" 2>&1

printf '%s DETACHED_SOAK_START source=%s seconds=%s interval=%s\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$SOURCE_ROOT" \
    "${NBREQ_SOAK_SECONDS:-unset}" "${NBREQ_SOAK_INTERVAL_SECONDS:-unset}"
cd "$SOURCE_ROOT" || exit 125
"$SCRIPT_DIR/target/release/nbreq-f6-soak"
code=$?
printf '%s DETACHED_SOAK_EXIT code=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$code"
printf '%s\n' "$code" > "$EXIT_FILE"
exit "$code"
