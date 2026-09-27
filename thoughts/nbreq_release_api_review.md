# Next release: API compatibility and version

Opened 2026-09-27. Decision review requested by the owner after the
[HTTP timeout test repair](nbreq_http_timeout_test_plan.md).

## Checkpoint

| Field | State |
| --- | --- |
| Published baseline | `v0.2.0`, source `d866179719f1cd4e2efcda7e4a533fe590ae3dd6` |
| Reviewed candidate | `f636a3c00728379758f7e922d9968c478fec81a5`, `codex/nbreq-smtp` |
| Recommendation | Release core NBReq as **0.2.1**, retaining the 0.2 compatibility line |
| Decision | Owner accepted **0.2.1** on 2026-09-27. Apply package/dependency/release-tool metadata now; user documentation is the next pass. |
| Review | Complete: root source comparison and independent Astra xhigh API/behavior/evidence review found no demonstrated source-breaking change. Unchanged old-consumer tests and representative new API compilation pass. |
| Scope | API/version decision and metadata implementation. Final release-candidate packaging after documentation, hosted CI, publication and GDS integration remain separate release stages. |

## API assessment

| Area | Change | Compatibility assessment |
| --- | --- | --- |
| Existing HTTP/DNS/plain TCP | Existing public items, signatures and feature availability retained | No removal or incompatible signature change identified |
| Standalone TLS | `TlsOptions`, connection/reader/writer types, pending operation, callback/cancellation/wait outcomes; immediate TLS and consuming upgrade methods | Additive; plain TCP remains available without changing existing calls |
| Engine trust | `TlsTrust`, `EngineConfig::with_tls_trust` and `tls_trust` | Additive; platform trust remains the default, even with the root-bundle feature enabled |
| Engine mode | `Engine::run_mode()` | Additive read-only accessor |
| Error classification | `TlsFailure::Truncated` | Existing enum was already `#[non_exhaustive]`; downstream matches already require a fallback |
| Manual waiter integration | `PendingTlsConnect` implements existing `WaiterTarget` | Trait was already sealed; no new required method imposed on downstream implementers |
| Representation/traits | New private state and TLS types; existing connection ownership markers/callback bounds retained | No public layout promise, removed auto trait, or tightened existing bound identified |
| Features/dependencies | Non-default `bundled-roots` adds optional compatible `webpki-roots = "1.0.9"` | Existing default features and runtime dependency requirements retained; no Rustls types exposed in public API |
| Platform/toolchain | Rust 1.85, edition 2024; unchanged helper crates | No new baseline requirement introduced |

The source audit checks the existing public surface and meaningful old-path
behavior. It is not a mathematical proof for every possible downstream program.
`cargo-semver-checks` is not installed and was not run; no tooling installation is
needed for this decision. As with ordinary additive Rust releases, unusual glob
imports or downstream extension methods can have name collisions.

## Behavior changes to disclose

1. Platform-mode additional roots now pass WebPKI trust-anchor parsing before
   entering the platform verifier. Supplied certificates that an OS previously
   ignored or accepted differently can fail Engine construction. This enforces the
   existing documented validation contract, but parser differences deserve an
   explicit release note rather than a promise of identical platform acceptance.
2. Engines/backends that cannot apply explicit trust settings now reject them.
   This includes public `test-support` held/HTTP-only proving constructors that
   previously ignored additional roots. Tests reusing production TLS configuration
   may need an appropriate HTTPS fixture or a configuration without unused roots.
   The test-support surface is public despite its laboratory-purpose documentation.
3. Accepted plain TCP send buffers shed unused allocation capacity; refused buffers
   retain their original allocation. This can introduce allocation/copy work for
   overallocated accepted buffers while enforcing the intended memory accounting.
4. Plain TCP remembers peer FIN so later pressure updates cannot restart read
   inactivity after EOF. Small bounded native receive windows also change allocation
   strategy. These are correctness/resource fixes, not new wire-protocol contracts.

New standalone TLS has its own documented upgrade, closure and additional-memory
contracts. Choosing portable trust is explicit and changes the source of trust and
revocation behavior; enabling the feature alone does not switch existing Engines.
Wine platform verification is not repaired or automatically bypassed.

## Why 0.2.1

