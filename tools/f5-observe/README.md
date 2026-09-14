# F5 observation harness

This unpublished tool package records NBReq's F5 observations. It is outside the product crate
archive, uses only public constructors, and does not define product API or performance promises.

The first workload is `buffered_keepalive`: a loopback HTTP/1.1 fixture validates every response,
connection reuse, NBReq counters, zero operation/queue gauges, joined shutdown, and fixture exit.
The same source is built twice:

- ordinary release binary for whole-process observation;
- release binary with `alloc-stats` for Rust allocation attribution.

The binary writes one inner `nbreq-f5-observation-v1` JSON record to stdout and progress to stderr.
A platform launcher supplies source provenance, bounds the process, fills platform process metrics,
checks that the process is gone, and writes the final JSON plus human log beneath `target/`.
The loopback fixture is currently in the observer process and polls accept every millisecond. Windows
private bytes are therefore named as a 10 ms sampled maximum; working set uses the OS peak counter.
Quiet or split fixture work remains before idle-CPU observation.

On Windows, from the repository root:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File tools/f5-observe/run-windows.ps1
```

Use `-Mode CurrentNative` and `-Mode V011` for the two sides of the Windows/Ubuntu common-native-
HTTP comparison. `V011` resolves the exact registry release `nbreq = "=0.1.1"` through its own
lockfile; it does not rebuild the tag from the current checkout. The default `CurrentDefault` mode is
the ordinary 0.2 feature set and is descriptive only.

## R4 scoped registry comparison

`compare.py` consumes a frozen `r4-compare-source.json` manifest. Build first with
`python tools/f5-observe/compare.py build --out /new/lab`, then collect with `run --out /same/lab`.
It checks every input hash and retains exact binary hashes, commands, toolchains, raw inner
samples, outer process samples and byte/quiescence/cleanup checks. Both versions use the shared
explicit Engine/Client/Request native HTTP source; registry 0.1.1 has its own unchanged lockfile.
The current lock is resolved under Rust 1.85. Only Windows and Linux are comparison hosts.

There are three alternating repetitions for each plain/allocation-instrumented pair and each
1 KiB, 64 KiB and 1 MiB response. Each launch has 32 warmups and three measured samples: 4,096
requests per sample for the smaller two bodies and 256 for the large body. The shared fixture
uses explicit blocking accepted sockets, TCP_NODELAY and bounded writes so buffered bodies do
not accidentally become an OS delayed-ACK comparison. Current-only budget gauges must quiesce;
gauges missing from 0.1.1 remain unavailable. The child has a 180-second hard bound.

The fixture remains in-process. Process figures include it and the 1 ms accept poll, and do
not measure client-only memory or idle CPU. Nominal 10 ms process samples can miss peaks and
the final CPU interval. Allocation-instrumented timings are attribution data, not throughput
evidence. Compare repeated plain timings only within one host/toolchain; no timing threshold
is an ordinary CI test, and these observations are not public performance promises.
