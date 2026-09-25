# NBReq SMTP — work and progress plan

Opened 2026-09-26 (NZST). Keep this checkpoint current through contract, runtime reds, implementation, independent review, platform checks and live evidence.

## Resume checkpoint

| Field | State |
| --- | --- |
| Objective | Create a useful, separate `nbreq-smtp` sending library on the reviewed NBReq TCP TLS implementation. No mail reader or mail server in this slice. |
| Authority | Owner requested the stretch project, Sol/Astra work team, normal thoughts/progress record, and permits pausing for genuine showstoppers. Reviewer reviews and recommends; worker agents implement corrections; review repeats until satisfied. |
| Worktree | `C:/User/projects/nbreq/target/worktrees/nbreq-smtp`, branch `codex/nbreq-smtp`, based on TLS documentation/evidence checkpoint `c6872493b8f9f6c19cc621dbffdf5b138167e8d9`. Main and `codex/tcp-tls` stay separate. |
| Baseline | TLS production is `bede85c32a41b7099c073e9585517d0ae4027342`, independently accepted on native Windows/Linux/Intel Mac/ARM Mac. Wine5 platform certificate setup/validation remains unresolved; SMTP must preserve verification and disclose that inherited limitation. |
| Active stage | S0–S5 complete for the documented development slice. Astra approved source/docs/tests/helpers and verified frozen platform evidence, with no open blocking finding. One owner-authorized live message was accepted by cavesvr3 over verified STARTTLS. Mailbox receipt remains unconfirmed. |
| Frozen candidate | `d5c0ba13a94261b5b5496c0a8ef63b0e48cb0978`; source manifest SHA256 `642b601e0dc17d080c47a60aae7ecfdfe394ece91c4574e077ea766f564ed3e2`; 135 source/helper files. Behavioral-red checkpoint: `62e0571`. |
| Next decision | Review this first slice with the owner, then choose release/version coordination for NBReq TLS and this crate, and whether authenticated SMTP submission is the next increment. No publishing, main-tree merge, GDS change or mail reader is included. |
| Team | `smtp_worker`: Sol-6 high, package/manifests and `smtp/src`; `smtp_tests`: Sol-6 high, independent fixtures/tests and examples/docs; `smtp_review`: Astra xhigh, read-only design/code/evidence review; root: coordination, plan, source snapshots, host queues and live send. |
| Package boundary | Independent workspace package `smtp/`, Rust 1.85, safe Rust, `publish = false` during development. It needs unreleased local NBReq TLS; published NBReq0.2.0 cannot supply that API. Publication/version coordination comes later. |
| Live authority | Owner permits mail through `cavesvr3.caverock.com` from `andrew@gdsresponse.com` to `andrew@caverock.com`. Root sent exactly one 617-byte test after reviewed frozen checks. Final reply: `250 2.0.0 Ok: queued as 2D78920121`. No retry. |
| Evidence lab | `C:/User/projects/nbreq/target/smtp-20260926/`. Preserve compiler/commit/hash identity, actual runtime reds, raw passing and failed logs, and final SMTP acceptance separately from mailbox delivery. |

## Stages

| ID | Work | Acceptance | Status |
| --- | --- | --- | --- |
| S0 | API and protocol contract | Concrete API, supported subset, delivery/cancellation/deadline semantics, bounds and test authority approved by reviewer | Accepted 2026-09-26; decisions below |
| S1 | Compiling scaffold and behavioral reds | Independent tests run and fail for absent behavior, not merely missing symbols; root inspects raw failures | Passed: six meaningful runtime reds, 0.04s, raw `runtime-red-02.log` |
| S2 | Core submission | EHLO, envelope, recipients, DATA framing, limits and structured outcomes; blocking and caller-driven nonblocking progress | Complete; reviewed and locally green |
| S3 | Security and failure behavior | Required STARTTLS/direct TLS, capability reset, verified identity, cancellation/timeout/ambiguity and partial-recipient proof | Complete; reviewed and locally green |
| S4 | Independent review and consumer examples | Findings fixed by workers and re-reviewed; public examples/docs compile; strict lint/MSRV and relevant NBReq regressions pass | Complete: source/docs/tests/helpers independently approved; native platform, MSRV and NBReq regressions passed |
| S5 | Frozen platform and live evidence | Windows/Linux/Macs as useful; one owner-authorized mail submission with exact frozen source and acceptance evidence; no unsupported delivery claim | Complete: five native architecture checks, Windows/Linux Rust1.85, and one server-accepted owner message; evidence details below |

