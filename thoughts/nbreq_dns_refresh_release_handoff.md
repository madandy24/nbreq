# NBReq DNS refresh release handoff

22 October 2026. The owner requested the shared DNS correction on both release
lines, with GDS verifying the 0.2 line before migrating to 0.3.1. GDS source,
dependency lock, running processes and OS networking were not changed here.

## Release identity

Both versions are published and non-yanked. Registry API, sparse index and
independent archive downloads agree with the accepted package checksums below.
Both published consumer matrices passed all eight platform/compiler jobs.

| Version | Published source | Archive SHA256 |
| --- | --- | --- |
| 0.3.1 | `ca5dde96639d8bbd4d3394ebbb27e5dc3fad6f60` | `8728fe138b31c5f0944437d0a2687cebcfab9f178d16f4b38b75f16f386d3696` |
| 0.2.2 | `6e14ae7807b9c82dbb397c603b480c45f8fe0f29` | `b5dff380a6949632fc9667091238d6fab9a848f94ad42177da8eee1acf78d9f2` |

The annotated release tags identify the published sources. Later closeout notes
do not move those tags. Support crates remain nbreq-winpoll 0.1.1 and
nbreq-darwin 0.1.0. There is no SMTP publication in this change.

Both releases were published on 22 October 2026 (UTC and New Zealand time).
The canonical 0.2.2 archive is 410,522 bytes with 114 regular members; the
0.3.1 archive is 417,146 bytes with 116 regular members. Remote annotated tags
peel to the source identities above.

## Corrected behavior

DNS refresh now uses startup's ordered nameserver socket fallback and stores the
actual selected index. An unusable first VPN DNS address therefore no longer
hides a working later nameserver. UDP retries and DNS-over-TCP fallback use that
selected server. When all candidates fail, the resolver preserves the underlying
discovery, socket setup or registration detail, including address/error context,
and clears that detail on recovery.

The public error classifications, configuration generations, pending-operation
completion, cache invalidation, idle HTTP connection eviction, cancellation and
shutdown policies are preserved. No IPv4 filter or adapter-ranking workaround
was added. Both release lines have identical DNS implementation and tests.

Each patch keeps its own release line's timeout defaults and public API. Moving
GDS from 0.2.0 to 0.2.2 also includes the previously released 0.2.1 TLS/trust
features and stricter trust configuration validation; see the changelog.

## Validation

- The original implementation failed both new deterministic regressions. Tests
  now cover first-server failure with working fallback, the selected nonzero
  index, cached-answer invalidation, unchanged-refresh selection, all-server
  failure and recovery, discovery diagnostics, and TCP DNS fallback.
- Both source lines passed all 30 local verifier stages, including ordinary,
  native-only and all-feature tests, doctests, formatting and lint checks.
- Windows x86 passed 53 native-only DNS tests and 57 default-feature DNS tests
  for each canonical runtime. The registry workflow repeats these against the
  downloaded package and runs native-only x86 consumer tests.
- Hosted verification covers Windows, Ubuntu, Intel macOS and Apple Silicon,
  each on stable Rust and Rust 1.85. CI also checks licenses and advisories.
- Every registry job checks fresh and Mio-coexisting consumers, default/native/
  minimal/test-support/bundled-root features, expected absent APIs and 18 local
  example executions. Raw artifact hashes, step results and lock hashes are
  inspected in addition to GitHub job conclusions.
- GitHub Apple Silicon runners were available. Scaleway is not required, and
  no Apple Silicon validation waiver is used for the completed matrices.
- Each published matrix records 20 packaged DNS test executions, 16 consumer
  graphs and 144 local example executions. Every job uses the exact final
  registry checksum, with no local override for NBReq or its platform helpers.

