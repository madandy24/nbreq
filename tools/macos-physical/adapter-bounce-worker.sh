#!/bin/sh
set -eu

mode=${1:?usage: adapter-bounce-worker.sh main|watchdog interface}
interface=${2:?usage: adapter-bounce-worker.sh main|watchdog interface}

case "$interface" in
    *[!A-Za-z0-9]*)
        echo "unsafe interface name: $interface" >&2
        exit 2
        ;;
esac

restore_link() {
    printf '%s RESTORE interface=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$interface"
    /sbin/ifconfig "$interface" up
}

case "$mode" in
    main)
        trap restore_link EXIT HUP INT TERM
        sleep 5
        printf '%s LINK_DOWN interface=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$interface"
        /sbin/ifconfig "$interface" down
        sleep 45
        restore_link
        trap - EXIT HUP INT TERM
        printf '%s PRIMARY_RESTORE_COMPLETE interface=%s\n' \
            "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$interface"
        ;;
    watchdog)
        sleep 90
        restore_link
        printf '%s WATCHDOG_RESTORE_COMPLETE interface=%s\n' \
            "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$interface"
        ;;
    *)
        echo "unknown mode: $mode" >&2
        exit 2
        ;;
esac