Each stage uses contract -> compiling behavioral red -> implementation -> passing evidence -> independent review -> worker correction -> re-review. A fixture, selector, environment or packaging failure remains labeled as such. Do not make tests pass by weakening verification or hiding inconvenient outcomes.

## Accepted first-slice contract

Use NBReq's Engine-owned TCP/TLS connections for one bounded SMTP submission operation. Avoid an async runtime and per-send thread. `SmtpClient::new(&Engine)` owns a connector/mode snapshot without retaining an Engine borrow. `submit(SendRequest)` returns a poll/cancel operation; manual callers drive their Engine between polls. The blocking convenience rejects manual mode before submitting. Add a small read-only `Engine::run_mode()` accessor for reliable mode detection. Pending connection waiters do not cancel on drop themselves, so SMTP explicitly cancels pending handles and releases owned connections.

Transport requires verified STARTTLS or immediate TLS. There is no plaintext policy, opportunistic downgrade or automatic retry. TLS identity remains independent of a literal socket endpoint. Keep the protocol byte boundary clean for consuming TLS upgrade, reject stale buffered plaintext, then discard old capabilities and issue EHLO again.

SMTP acceptance occurs only after parsing final DATA positive completion (normally `250`), and means the server has accepted responsibility, not that the destination mailbox has received the message. Admission of the complete DATA terminator to NBReq's send queue starts uncertainty; disconnect, malformed reply, timeout or cancellation thereafter cannot imply safe retry. Final positive completion permanently latches acceptance; QUIT, cancellation or shutdown cannot revoke it. Delivery states distinguish Accepted, Rejected, NotAccepted and Uncertain. Explicit rejection retains its protocol stage. All terminal outcomes remain stable on later cancellation/polling. Per-recipient results retain accepted-for-transaction, rejected, unresolved (RCPT admitted, reply absent) and truly unattempted entries. `RequireAllRecipients` is the default: attempt all RCPTs, then skip DATA if any rejects, with a distinct recipient-policy failure reason. `AcceptedRecipients` explicitly opts into submission to the accepted subset.

The initial exact-250-only sketch was corrected during RFC/code review: SMTP requires clients to interpret unknown valid codes by their first digit. Validate reply-code syntax (first digit 2–5, second 0–5), accept positive completion at ordinary completion phases, and accept intermediate replies only for initial DATA. STARTTLS still requires exactly `220`; an intermediate or malformed final DATA reply remains Uncertain. `421` stops the whole session immediately. Replies accept valid horizontal tabs in text as well as printable ASCII.

Validate envelope addresses, EHLO identity and message framing before network activity. The initial API takes prepared, canonical CRLF, 7-bit RFC5322 content: at least one syntactically valid header, valid continuation framing, a header/body separator, no NUL/bare CR/bare LF/non-ASCII, and content lines at most 998 octets. A missing final CRLF is appended through a small suffix. Keep this supported subset explicit. The input allocation is owned once; validation may scan it at construction, but polling neither rescans the whole message nor creates a second message-sized encoded copy. Incremental DATA dot stuffing needs a byte-exact independent oracle, including terminator lookalikes and chunk boundaries.

AUTH, SIZE and 8BITMIME are deferred in this first slice. There is no credentials API; the target is a relay that already permits the supplied sender/recipient, including the owner's authorized test server. A typical authenticated submission service will need a follow-up. Also deferred: PIPELINING, CHUNKING/BDAT, SMTPUTF8, OAuth, MX routing, connection pools, retry spool, MIME composition/attachments and mail reading. Existing MIME builders can supply suitably prepared 7-bit content; the SMTP crate does not claim to compose full mail formats.

Deadlines are finite and overflow-checked. Defaults are 30 seconds per command/phase and 120 seconds overall, configurable submission policy rather than RFC relay-server timeout recommendations. A slow final DATA response can result in Uncertain; this is a reason to size deadlines carefully, never to retry blindly. Distinguish total/phase timeout, NBReq transport inactivity, cancellation and Engine shutdown. Each poll performs at most 64 state/I/O steps and 16KiB of DATA progress, with staging at most 4KiB. Failure/drop must release connection permits and buffers.

Initial caps are 8MiB message including any appended CRLF, 100 recipients, 512-octet reply lines including CRLF and 8KiB/32 lines per complete reply. Retaining all recipient replies can use approximately 100 x 8KiB plus metadata, in addition to one message, one active reply, small staging and the separate NBReq TCP/TLS budget. These are per-operation logical bounds, not a process RAM ceiling.

