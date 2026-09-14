# Pre-R5 independent-review follow-up

2026-09-15. The scoped pass is implemented on `codex/pre-r5-release`, based on R4 `68649ee`.
Planning commit `db55c31` records the accepted scope; `9c4c46d` contains the three failing tests.
Packaging and the remote checkpoint are pending. No publication, tag, main merge or GDS change
is included. The [release plan](nbreq_020_release_plan.md) owns the remaining R5/R6 gates.

## Changes and proof

The body-budget helper previously charged an entire replacement allocation in addition to an
unused future reservation when growth exceeded the plan. The three new tests failed on that
implementation: an 8 KiB allocation falsely refused an 8 KiB budget; an uncapped tracker recorded
12 KiB instead of 8 KiB; retry after a failed admission was also falsely refused. The fix acquires
only the reservation's shortfall, transfers that charge once, and preserves the old allocation and
reservation on refusal. Both real allocations remain charged while replacing an existing buffer.
Current fixed-length HTTP bounds its input before this sequence, so this is a demonstrated helper
bug, not a demonstrated current HTTP-triggered failure.

All six budget tests pass, including the three new regressions, old-plus-new allocation accounting,
and concurrent admission bounds. The complete Windows x64 stable verifier passes **24/24 stages**
in **131.476 s**, including 421 default library tests and 13 winpoll tests. Raw red/green output is
retained under `target/pre-r5-20260915` pending evidence sealing.

Ordinary root/support runtime dependencies now accept Cargo-compatible ranges at the same tested
lower bounds, with feature choices unchanged. Dev/tool pins and the documented test-only `time`
MSRV exception remain. Root `Cargo.lock` is unchanged (SHA256
`4d9577637872c8f7e04d85779d6d0b2f471ed324543b1d580a902831a6b50ae5`).

Before the manifest change, an independent consumer requiring Mio 1.2.3 failed to resolve against
NBReq's exact Mio 1.0.4 requirement. Afterward, fresh consumers on stable Rust **1.97.1** and Rust
**1.85.0** pass, both with and without the explicit Mio 1.2.3 requirement. Each case tests default,
native-only, minimal and test-support features plus two negative API-visibility probes. Total:
**34/34 stages**, **116 test executions**, eight expected negative feature probes. All four fresh
graphs selected Mio **1.2.3** and rustls **0.23.45**. Their independent locks and compiler details
are retained. These are edition-2024 consumers using local root/Darwin/winpoll overrides;
they do not establish registry-only acceptance or every future consumer resolution.

Documentation now describes rejection of Transfer-Encoding plus Content-Length and the Windows
adapter parser's whole-snapshot error policy for malformed required fields. Existing valid
unaligned Wine data remains supported. The callback-activation concern was reviewed against its
private RAII ownership and existing exact-once/shutdown tests; no speculative counter patch was
made. No protocol or adapter-parser behavior changed in this pass.

## Dependency pinning: consumer impact

Pins are a build-time dependency-resolution issue. Someone running an already built application
does not acquire a new runtime or memory cost because a manifest uses an exact version.
An application with an otherwise compatible graph may never notice the restriction.

For a library consumer, an exact pin can prevent Cargo from sharing a compatible dependency
required by another library, producing a hard build failure. The Mio conflict above demonstrates
that failure. Pins can also delay uptake of compatible dependency security fixes until NBReq
changes its manifest. They do not freeze the complete transitive graph.

Compatible runtime ranges let consuming applications choose a shared graph; their own committed
lockfiles retain reproducibility and control over updates. Our lockfile records our reviewed test
graph but does not become their lockfile. Fresh stable/MSRV checks cover the resulting distinction.
Ranges do not automatically update existing application lockfiles or promise compatibility with
every future release. Dev-only pins constrain our test tooling, not NBReq's downstream users.

## Packaging and remaining acceptance

`tools/release-consumer/package_candidate.py` packages a clean commit into a new directory, verifies
each exact archive and its source identity, checks packaged relative documentation targets, and
writes a path/hash/file inventory. It uses explicit local helper overrides for the root package;
it does not publish. The old main `target/package` files are not selected or removed.

crates.io rewrites relative README links to repository `blob/HEAD` links. Existing relative links
are not inherently broken. Newly added docs must reach the repository's default branch, or use
verified release-specific destinations, before publication. R5/R6 retain that integration and
actual published README/docs.rs check; validating archive targets alone is not hosted-link proof.

R5 still needs the final integrated source's platform/MSRV/x86 and hosted CI checks, refreshed
advisory/license evidence (including the advisory DB commit/date), and registry-only acceptance.
Both helper packages need rebuilding after their manifest changes and separate publication
authorization. Either helper may publish first; both must resolve before the root registry gate.
The completed R4 soaks remain historical evidence and were not repeated here.