Cargo's pre-1.0 convention treats changes to the first nonzero component as a
compatibility boundary. Thus 0.2.1 can carry compatible additions to 0.2.0, while
0.3.0 starts a separate compatibility line. See the official
[change categories](https://doc.rust-lang.org/cargo/reference/semver.html#change-categories)
and [default version requirements](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#default-requirements),
checked 2026-09-27.

| Number | Effect | Recommendation |
| --- | --- | --- |
| 0.2.1 | Ordinary `"0.2"` consumers can resolve the update; compatible users can share one NBReq version | Recommended for these additive APIs and documented fixes |
| 0.3.0 | Existing `"0.2"` requirements exclude it; mixed dependency graphs can retain both NBReq versions with distinct public types | Reserve for a deliberate incompatible change or explicit compatibility-policy reset |

Release size alone does not require a compatibility boundary. A lockfile or exact
pin still controls when an existing application upgrades; a new version does not
rewrite its lockfile automatically.

Code using the new APIs should require **`nbreq = "0.2.1"`**, a normal compatible
range rather than an exact pin. In particular, the unpublished SMTP crate uses TLS
and `Engine::run_mode` APIs absent from published 0.2.0. Its minimum is raised from
`"0.2.0"` to `"0.2.1"` during versioning. Leaving it at 0.2.0 could permit an old
consumer lock that satisfies the manifest but cannot compile the new API usage.

Keep `nbreq-darwin` at 0.1.0 and `nbreq-winpoll` at 0.1.1: their source and requirements
are unchanged. `nbreq-smtp` remains unpublished at its own 0.1.0 development version;
its publication readiness is independent of the core release number.

## Evidence and next step

Independent checks in `C:/User/projects/nbreq/target/api-release-review-20260927/`
use the published 0.2.0 consumer Rust sources and feature probes unchanged. A local
manifest patch selects the actual candidate checkout; Cargo metadata records that
selection instead of silently using registry 0.2.0.

| Check | Result |
| --- | --- |
| Old consumer: default / native-only / minimal / test-support | 9 / 8 / 2 / 10 tests pass (29 total) |
| Existing resolver and test-support feature probes | Both compile when enabled and fail with the expected missing-import diagnostic when disabled |
| New TLS API probe: default / native-only / minimal | All compile; type-checks immediate TLS, upgrade, split I/O, manual waiting and trust configuration |

The new probe is compilation evidence only, not a TLS handshake or trust-validation
test. These checks use Windows x64 and stable Rust 1.97.1, with offline resolution against
the available registry cache; they are not a fresh online dependency sweep or a
published-package/MSRV/platform acceptance matrix. Earlier reviewed runtime/MSRV
evidence remains in the [portable TLS plan](nbreq_portable_tls_plan.md).

Independent review checked all six baseline Rust files, the copied manifest patch,
the effective feature/source metadata, expected negative diagnostics and raw pass
counts. All 63 recorded source/manifest/helper hashes match before, after and the
reviewed checkout. The new TLS probe and old consumer have separate dependency
locks; the new probe does not declare Rust 1.85 and is not MSRV evidence.

Retained [evidence](evidence/nbreq-release-api-review-20260927.tar.gz): 60 regular
members, 192,194 bytes, SHA256
`60d85231e331196b9fb15331d8823cfd9fec931a95f984b2ce12cbf4855eefb0`.
The [artifact index](evidence/nbreq_release_api_review_artifacts.json) includes
per-member hashes and the consumer report. Sources, manifests/locks, commands,
metadata and logs are included; build output is excluded.

## Accepted version implementation

Owner accepted core **0.2.1** on 2026-09-27, with user documentation explicitly next.
The metadata pass updates the root package and current local-path lock entries,
SMTP's dependency minimum, the exact release-consumer candidate requirement,
package/download/manifest assertions, the registry-check workflow and the current
benchmark version label. The regenerated license inventory identifies the new
root version too. The exact consumer pin is a release-verification input, not a
recommended application dependency policy.

The actual registry 0.1.1 comparison and recorded 0.2.0 publication evidence remain
unchanged. Helper crates stay at their existing releases; SMTP remains unpublished
at 0.1.0. No production Rust or third-party dependency changes are intended.

Focused Windows stable checks pass in
`C:/User/projects/nbreq/target/version-021-20260927/`: 11 locked/offline metadata
checks resolve local NBReq 0.2.1, SMTP compiles with all targets, and an independent
exact-0.2.1 consumer's tests compile in default/native/minimal/test-support modes.
These are compilation checks, not another runtime/MSRV/platform matrix. Executing
the legacy manifest transformation still selects exact registry 0.1.1. All eight
changed locks differ only in the local NBReq version; third-party entries remain
unchanged. Python syntax checks pass. Cargo-about 0.9.1 regenerates the license
report with only the root version changed. Independent review found no blocker.

A clean package identity check remains before closing this metadata pass. It is
a metadata rehearsal; the final candidate must be rebuilt after documentation.
README, guides, examples, changelog and tool README updates remain for that next
pass, including the behavior disclosures above. Then perform the exact versioned
candidate, hosted CI and published-registry checks already identified in release
preparation. This pass does not replace those gates or authorize publication.
