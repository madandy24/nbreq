# R4 release reliability soak

Unpublished Rust 1.85 / Python 3.8+ tooling. Build with default features for the long mixed
HTTP/Resolver/TCP run; `--no-default-features` provides a labelled native-only companion.
No private NBReq test backend or OS DNS override is used. Keep the source manifest, exact lock,
build commands/toolchains and binary hashes with every observation.

```text
cargo build --locked --release --manifest-path tools/release-soak/Cargo.toml
python tools/release-soak/check.py --binary /absolute/binary --out /new/checks
python tools/release-soak/run.py --binary /absolute/binary --out /new/smoke --seconds 30
python tools/release-soak/run.py --binary /absolute/binary --out /new/soak --seconds 14400
```

Set `NBREQ_R4_SOURCE` to the frozen source-manifest hash at build time, and supply matching
`--source` to `run.py`. The default `unfrozen-development` value labels rehearsals honestly.
Each output directory must be new. The runner opens three separate fixture processes, then one
client. Only child PIDs created by that invocation may be terminated. Forced cleanup invalidates
a run. Corruption and watchdog tests deliberately expect failure and verify complete cleanup.

The fixture derives from the accepted M1 HTTP/TLS fixture, with completed threads and socket
clones released during connection churn, a bound on live owners and a bounded exact-byte TCP
echo mode. Its ephemeral TLS CA is supplied only to the test Engine; platform verification,
hostname checking and ordinary system roots remain enabled. No key is written to disk or exported,
and no host trust/DNS configuration is changed. `check.py` exercises 300 new connections per
fixture, including verified TLS, and requires all socket owners to disappear after joining.

Each long-lived Engine round overlaps two TCP sessions, two held HTTP polls and 16 small HTTP/
HTTPS requests. It then drains slow-consumed streaming/TCP data, checks fixed uploads and
cancels the held polls. Periodically it retains a shared 2 MiB response under a 4 MiB aggregate
cap, verifies both pre-admission spare-capacity refusal and response-length refusal, completes
small work with the retained body still charged, and transfers/releases that body. Callback,
stream cancellation, spawned/manual Engine churn and shutdown of pending work are also checked.
Final shutdown occurs with a live streaming response and must report cancellation.

Public DNS and platform-trusted `https://example.com/` run at most once a minute and are recorded
separately. The native-only companion still uses hostname HTTPS. External network failures stop
the run and remain evidence; they are not silently removed from acceptance. This dependency is
explicit, and loopback body/TCP checks do not rely on it for expected bytes.

The supervisor records raw timestamped events, per-round exact terminal accounting, all operation/
queue/body/TCP gauges at quiescence, idle connection state, high-water bounds, sampled client RSS/
private memory/CPU where available, and final fixture cleanup. Sampling comes from the accepted
F5 process sampler; the fixture processes are outside the sampled client. macOS private memory
is unavailable, and sampled peaks are not exact heap peaks. Clock/suspend discrepancies are
logged separately. Review memory trends and interruptions before accepting the final report;
this tool does not assert a universal whole-process RAM limit or publish performance promises.

The 2-second held polls must remain incomplete while the small burst finishes. This is a
workload check with scheduler headroom, not a hard library latency guarantee. A failure requires
inspection, not automatic timeout inflation or removal. Earlier M1–M3 and consumer evidence
remain separate from this exact-source reliability run.