Admission also bounds retained allocation capacity, so an overallocated input cannot bypass the stated limits. Request diagnostics redact the raw message; envelope diagnostics use metadata. Framing checks are intentionally not full RFC5322 semantic validation.

An optional fixed-size transport diagnostic retains NBReq's existing error classification enums (TLS, DNS, timeout, stage and resource limits), without free-text errors or refused payload buffers. This keeps certificate and transport failures diagnosable within the same allocation/privacy bounds.

## Review questions and decisions

| ID | Question | State |
| --- | --- | --- |
| SQ-01 | Exact public API and nonblocking/manual progress contract | Accepted: owned connector/mode, submit/poll/cancel, stable retained outcome, caller drives manual Engine, blocking rejects manual mode |
| SQ-02 | Recipient rejection policy | Accepted: RequireAllRecipients default; AcceptedRecipients explicit opt-in; preserve all per-recipient states |
| SQ-03 | Message representation | Accepted: owned prepared 7-bit CRLF message, framing validation before network, optional final CRLF, incremental dot stuffing |
| SQ-04 | Irreversible/uncertain point and final-positive-completion precedence | Accepted: complete terminator admitted starts Uncertain; parsed final positive completion permanently records Accepted |
| SQ-05 | Credentials and SMTP extensions | Deferred: no AUTH/SIZE/8BITMIME or credentials API in first slice; transparent relay-only limitation |
| SQ-06 | Bounds/default timeouts | Accepted numeric limits above; implementation and adversarial tests must substantiate them |

## Test and evidence requirements

- Bounded independent loopback SMTP peer with plaintext, STARTTLS and immediate TLS, generated private CA, deterministic event gates and socket cleanup.
- Byte-exact command and DATA transcript oracle, fragmented/multiline/coalesced/oversize/mismatched replies, input injection, absent/rejected STARTTLS, wrong identity and untrusted root.
- Recipient rejection policy, final DATA rejection, accepted response then QUIT failure, uncertainty after terminator, no implicit retransmission.
- Capability reset after STARTTLS, unsupported message/envelope syntax and limits fail explicitly; no credentials API or plaintext fallback.
- Manual and spawned operation, partial writes and tiny windows, bounded poll work, cancellation/deadline/shutdown/drop cleanup, small requests making progress beside a stalled submission where supported.
- Windows x64 and Rust1.85 first; independent frozen Linux and Mac checks where useful. Inherited Wine5 trust failures are not fixed by SMTP or relabeled as passing.
- Root owns all host queues and actual mail submission; agent fixtures must use loopback only. Preserve the final DATA reply and test message ID; distinguish server acceptance from a recipient confirming receipt.

## Live message rule

Create the full harmless test message locally first with Date, a unique Message-ID, the authorized sender/recipient and a clear NBReq SMTP test subject/body. Review its exact bytes before dispatch. Use `cavesvr3.caverock.com:25`, required verified STARTTLS, no credentials. Record the final server result and stop on acceptance or uncertainty. If the server rejects before DATA, diagnose the specific policy without broadening recipients or weakening TLS. Do not alter the mail server, its trust stores or GDS.

## Working record

