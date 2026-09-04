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

Use `-Configuration NativeOnly` for the current-tree side of the eventual NBReq 0.1.1 common-HTTP
comparison. The default configuration is the ordinary 0.2 feature set and is descriptive only.
