#!/bin/sh
set -eu

probe=${1:?usage: split-guard-worker.sh probe owner}
owner=${2:?usage: split-guard-worker.sh probe owner}
resolver_dir=/etc/resolver
fixture="$resolver_dir/nbreq-f64.test"
created_dir=0

cleanup() {
    status=$?
    trap - EXIT HUP INT TERM
    rm -f "$fixture"
    if [ "$created_dir" -eq 1 ] && [ -d "$resolver_dir" ] &&
        [ -z "$(/bin/ls -A "$resolver_dir")" ]; then
        rmdir "$resolver_dir" || :
    fi
    printf '%s SPLIT_GUARD_CLEANUP status=%s\n' \
        "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$status"
    exit "$status"
}
trap cleanup EXIT HUP INT TERM

if [ -e "$resolver_dir" ] && [ ! -d "$resolver_dir" ]; then
    echo "$resolver_dir exists but is not a directory" >&2
    exit 2
fi
if [ -d "$resolver_dir" ]; then
    existing=$(/bin/ls -A "$resolver_dir")
    if [ -n "$existing" ]; then
        echo "$resolver_dir is not empty; refusing to alter it" >&2
        exit 2
    fi
else
    mkdir -m 755 "$resolver_dir"
    created_dir=1
fi

printf 'nameserver 127.0.0.1\n' > "$fixture"
chmod 644 "$fixture"
printf '%s SPLIT_GUARD_FIXTURE_CREATED path=%s\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$fixture"

/usr/bin/sudo -u "$owner" -- "$probe"
printf '%s SPLIT_GUARD_FIXTURE_REJECTED\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
