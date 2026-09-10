# NBReq 0.2 consumer preparation — 2026-09-10

Resume point: R1 and the R2 platform rehearsal pass on Windows, Linux, Intel Mac and Apple
Silicon. All jobs finished; source/package hashes were rechecked. Darwin-specific tests/lint/
publication dry runs and current advisory/license checks also pass. Registry-only acceptance
is still open. Publication is not authorized by this slice.
GDS production source and its installed DLL are outside this work.

## Contract review

Compared the public-surface source diff from tag `v0.1.1` with the candidate. The retained HTTP
Engine/Client/Request, callbacks, direct waiters, streaming, cancellation and consuming shutdown
interfaces remain. `Request` still implements Clone with independent application body storage;
its removed derive is replaced by an explicit implementation. `Response::body()` stays borrowed,
but Response clone now shares its immutable body allocation. Shared/unique consuming access and
the capacity-accounting lifetime are explained in the guide and migration notes.

New surface groups are public Resolver, cleartext TCP, Engine GET/POST convenience, additional
Engine-scoped DER roots, per-operation body ceilings, aggregate buffered-body budgeting and
metrics. `drive_until` is now generic over a sealed waiter target; ordinary HTTP inference is
preserved, but function-item users may need specialization. Public non-exhaustive configuration
and error types still require downstream defaults/wildcard handling. Default features now expose
Resolver; native-only retains internal exact-name DNS, HTTP/TLS and TCP. No-feature Engine
construction returns Unsupported. macOS's bounded DNS topology is an explicit consumer constraint.

This is a scoped source/behavior review backed by common-consumer tests, not a claim that every
possible downstream program has undergone mechanical SemVer analysis. The saved API diff lives
in the evidence lab. Memory behavior and the platform-verification shutdown caveat are described
without implying a universal process/device RAM limit or automatic POST retry safety.

## Implementation

- Updated README, consumer guide, SECURITY and added `docs/migrating-to-0.2.md`.
- Added executable DNS, TCP and bounded-HTTP examples; made manual and callback examples useful
  URL-taking programs with bounded operations and explicit shutdown.
- Registered and packaged all seven examples with correct required features. Migration snippets
  join the crate's compile-checked documentation.
- Added unpublished `tools/release-consumer` tooling. It copies itself into a separate temporary
  Cargo workspace and consumes unpacked normalized archives, never private nbreq modules or GDS.

Runtime production behavior is unchanged in this slice. Existing uncommitted M2.5/M3 work and
separately owned F5 edits remain intact; no commit or publication has been made.

## Frozen inputs and Windows results

| Item | Evidence |
| --- | --- |
| Root package B | `010b75a333e2648d32ed780d090eeee2365cf0f8bb80242d09d0d05029737228` |
| Darwin package | `77ff052182129c74490e86c7b2f4a1b51ecf4073f23b03617d3feb049302de5e` |
| Remote source/harness/packages bundle | `7bb94832d20963cf0fbca9c073d06c905650b7e08f0199ae275d61224eb1b1e9`; 114 files, individually hashed |
| Windows full verifier | All 24 steps pass in 101.671 seconds, Rust 1.97.1 x64 MSVC |
| External consumer C | Rust 1.97.1 and 1.85.0: default 9, native-only 8, minimal 2, test-support 10 tests each; four feature import probes each |
| Published 0.1.1 comparison | Same three ordinary HTTP tests pass on both toolchains against actual registry 0.1.1 |
| Packaged examples | All seven built; manual/callback/bounded HTTP and TCP against local fixtures; FFI owner shutdown; ordinary live Resolver and platform-trusted HTTPS to example.com |
| Dependency resolution | Fresh online index for consumer C and its 0.1.1 comparison; both resulting graphs compile on Rust 1.85 |
| License report | Fresh frozen all-feature generation exactly matches checked-in report: `4b7fd101a8dd47d999c5db6f200bfb8a131afaf51fb1664121c8337c06b8979e` |

The full verifier also compiles native-only/all-feature targets, runs doctests across the feature
matrix, builds documentation, denies lint warnings and exercises the three pressure regressions.
The independent tests add real loopback HTTP/streaming/TCP, manual passive TCP drive, retained-body
capacity refusal, cancellation callback uniqueness and joined shutdown. Only the explicit
test-support case injects a DNS server; the separate live example uses ordinary host discovery.

Consumer A is an earlier passing offline run with fewer tests; package A preceded final doc
comments. Consumer B failed before compiling because the Windows sandbox could not obtain
Schannel credentials. Consumer C reran outside that sandbox with normal certificate validation
and passed online. This was an environment correction, not a library fix. Windows C used the
runner before its Python 3.8 filename-suffix compatibility edit; the remote bundle includes that
edit. Package/Rust test/example bytes are identical.

## Open acceptance gates

