#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
SOURCE_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
LOG_DIR=${NBREQ_F6_LOG_DIR:-"$HOME/nbreq-f6-physical"}
SOAK_SECONDS=${NBREQ_SOAK_SECONDS:-43200}
SOAK_INTERVAL_SECONDS=${NBREQ_SOAK_INTERVAL_SECONDS:-60}
STAMP=$(date -u +%Y%m%dT%H%M%SZ)
LOG="$LOG_DIR/soak-$STAMP.log"
PID_FILE="$LOG_DIR/soak-$STAMP.pid"
EXIT_FILE="$LOG_DIR/soak-$STAMP.exit"

mkdir -p "$LOG_DIR"
test -x "$SCRIPT_DIR/target/release/nbreq-f6-soak"

NBREQ_SOAK_SECONDS="$SOAK_SECONDS" \
NBREQ_SOAK_INTERVAL_SECONDS="$SOAK_INTERVAL_SECONDS" \
nohup sh "$SCRIPT_DIR/soak-worker.sh" "$SOURCE_ROOT" "$LOG" "$EXIT_FILE" \
    > /dev/null 2>&1 < /dev/null &
PID=$!
printf '%s\n' "$PID" > "$PID_FILE"

printf 'SOAK_LAUNCHED pid=%s seconds=%s\n' "$PID" "$SOAK_SECONDS"
printf 'SOAK_LOG %s\n' "$LOG"
printf 'SOAK_PID %s\n' "$PID_FILE"
printf 'SOAK_EXIT %s\n' "$EXIT_FILE"