- **2026-09-26, S0:** Sol worker/tests and Astra reviewer agreed the concrete bounded first slice above. Root accepted the narrower extension scope explicitly. Reviewer corrected five API ambiguities before runtime reds: message diagnostics, rejection stage, recipient-policy reason, terminal stability and unanswered recipient state. Worker applied the corrections to the compiling scaffold; implementation remains paused until root inspects the red proof.
- **Host readiness:** all three existing queues confirmed the expected test hosts: Linux `gds-srv-test2` x86_64, Intel Mac `Andrews-iMac2019.local` x86_64, and Scaleway Mac arm64. Linux reports 4.8GiB free, so isolated SMTP builds will disable debug data/incremental output to limit disk use. No remote cleanup or server changes. Raw identity results are retained in the SMTP evidence lab's `hosts.json`.
- **S1 proof:** Windows Rust1.97.1 compiled and ran six independent SMTP integration tests; all six failed on the intentional scaffold's premature/incorrect result, rather than missing symbols or fixture timeouts. Cases cover implicit TLS/exact DATA, required STARTTLS/re-EHLO, missing STARTTLS rejection, no-final-reply uncertainty, default recipient policy and explicit subset policy. Command and raw output are `runtime-red-02-command.txt` and `runtime-red-02.log` in the evidence lab. The initial failed build attempt remains separate evidence, not a behavioral red claim.
- **S2/S3 first implementation:** the production state machine compiles. Test author owns the first green/debug run; Astra independently reviews the full code. An additional validation-red run compiled four tests: diagnostics redaction passed, while envelope injection, EHLO injection and zero-deadline admission failed for absent validation. Evidence is `validation-red.log`.
- **Live preparation:** one 617-byte ASCII message was prepared and inspected locally, SHA256 `5a08a7b2ad6d62727e8137a89754906b2caf666b86fb041b90b9dec624173f34`. Its Message-ID and exact bytes remain in the evidence lab under `live/`. This was prepared before dispatch; the later single-send result is recorded below. No credentials or additional recipients are involved.
- **First green/review cycle:** 17 integration plus four validation tests passed. Review then identified valid IPv6 EHLO literal handling, transport classification/admission diagnostics, healthy TLS close-notify cleanup, tabbed reply text and unknown valid reply-code handling. Workers own corrections and new regression proof; reviewer rechecks. The workspace license inventory regenerated successfully and adds only the local `nbreq-smtp` component, with no new third-party runtime dependency.
- **Review corrections proved:** subsequent logs preserve the valid-tab, DATA359 and final259 reds before correction. The expanded implementation passed 31 integration and six validation tests in `all-targets-checkpoint-02.log`; two README examples compiled as no-run doctests. Tests cover strict peer observation of the client's TLS close-notify, trust/name errors with structured diagnostics, phase/overall/transport timeouts, manual pending-connect drop, live cancellation/slot reuse, and a healthy submission beside a stalled peer. Library and all-target strict Clippy, formatting and Rust1.85 compilation passed. Remaining numeric boundaries and active large-DATA fairness are being added before source freeze.
- **Repository maintenance:** SMTP has six explicit checks in the normal xtask verifier. License generation needs `--workspace` to include this new component; CI and CONTRIBUTING now use that flag, and the regenerated report adds exactly the SMTP component row. The earlier root-only generation passed but did not cover SMTP, so it is not the complete-workspace evidence.
- **Live runner review:** the reviewer required explicit ambiguous-run status, a built-example hash recorded by the passing platform gate, and exclusive ownership of the attempt record. Worker corrected all three. A fixed one-attempt directory and exact message hash prevent accidental replay; a command timeout or unknown launched termination is Uncertain. Mocked, no-network guard/status tests are being retained for final review. The Linux client's observed IPv6 address literal is used as EHLO, not a guessed placeholder domain.
- **Final independent approval:** Astra inspected the corrected source, docs, CLI, verifier changes and raw logs and found no remaining blocking issue. `final-all-targets.log` records 33 integration + seven validation passes; `final-doctests-after-readme.log` records two compile-only README passes after indentation cleanup; `final-clippy.log`, `xtask-tests.log`, `xtask-clippy.log` and formatting checks are green. Focused tests admit valid prepared input up to 8MiB while rejecting oversized capacity/content, successfully send with an omitted request window and a 37-byte Engine default, and keep a 3.2MiB DATA operation actively Pending while a small submission completes, then cancel/reclaim all connection and queue credit. Helper approval includes inspected `helper_mock_check.py`, command record and passing raw log. Platform and live results are not yet claimed.

## Frozen platform evidence

Implementation commit `d5c0ba13a94261b5b5496c0a8ef63b0e48cb0978` is unchanged after the matrix. Source archive `nbreq-smtp-d5c0ba13a942-source.tar.gz` has SHA256 `2ff6fd0d79c7d04a4677fdd18434a93fc16ce907b72e97403af1cbb22eb6ce2c`. Every 64-bit supervisor verifies the same source manifest before and after its checks. Retrieved evidence archives and individual files passed independent hash verification. Astra checked the frozen source, raw results and platform identities and removed the platform condition from its approval.

| Native host | Compiler | SMTP proof | Additional proof |
| --- | --- | --- | --- |
| Windows11 x64 | Rust1.97.1 and 1.85.0 | Each: 33 integration + seven validation + two compile-only doctests | Stable strict all-target Clippy and example build; NBReq all-feature regression: 443 unit + 54 integration + 32 doctests |
| Windows11, 32-bit binaries | Rust1.97.1, `i686-pc-windows-msvc` | 33 integration + seven validation | Actual native execution, example test target compiled; executed test binaries have PE machine `0x014c` |
| Linux x64, kernel5.4.0-216 | Rust1.98.0 and 1.85.0 | Each: 33 integration + seven validation + two compile-only doctests | Stable strict all-target Clippy and example build |
| Intel Mac, macOS15.7.9 | Rust1.98.0 | 33 integration + seven validation + two compile-only doctests | Strict all-target Clippy and example build |
| Apple Silicon Mac, macOS26.6.1 | Rust1.98.0 | 33 integration + seven validation + two compile-only doctests | Strict all-target Clippy and example build |

