# Physical macOS gate helpers

This unpublished tool package drives the physical Apple Silicon part of F6.4. It is outside the
`nbreq` crate archive and does not define product API.

The gate is intentionally split:

1. `run-static-gate.sh` records the host/toolchains, requires a native `arm64` process, runs the
   complete 24-stage verifier under stable and Rust 1.85, checks the Darwin helper, builds these
   probes, and performs a short live HTTP/DNS/TCP smoke.
2. `nbreq-f6-reacquire` proves that one Engine created before an owner-operated network outage
   reaches DNS-stage failure and later recovers. `launch-reacquire-watchdog.sh` is the physical-host
   launcher for a transient `ifconfig` bounce: it detaches the probe, arms an independent 90-second
   restore before link-down, and also restores through the primary worker after 45 seconds. Run it
   only when the provider can reboot the host if both local restore paths unexpectedly fail.
3. `nbreq-f6-split-guard` is the fail-closed observation binary for a separately controlled
   `/etc/resolver` experiment. The system change and exact cleanup remain owner-operated.
4. `launch-soak.sh` detaches one long-lived Engine, writes timestamped output plus PID/exit-marker
   files, and survives loss of the SSH/bridge session.

All commands assume the source archive has already been hash-checked and extracted. They do not
install software or change network/System Configuration state.

## Static gate

```sh
cd /path/to/nbreq
export PATH="$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
nohup sh tools/macos-physical/run-static-gate.sh \
  > "$HOME/nbreq-f6-static.log" 2>&1 < /dev/null &
echo $! > "$HOME/nbreq-f6-static.pid"
```

Optional live targets are comma-separated:

```sh
export NBREQ_SOAK_URLS="https://gds.caverock.com/,https://gds2.caverock.com/"
export NBREQ_SOAK_DNS_NAME="example.com"
export NBREQ_SOAK_TCP_HOST="example.com"
export NBREQ_SOAK_TCP_PORT="80"
```

## Long soak

The default is 12 hours at one cycle per minute. Override `NBREQ_SOAK_SECONDS` or
`NBREQ_SOAK_INTERVAL_SECONDS` before launching if the rental window calls for another duration.

```sh
cd /path/to/nbreq
sh tools/macos-physical/launch-soak.sh
```

The launcher prints the log, PID, and eventual exit-marker paths. A successful completion contains
`SOAK_END ... errors=0`, `DETACHED_SOAK_EXIT code=0`, and an exit-marker file containing `0`.

## Live disruptive checks

Builds are produced by the static gate under `tools/macos-physical/target/release/`.

```sh
tools/macos-physical/target/release/nbreq-f6-reacquire
tools/macos-physical/target/release/nbreq-f6-split-guard
```

Do not disable a rented host's only network path without first proving that the provider console can
restore it. Do not create an `/etc/resolver` entry without an exact cleanup trap. These two actions
remain interactive acceptance steps rather than automated side effects of this package.

Where the provider has no independent console but can reboot the host, a transient interface-flag
bounce is safer than disabling the persistent network service. Review the detected interface first,
then launch the detached probe and its two local restore paths while SSH is healthy:

```sh
cd /path/to/nbreq
sh tools/macos-physical/launch-reacquire-watchdog.sh en0
```

The command prompts for `sudo` before launching anything. It prints separate probe, primary-restore,
watchdog-restore, and exit-marker paths. SSH is expected to disappear briefly. Do not use this helper
with `networksetup -setnetworkserviceenabled`; that setting may survive a reboot.

The split-DNS lifecycle helper first proves ordinary Engine construction is supported, creates only
`/etc/resolver/nbreq-f64.test` under a root-owned cleanup trap, proves fail-closed construction, removes
the exact fixture (and the directory only if it created it), then proves ordinary construction again:

```sh
cd /path/to/nbreq
sh tools/macos-physical/run-split-guard-lifecycle.sh
```

It refuses to run if `/etc/resolver` already contains anything and writes separate before/during/after
logs. This fixture routes only the unused `nbreq-f64.test` suffix to loopback and does not interrupt SSH.
