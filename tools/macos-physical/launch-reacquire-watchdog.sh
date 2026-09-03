#!/bin/sh
set -eu

interface=${1:?usage: launch-reacquire-watchdog.sh interface}
case "$interface" in
    *[!A-Za-z0-9]*)
        echo "unsafe interface name: $interface" >&2
        exit 2
        ;;
esac

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
probe="$script_dir/target/release/nbreq-f6-reacquire"
worker="$script_dir/adapter-bounce-worker.sh"
log_dir=${NBREQ_F6_LOG_DIR:-"$HOME/nbreq-f6-physical"}
stamp=$(date -u +%Y%m%dT%H%M%SZ)
probe_log="$log_dir/reacquire-$stamp.log"
probe_exit="$log_dir/reacquire-$stamp.exit"
primary_log="$log_dir/reacquire-$stamp-primary-restore.log"
watchdog_log="$log_dir/reacquire-$stamp-watchdog-restore.log"

test "$(id -u)" -ne 0
test -x "$probe"
test -f "$worker"
mkdir -p "$log_dir"

# Authenticate while the network is still up, and prove detached restore jobs can
# use the cached credential without ever needing a terminal after link-down.
sudo -v
sudo -n true

# The pipe waits 15 seconds before releasing the probe's interactive gate. The
# primary worker drops the link at 5 seconds and restores it at 50 seconds.
nohup sh -c '
    (sleep 15; printf "\n") | "$1"
    code=$?
    printf "%s\n" "$code" > "$2"
    exit "$code"
' sh "$probe" "$probe_exit" > "$probe_log" 2>&1 < /dev/null &
probe_pid=$!

# Arm the independent restore first. Both jobs are local root processes and
# therefore survive the SSH connection disappearing with the adapter.
sudo -n nohup sh "$worker" watchdog "$interface" \
    > "$watchdog_log" 2>&1 < /dev/null &
watchdog_pid=$!

sudo -n nohup sh "$worker" main "$interface" \
    > "$primary_log" 2>&1 < /dev/null &
primary_pid=$!

printf 'REACQUIRE_LAUNCHED interface=%s probe_pid=%s primary_pid=%s watchdog_pid=%s\n' \
    "$interface" "$probe_pid" "$primary_pid" "$watchdog_pid"
printf 'REACQUIRE_LOG %s\n' "$probe_log"
printf 'REACQUIRE_EXIT %s\n' "$probe_exit"
printf 'PRIMARY_RESTORE_LOG %s\n' "$primary_log"
printf 'WATCHDOG_RESTORE_LOG %s\n' "$watchdog_log"