| Line | Full CI | Candidate package consumers | Published registry consumers |
| --- | --- | --- | --- |
| 0.3.1 | [10 jobs](https://github.com/madandy24/nbreq/actions/runs/39714321706) | [8 jobs](https://github.com/madandy24/nbreq/actions/runs/39714324901) | [Run](https://github.com/madandy24/nbreq/actions/runs/39716308811) |
| 0.2.2 | [10 jobs](https://github.com/madandy24/nbreq/actions/runs/39715011282) | [8 jobs](https://github.com/madandy24/nbreq/actions/runs/39715014575) | [Run](https://github.com/madandy24/nbreq/actions/runs/39717478841) |

Pre-publication candidate archives can differ by host checkout line endings and
Cargo normalization. Stable Unix candidates match their corresponding canonical
candidate bytes. The final 0.2.2 changelog correction is described below. The
published matrices use the final canonical checksum on every platform.

### Security changes

The first audit rejected the old TLS dependency graph for seven October
advisories. Both releases raise the compatible rustls minimum to 0.23.46,
constrain its existing rustls-webpki component to 0.103.16 or later, and raise
optional webpki-roots to 1.0.10. The packaged lock also selects
webpki-root-certs 1.0.10. The direct WebPKI constraint prevents retained consumer
locks from keeping the vulnerable older transitive version; it adds no new
component or runtime and retains the existing ISC license.

The original dev-only time advisory exception, RUSTSEC-2026-0009, is unchanged;
no exception was added. All 28 distinct selected root, CI, candidate, example
and published consumer locks passed cargo-audit 0.22.2 against fresh official
RustSec database commit `4b587f00996ebe5c05a73eaef105ef241e876414`, with no
unignored vulnerabilities or warnings. The existing Rust 1.85/1.86 application-lock workaround remains
documented in the getting-started guide; newer compilers use normal resolution.

### Retained failed attempts and limits

- The original red regression and initial security rejection are retained.
- The first sandboxed doctest run was denied temporary executable writes; its
  unrestricted rerun passed. A canonical-package test harness first selected a
  path nested inside the repository workspace; it was corrected to use an
  independent temporary workspace without changing packaged code.
- Windows checkout byte normalization initially left stale Git bookkeeping.
  Exact Git blob bytes and unchanged index objects were verified before package
  acceptance. No dirty-check bypass was used.
- The 0.2.2 Windows stable job initially failed the existing 150 ms silent-peer
  TLS test while awaiting the fixture's Accepted event. The unchanged test
  passed five local repetitions. The original failure and the single hosted
  rerun are retained. This is consistent with a short-timeout fixture race;
  the test uses a literal loopback address and does not exercise DNS discovery.
- A changelog heading contained one Windows-1252 dash. The 0.2.2 package is
  corrected before publication; only that encoding and its VCS identity differ
  from the fully checked candidate. All runtime/dependency members are compared
  byte for byte before carrying forward those checks. The already published
  0.3.1 archive retains this cosmetic changelog defect; repository notes correct
  it. README, guides and rustdoc are valid UTF-8 and unaffected.

## GDS integration sequence

The originating GDS session should first change line 42 of
`C:/User/SecuritasNew/gds/rust/gds/Cargo.toml` to:

```toml
nbreq = { version = "=0.2.2", optional = true, default-features = false }
```

Keep the existing native feature wiring. Update its lockfile with:

```powershell
cargo update --manifest-path C:/User/SecuritasNew/gds/rust/gds/Cargo.toml -p nbreq --precise 0.2.2
```

Build the existing i686 target, restart through the usual GDS process, and verify
both primary and backup CAT2/WebRPC polling across a DNS refresh/topology change.
Check service recovery, useful failure detail if no server is usable, cancellation
and shutdown. These live application checks were deliberately left to that session.

Once the 0.2 line is verified, change the exact pin to `=0.3.1` and repeat:

```powershell
cargo update --manifest-path C:/User/SecuritasNew/gds/rust/gds/Cargo.toml -p nbreq --precise 0.3.1
```

Repeat the behavior checks and review the 0.3 defaults: HTTP connect 10 seconds,
useful-progress inactivity 30 seconds and buffered total 120 seconds; public DNS
total 30 seconds; TCP establishment 10 seconds and pending write inactivity
30 seconds. Quiet established TCP/TLS connections keep no default read timeout.
Existing explicit overrides remain effective; optional timers accept `None`.
See [the migration guide](../docs/migrating-to-0.3.md).

## Evidence and documentation

The release lab is `target/dns-refresh-20261022`. The retained
[evidence archive](evidence/nbreq-dns-refresh-20261022.tar.gz) and
[hash index](evidence/nbreq_dns_refresh_artifacts.json) bind the release identities,
logs, raw hosted artifacts, selected locks, audits and package comparisons. The
archive's complete inventory is checked by read-back. Build/cache trees,
credentials and the advisory database checkout are excluded.

Versioned [0.2.2 rustdoc](https://docs.rs/nbreq/0.2.2/nbreq/) and
[0.3.1 rustdoc](https://docs.rs/nbreq/0.3.1/nbreq/) are live. Eight API pages per
release and all immutable guide/example links were checked. Published guides
match the exact tagged Git bytes. The supplementary
[main-branch CI run](https://github.com/madandy24/nbreq/actions/runs/39716232641)
also passed 10/10. The final local closeout verifier passed all 30 stages.
