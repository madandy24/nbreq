# NBReq 0.2.1 documentation and examples

Opened 2026-09-27 after the accepted [0.2.1 version change](nbreq_release_api_review.md).

## Checkpoint

| Field | State |
| --- | --- |
| Working branch | `codex/nbreq-smtp`; version metadata/evidence complete at `4f7f202` |
| Current request | Basic examples of TLS on initial TCP connection and a plain TCP connection upgraded to TLS |
| Approach | Polish existing C04/C05 and keep their names; both already use local verified peers. Add both to the existing CI/release example runner. |
| Current scope | TCP example explanations, example index, TLS guide introduction and directly related runner/documentation updates |
| Deferred | Main README, remaining guides and changelog; final package/hosted CI/publication gates |

## Work sequence

| Item | Deliverable | State |
| --- | --- | --- |
| D1 | C04 immediate TLS and C05 STARTTLS; runnable, explained and included in local example checks | Complete: existing programs clarified for 0.2.1, both included in the runner; focused Windows checks pass |
| D2 | Main README and remaining guides describe 0.2.1, trust choices and new APIs | Next documentation pass |
| D3 | Changelog/release notes include additions and reviewed behavior changes | Pending |
| D4 | Final documentation/package link check and release candidate verification | After documentation; existing release gates remain required |

## D1 decisions and checks

- C04 shows `execute_tls`; C05 negotiates a complete plaintext response before the
  consuming `into_tls` call. Keep NBReq operations visible in the example programs.
- No external server, credentials or system trust-store changes are needed. Their
  generated local CA is trusted explicitly with `SuppliedRootsOnly`; certificate
  and IP identity checks remain enabled. Explain the normal platform-trust choice
  for a public server rather than teaching private-root setup as a requirement.
- Preserve the STARTTLS boundary/ownership explanation and TLS 1.3 fixture closure
  qualification. The small text exchange is not an SMTP or IMAP implementation.
- Keep HTTP/DNS examples unchanged. The set contains 19 numbered programs: 11 HTTP,
  three DNS, and five TCP. The local runner should execute 18 cases (11 HTTP,
  five TCP, two additional HTTP controls); DNS remains explicitly opt-in.
- Run the complete local example runner after a default-feature build; also build
  and run both TLS examples with native-only features and Rust 1.85. Check formatting
  and focused strict Clippy. No broad production/remote suite rerun is needed for
  comments, documentation and runner case additions.
- A worker implements changes; independent review checks the teaching examples,
  release-runner integration and actual validation evidence before completion.

Evidence lab: `C:/User/projects/nbreq/target/examples-021-20260927/`.

## D1 result

C04/C05 already supplied the requested local demonstrations. This pass keeps their
client logic and shared TLS fixture intact, adds concise explanations of the direct
and consuming-upgrade calls and trust configuration, and updates the example index
and TLS guide introduction for 0.2.1. CI and packaged-consumer checks now execute
both programs and require their distinct protected-echo success messages. Tool
documentation and the registry runner agree on 19 programs and 18 local cases.

Windows x86_64 validation passed on stable Rust 1.97.1 and minimum Rust 1.85.0:

| Check | Result |
| --- | --- |
| Default-feature build | All 19 example programs built |
| Local example runner | All 18 cases passed, including exact protected replies from C04/C05 |
| Native-only TLS examples | Both built and ran successfully |
| Rust 1.85 TLS examples | Both built and ran successfully |
| Focused strict Clippy / workspace formatting | Passed |
| Source integrity | Example, manifest, lock and runner hashes unchanged across validation |

These checks execute local verified TLS handshakes and application exchanges; they
are not live external-network, remote-platform or final packaged-release evidence.
DNS examples were built but not run against the host's live DNS configuration.
Independent review accepted the code/documentation diff and raw validation evidence
with no remaining findings. No production, dependency or fixture behavior changed.

Retained [evidence](evidence/nbreq-examples-021-20260927.tar.gz): 80 regular members,
52,256 bytes, SHA256
`3bcf1721a3df4fad3f2c0b2554e5deab455be0bbf0f38a8e417883eba88e0463`.
The [artifact index](evidence/nbreq_examples_021_artifacts.json) records member and
executed-binary hashes. Commands, raw logs, example sources and the scoped diff are
included; build outputs and binaries are excluded. D2/D3 are next; rebuild the final
release candidate after documentation is complete.
