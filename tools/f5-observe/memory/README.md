# F5 concurrent memory observer

This unpublished tool extends F5 with a separate client/fixture/controller design. It does
not change the existing observer or its v0.1.1 comparison. Use release builds for observations.
Python 3.8+ and Rust 1.85+ are required. All traffic in the matrix is loopback HTTP/1.1.

Build two copies (use `.exe` on Windows):

```sh
cargo build --release --locked --manifest-path tools/f5-observe/memory/Cargo.toml
cp tools/f5-observe/memory/target/release/nbreq-f5-memory /absolute/output/plain
cargo build --release --locked --manifest-path tools/f5-observe/memory/Cargo.toml --features alloc-meter
cp tools/f5-observe/memory/target/release/nbreq-f5-memory /absolute/output/meter
python3 tools/f5-observe/memory/test_runner.py --plain /absolute/output/plain --meter /absolute/output/meter --output /absolute/output/checks
python3 tools/f5-observe/memory/run.py --plain /absolute/output/plain --meter /absolute/output/meter --output /absolute/output/baseline --source MANIFEST_SHA256
```

Output folders must be new. `--smoke --repeats 1` runs four cases; the default runs 120 cases
(three repetitions of 18 ordinary and two extended workloads, each plain/instrumented).
Keep both Cargo.lock files, exact source manifest, build commands/logs, binary hashes and
`provenance.json` with any published comparison. The driver reports actual Rust version,
target, features and Engine limits. It uses the `native` and `resolver` features by default.

## What the phases mean

Every case gets a new client process and one separate, uninstrumented fixture process. The
fixture generates a private CA/leaf/key in memory. Only the public CA is saved to `root.der`;
the client adds it to its Engine's normal platform verifier. No machine trust-store changes
or verification bypasses are involved. External trusted HTTPS checks run separately.

The client uses one submitting/collecting thread, the normal Engine threads, and Engine defaults
except per-origin connection and idle capacity explicitly raised to 32. This permits actual
1/16/32 concurrent connections instead of queueing behind the default per-origin cap of 8.
Global connection/idle limits remain 32. This is a capacity experiment, not a proposed GDS profile.

| Phase | Work and gate |
| --- | --- |
| driver_idle | 150 ms before Engine creation; includes CLI/config/root and reporting overhead. |
| engine_idle | Construct the Engine, then wait 100 ms. |
| cold_connections | Concurrent POST requests; server holds responses 250 ms to prove the requested concurrency. This latency includes that intentional hold. |
| steady | Reuse exactly the same connections for `max(16, 256 / connections)` concurrent waves. Upload/response are each 1/8/50 KiB. No artificial server delay. |
| burst_retained | Concurrent wave, server hold 100 ms, then retain all complete responses for 150 ms. |
| released_idle | Drop those responses; wait 300 ms with reusable connections retained. |
| shutdown_idle | Drop client/response ownership, join Engine shutdown, wait 150 ms. |

Extended cases append these phases before shutdown, at 32 connections:

| Phase | Work and gate |
| --- | --- |
| slow_peer | 32 × 50 KiB replies sent in 1 KiB chunks with 2 ms pauses. |
| long_polls_and_burst | 16 × 1 KiB replies held 300 ms, alongside 16 × 8 KiB fast replies; fast replies must finish while at least one poll remains pending. |
| cancel_and_replace | Cancel 32 × 50 KiB requests held 600 ms, after 100 ms; replace with 32 × 1 KiB requests. Exact cancellation/success accounting required. |
| slow_reader_held | 32 × 512 KiB streaming responses, hold readers 300 ms; require inflight work and reserved queue capacity. Default 256 KiB response windows force backpressure. |
| slow_reader_drain | Drain all readers round-robin, 1 KiB per reader per pass with 1 ms pauses; check every byte and exact EOF, then release queues. |
| large_transfer_retained | One 4 MiB POST and 4 MiB reply, retain response 100 ms. |
| post_large_idle | Drop the large response, wait 500 ms. |

All successful uploads/responses use a deterministic byte pattern and exact length/status checks.
Normal cases require exact fixture/client request counts, physical connections and reuse. All
cases require observed fixture concurrency, no malformed uploads, no unexpected client failures,
settled queues, terminal accounting and joined shutdown. Cancellation can reach the fixture after
its reply was written to the socket; fixture completion is not a client terminal outcome.
The fixture keeps bounded thread handles/socket clones until case shutdown (at most 256 accepted
connections); its allocations and stacks are outside client memory and measured separately.

## Measurement limits

* `plain` supplies performance and uninstrumented process-memory observations. `meter` uses a
  nonallocating lock around exact logical Rust allocation accounting across threads. Its timings
  include instrumentation cost and must not be substituted for plain timings.
* Meter live/peak bytes are requested allocation sizes. Successful realloc replaces the previous
  logical allocation; allocator-internal transient coexistence, size classes, metadata, native
  TLS/system verification allocations and stacks are excluded. Phase reset changes only peak,
  never live ownership/cumulative counters. Phase snapshots precede JSON construction.
* Process samples include the entire client, including driver payload ownership, native runtime,
  stacks, reporting buffers and allocator retention. They exclude Python and the fixture.
  Windows reports private commit and working set via GetProcessMemoryInfo; Linux reports RSS
  and private resident pages from /proc; these **private** figures are different measures.
  Windows/Linux also report OS lifetime RSS high-water marks, which cannot be reset per phase.
* Phase peaks are sampled approximately every 10 ms on Windows/Linux, including both endpoints.
  Short peaks can be missed. Macs use `ps` approximately every 100 ms and report RSS/virtual size,
  with no private/physical-footprint or lifetime-peak claim. Sampling overhead and shared CPU
  contention can affect the workload. Actual sample counts accompany every phase.
* CPU is the client's user+kernel time delta over the phase's controller endpoints; it includes
  phase reporting and is quantised by the OS. Very short phases can show zero CPU. Fixture CPU/RAM
  are reported separately over the case; do not subtract process maxima from one another.
* Latencies are application-observed submit-to-waiter-collection times in submission order, not
  independently timestamped wire completion. Throughput uses repeated concurrent waves, not
  constant replacement load. Per-phase time includes exact-byte validation and settling; idle,
  burst and cold phases include intentional waits. Use steady plain results for comparisons.
* Warm TLS means connection reuse; cold connections use a fresh process/verifier, but OS caches
  and shared system trust services may already be warm. Loopback measures no WAN/DNS delays.
* These are bounded workload observations, not hard RAM ceilings or proof an entire GDS install
  fits a 128 MB device. Kernel socket memory, other application allocations and other processes
  require additional headroom. No allocator trimming or process-global allocator tuning is used.

`test_runner.py` proves real corruption invalidates a run, timeout cleanup joins both owned
processes, exact success accounting/concurrency/reuse, and public system trust with/without the
extra private CA. Failure folders retain events, stderr and cleanup receipts, but have no valid
`result.json`. Run-level `results.json` is written only after every case passes. Each case has
a 45-second work bound plus bounded cleanup; the public HTTPS smoke has a 30-second process bound.

The meter's own tests cover live/peak retention, realloc growth/shrink, allocation failure,
simultaneous ownership and racing phase resets/snapshots. Run them and clippy independently:

```sh
cargo test --manifest-path tools/f5-observe/memory/meter/Cargo.toml
cargo clippy --manifest-path tools/f5-observe/memory/meter/Cargo.toml --all-targets -- -D warnings
cargo clippy --manifest-path tools/f5-observe/memory/Cargo.toml --all-targets --all-features -- -D warnings
```