The supplementary Win32 command was invoked from `target/smtp-20260926/frozen-d5c0ba13a942`; its raw Cargo compile lines name that source directory. `final-provenance.json` records post-test verification of all 135 source files, matching worktree source and the executed test-binary hashes/PE headers. Unlike the four 64-bit supervisors, this supplementary run did not write a contemporaneous cwd/source record. No Mac or Win32 MSRV run is claimed. These are correctness checks, not comparable performance benchmarks.

## Live owner-message result

At approximately 2026-09-25 16:42 UTC (2026-09-26 NZST), the passing Linux build submitted exactly one prepared message to `cavesvr3.caverock.com:25`, using required verified STARTTLS, sender `andrew@gdsresponse.com` and recipient `andrew@caverock.com`. The actual Linux example binary SHA256 was `248c0a03b61b587920978d1804439a24c04ef4c5b07791699472ae082ebb4a70`, matching its recorded passing gate. The runner verified source, binary and message hashes and acquired an exclusive one-attempt record before dispatch.

- Message-ID: `<179035189980.69072.1478554498044923328.nbreq-smtp@gdsresponse.com>`.
- Subject: `NBReq SMTP development test - verified STARTTLS`.
- Final DATA reply: `250 2.0.0 Ok: queued as 2D78920121`.
- CLI exit0, structured status `server-accepted`; no automatic retry and no further submission.
- Mailbox receipt is **not independently confirmed**. This result establishes server acceptance only.
- SMTP implicit TLS was exercised with independent local peers on all native platforms; the owner-server SMTP test used STARTTLS. Earlier TCP TLS evidence separately covers the owner's implicit-TLS IMAP port993 without reading mail.

The prepared-message record intentionally retains its original pre-send state; the immutable `owner-live-d5c0ba13a942/result.json` and raw log record the subsequent accepted attempt.

## Handoff and remaining work

The first slice is useful for a relay that already permits the supplied sender and recipient. Typical authenticated mail submission needs AUTH support next; keep credential handling, redaction and replay rules under a separate reviewed increment. MIME composition/attachments can remain a separate component supplying prepared 7-bit bytes. No IMAP implementation is present.

Before public release, coordinate an NBReq version containing TCP TLS, update the SMTP dependency/version policy, enable packaging deliberately, and run fresh packaged-consumer checks. `publish = false` remains intentional; published NBReq0.2.0 alone cannot compile this crate. The inherited Wine5 platform trust problem remains a separate investigation; no Wine SMTP compatibility claim or insecure workaround is made. Main, the completed TLS branch and GDS remain untouched by this SMTP development checkpoint.

Permanent raw evidence and reproduction helpers are indexed by [nbreq_smtp_artifacts.json](evidence/nbreq_smtp_artifacts.json) in [nbreq-smtp-20260926.tar.gz](evidence/nbreq-smtp-20260926.tar.gz). They include behavioral reds, corrected greens, frozen source, platform/compiler identities, exact live message and server acceptance. Build outputs, executables, generated private keys, credentials and GDS source are excluded.

The final bundle contains 162 evidence files, 537,884 compressed bytes, SHA256 `4d7a3cc2bb149301e9b1e6c3fa340a9c76209c0b669c4547a431a3da4676c10d`. Its complete per-file manifest was read back and hash-verified. Astra independently approved the collected live evidence and final handoff plan after checking the message bytes, seven live evidence hashes and the matching Linux binary/source identities. No further code correction was requested.

## References

- [SMTP protocol and DATA semantics — RFC5321](https://www.rfc-editor.org/rfc/rfc5321.html)
- [STARTTLS reset and security — RFC3207](https://www.rfc-editor.org/rfc/rfc3207.html)
- [SMTP authentication — RFC4954](https://www.rfc-editor.org/rfc/rfc4954.html)
- [Mail submission TLS — RFC8314](https://www.rfc-editor.org/rfc/rfc8314.html)
- [SIZE extension — RFC1870](https://www.rfc-editor.org/rfc/rfc1870.html)
- [8BITMIME — RFC1652](https://www.rfc-editor.org/rfc/rfc1652.html)
- Baseline [TCP TLS plan](nbreq_tcp_tls_plan.md), [TLS guide](../docs/tcp-tls.md), and [Wine follow-up](nbreq_tcp_tls_wine_followup.md).