Root packaging without a Darwin override fails because nbreq-darwin 0.1.0 is not on crates.io.
Both the root packaging rehearsal and independent consumers therefore use an **explicit unpacked
Darwin support override**. This is useful compilation/runtime proof and cannot establish the
registry-only release gate. Published winpoll source matches the v0.1.1 support source unchanged.

R3 still needs a clean, identified Darwin publication candidate and separately authorized
publication. Its rehearsal and the dependency checks below reduce the remaining preparation.
R4's final mixed-capability soak and R5's clean exact-candidate/hosted-CI gates also remain.
GDS/device acceptance remains separate.

## Final platform evidence

| Host | Full verifier(s) | Independent consumer |
| --- | --- | --- |
| Windows x64, Rust 1.97.1 | 24/24; 101.671 s | 1.97.1 and 1.85.0: all feature/test/example checks pass |
| Linux x64, Rust 1.98 / 1.85 | 24/24 each; 407.856 / 467.195 s | Both toolchains pass |
| Intel macOS 15.7.9, Rust 1.98 / 1.85 | 24/24 each; 166.582 / 201.980 s | Both toolchains pass |
| Apple Silicon macOS 26.6.1, Rust 1.98 / 1.85 | 24/24 each; 79.198 / 82.888 s | Both toolchains pass |

Totals: seven full verifiers, 256 consumer test executions (including the unchanged 0.1.1 common
HTTP cases), 32 feature import checks, and 28 packaged example runs. These execution times include
build/test work and are not runtime performance comparisons. All four freshly resolved consumer
locks are byte-identical; the advisory scan therefore covers the common resolved graph.

Linux's bridge display stopped advancing at stable-verify. After the owner restarted it, a
read-only inspection found that both full verifiers and the consumer had already completed with
exit zero and no test processes remained. The completed archive was retrieved, its hash and
internal source manifest verified, and every success/result checked. No test rerun or code fix
was needed. The original bridge completion was not recovered; acceptance rests on the remote
worker's saved exits/logs and separately verified archive transport. Earlier PowerShell/Plink
quoting failure during Mac evidence collection is also retained as a transport-only failure.

## R3 preparation completed alongside the consumer checks

Read all five explicit unsafe sites in the Darwin helper: three reference static Core Foundation
run-loop mode constants; two retain borrowed CF values under the get rule before checked
downcasts. The monitor removes its registered source on Drop, and its notification callback only
marks an atomic dirty flag. This is a focused maintenance review of the existing F6 boundary,
not a new claim that all dependencies have undergone a formal safety audit.

On each Mac, all five helper tests pass on stable and Rust 1.85, stable all-target Clippy passes
with warnings denied, and `cargo publish --dry-run -p nbreq-darwin --locked` packages and verifies
successfully. Cargo explicitly aborts the upload for the dry run. These rehearsals use the frozen
source in a directory without Git metadata; the generated helper archives need not have the same
VCS metadata/hash as the Windows archive used by the consumer matrix. They are not publications.

Installed CI's pinned cargo-audit 0.22.2 only into `target/release-020/audit-tool`. Fresh RustSec
database commit `b50980aad8b8f14f77e25a97b32dd94bf008b0af` (updated 2026-09-09) reports zero
unignored vulnerabilities or warnings for the root 120-dependency lock and the fresh consumer
111-dependency lock. The project already ignores RUSTSEC-2026-0009 for dev-only time 0.3.45;
a new feature-tree check confirms only alloc/std, with the affected parsing feature absent.
The downstream consumer graph has no time dependency. Windows and both Macs independently
resolved byte-identical consumer locks, SHA256
`ff8bb5486d4ce88ea7eb718c366fdfee942017bac7c8ba7605494d87bb9489b5`.
The consumer scan reuses the same fetched advisory DB; record its Git commit alongside the JSON
because cargo-audit leaves the DB commit fields null when `--no-fetch` is used.

## Lab and remote jobs

Durable evidence: [artifact manifest](evidence/nbreq_020_artifacts.json) and
[source/log/package archive](evidence/nbreq-020-consumer-20260910.tar.gz), 1,977,401 bytes,
SHA256 `e2425b7b46abed9a7a9d673da03b95d80e85efed64c5e2feaafed52d5f1a7f85`.
The archive preserves A/B/C results, package A/B, frozen remote source, logs, locks, advisory
results, helper dry runs and bridge recovery. Built binaries and unrelated process inventory
are excluded. No publication, commit, push, GDS production edit or installed DLL change occurred.

Local lab: `target/release-020`. Remote labs are directories named
`nbreq-020-20260910-consumer` under `/home/ubuntu`, `/Users/andrew`, and `/Users/m1`.
The uploaded worker validated all file hashes before running and produced `evidence.tar.gz`
without build trees. The three archives have been collected and verified. Labs/build trees remain
available for the next release slice; all test jobs and helper rehearsals have finished.
