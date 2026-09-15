# Pre-R5 independent-review follow-up

2026-09-15. The scoped pass is implemented on `codex/pre-r5-release`, based on R4 `68649ee`.
Planning commit `db55c31` records the accepted scope; `9c4c46d` contains the three failing tests.
Fix and package source: **`a44cac5788d0596040ba65df29ccff387b5b493d`**. Local validation and
packaging are complete; the approved public checkpoint is pushed and verified. No publication, tag, main merge or
GDS change is included. The [release plan](nbreq_020_release_plan.md) owns the remaining R5/R6 gates.

The owner explicitly approved publishing this branch and its repository history/evidence to
`https://github.com/madandy24/nbreq` on September 15, resolving the earlier automatic-review
rejection. Checkpoint **`72085093c88f12a5afcc9c2548a4b74dc305277b`** was pushed normally to
`origin/codex/pre-r5-release`, and `git ls-remote` confirmed the exact remote identity. The branch
includes the existing memory, consumer and R4 history/evidence as well as this pass. Its outgoing
blobs and nested archives had been inventoried (13,297 ordinary file instances, no matches in the
bounded credential-pattern scan); that check is not a comprehensive secret audit. Subsequent
planning-only commits record completion and the owner's documentation request without changing
the tested crate inputs.

The owner requested a README and examples refresh when work returns to main-tree integration,
before the full release. This is an open R5 acceptance item: review the presentation and usage,
validate the changed examples/doctests and package contents, then freeze fresh publication
packages. The final hosted README/docs.rs link checks remain under R6. This checkpoint does not
perform that refresh or merge main.

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
sealed in the [evidence archive](evidence/nbreq-pre-r5-20260915.tar.gz), with exact identities in
the [artifact manifest](evidence/nbreq_pre_r5_artifacts.json).

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

All three packages from clean commit `a44cac5` pass `cargo package --locked --offline`, including
Cargo's package verification. The root's 14 relative Markdown links resolve to included files.
Root normalization removes local helper paths and retains their release version requirements.
These Windows x64 package checks do not establish native macOS behavior or registry acceptance.

| Exact archive | Bytes | Files | SHA256 |
| --- | ---: | ---: | --- |
| nbreq-darwin-0.1.0.crate | 11,377 | 8 | `4fee1289f9b9d4949c3f48c9f7e47166afd15299f4185916ef1e304e20d31873` |
| nbreq-winpoll-0.1.1.crate | 13,034 | 10 | `7284d7fedda8a13f4bf53aef3634c0ed4097dc94b6400c1f50b8a195791aa55f` |
| nbreq-0.2.0.crate | 336,320 | 82 | `fa099cb2213705e51e913c6a1f8f94afc5c2ee28e2848c491424734fb44d0a63` |

The sealed evidence archive contains **322 files**, **1,700,989 bytes**, SHA256
`cde23b9744e4da6a576bc327cddca59e54dfb719c3ff17c4ee96b48da707d4c6`.
Every archived member was compared with its selected input. It includes red/green logs, the
original Mio resolution failure, full verifier output, four fresh consumer locks/test outputs,
the exact packages, source snapshot, link checks and outgoing-history inventory; build trees
are excluded. The source snapshot matches the validation input hashes. Later report and private
tool README edits do not change the packaged crate inputs.

The fix, manifest changes, compatibility documentation and validation tools were mirrored into
main only after its original file hashes/content were checked. Its unrelated work and unreleased
notices were preserved, and Cargo.lock was unchanged. The clean branch owns the commits.

crates.io rewrites relative README links to repository `blob/HEAD` links. Existing relative links
are not inherently broken. Newly added docs must reach the repository's default branch, or use
verified release-specific destinations, before publication. R5/R6 retain that integration and
actual published README/docs.rs check; validating archive targets alone is not hosted-link proof.
On September 15, remote default branch `5441d205` contains the older consumer guide but lacks
`docs/migrating-to-0.2.md`, `examples/bounded_http.rs`, `examples/resolve.rs` and
`examples/tcp_echo.rs`. Integrating the candidate documentation before publication resolves these
known destinations; this pass does not merge main.

R5 still needs the final integrated source's platform/MSRV/x86 and hosted CI checks, refreshed
advisory/license evidence (including the advisory DB commit/date), and registry-only acceptance.
Fresh helper candidates are included above; they still need final R5 acceptance and separate
publication authorization. Either helper may publish first; both must resolve before the root registry gate.
The completed R4 soaks remain historical evidence and were not repeated here.
