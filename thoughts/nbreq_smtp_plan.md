# NBReq SMTP — work and progress plan

Opened 2026-09-26 (NZST). Keep this checkpoint current through contract, runtime reds, implementation, independent review, platform checks and live evidence.

## Resume checkpoint

| Field | State |
| --- | --- |
| Objective | Create a useful, separate `nbreq-smtp` sending library on the reviewed NBReq TCP TLS implementation. No mail reader or mail server in this slice. |
| Authority | Owner requested the stretch project, Sol/Astra work team, normal thoughts/progress record, and permits pausing for genuine showstoppers. Reviewer reviews and recommends; worker agents implement corrections; review repeats until satisfied. |
| Worktree | `C:/User/projects/nbreq/target/worktrees/nbreq-smtp`, branch `codex/nbreq-smtp`, based on TLS documentation/evidence checkpoint `c6872493b8f9f6c19cc621dbffdf5b138167e8d9`. Main and `codex/tcp-tls` stay separate. |
| Baseline | TLS production is `bede85c32a41b7099c073e9585517d0ae4027342`, independently accepted on native Windows/Linux/Intel Mac/ARM Mac. Wine5 platform certificate setup/validation remains unresolved; SMTP must preserve verification and disclose that inherited limitation. |
| Active stage | S1 runtime reds inspected by root: six compiled tests fail on absent scaffold behavior. Save this red checkpoint, then proceed to S2/S3 implementation and adversarial coverage. |
| Team | `smtp_worker`: Sol-6 high, package/manifests and `smtp/src`; `smtp_tests`: Sol-6 high, independent fixtures/tests and examples/docs; `smtp_review`: Astra xhigh, read-only design/code/evidence review; root: coordination, plan, source snapshots, host queues and live send. |
| Package boundary | Independent workspace package `smtp/`, Rust 1.85, safe Rust, `publish = false` during development. It needs unreleased local NBReq TLS; published NBReq0.2.0 cannot supply that API. Publication/version coordination comes later. |
| Live authority | Owner permits mail through `cavesvr3.caverock.com` from `andrew@gdsresponse.com` to `andrew@caverock.com`. Root will send one small, clearly identified test via verified STARTTLS only after local review/tests. No blind retry after uncertain acceptance. |
| Evidence lab | `C:/User/projects/nbreq/target/smtp-20260926/`. Preserve compiler/commit/hash identity, actual runtime reds, raw passing and failed logs, and final SMTP acceptance separately from mailbox delivery. |

## Stages

| ID | Work | Acceptance | Status |
| --- | --- | --- | --- |
| S0 | API and protocol contract | Concrete API, supported subset, delivery/cancellation/deadline semantics, bounds and test authority approved by reviewer | Accepted 2026-09-26; decisions below |
| S1 | Compiling scaffold and behavioral reds | Independent tests run and fail for absent behavior, not merely missing symbols; root inspects raw failures | Passed: six meaningful runtime reds, 0.04s, raw `runtime-red-02.log` |
| S2 | Core submission | EHLO, envelope, recipients, DATA framing, limits and structured outcomes; blocking and caller-driven nonblocking progress | Pending |
| S3 | Security and failure behavior | Required STARTTLS/direct TLS, capability reset, verified identity, protected AUTH if included, cancellation/timeout/ambiguity and partial-recipient proof | Pending |
| S4 | Independent review and consumer examples | Findings fixed by workers and re-reviewed; examples/docs work as a separate consumer; strict lint/MSRV and relevant NBReq regressions pass | Pending |
| S5 | Frozen platform and live evidence | Windows/Linux/Macs as useful; one owner-authorized mail submission with exact frozen source and acceptance evidence; no unsupported delivery claim | Pending |

Each stage uses contract -> compiling behavioral red -> implementation -> passing evidence -> independent review -> worker correction -> re-review. A fixture, selector, environment or packaging failure remains labeled as such. Do not make tests pass by weakening verification or hiding inconvenient outcomes.

## Accepted first-slice contract

Use NBReq's Engine-owned TCP/TLS connections for one bounded SMTP submission operation. Avoid an async runtime and per-send thread. `SmtpClient::new(&Engine)` owns a connector/mode snapshot without retaining an Engine borrow. `submit(SendRequest)` returns a poll/cancel operation; manual callers drive their Engine between polls. The blocking convenience rejects manual mode before submitting. Add a small read-only `Engine::run_mode()` accessor for reliable mode detection. Pending connection waiters do not cancel on drop themselves, so SMTP explicitly cancels pending handles and releases owned connections.

Transport requires verified STARTTLS or immediate TLS. There is no plaintext policy, opportunistic downgrade or automatic retry. TLS identity remains independent of a literal socket endpoint. Keep the protocol byte boundary clean for consuming TLS upgrade, reject stale buffered plaintext, then discard old capabilities and issue EHLO again.

SMTP acceptance occurs only after parsing final DATA `250`, and means the server has accepted responsibility, not that the destination mailbox has received the message. Admission of the complete DATA terminator to NBReq's send queue starts uncertainty; disconnect, malformed reply, timeout or cancellation thereafter cannot imply safe retry. Final `250` permanently latches acceptance; QUIT, cancellation or shutdown cannot revoke it. Delivery states distinguish Accepted, Rejected, NotAccepted and Uncertain. Explicit rejection retains its protocol stage. All terminal outcomes remain stable on later cancellation/polling. Per-recipient results retain accepted-for-transaction, rejected, unresolved (RCPT admitted, reply absent) and truly unattempted entries. `RequireAllRecipients` is the default: attempt all RCPTs, then skip DATA if any rejects, with a distinct recipient-policy failure reason. `AcceptedRecipients` explicitly opts into submission to the accepted subset.

Validate envelope addresses, EHLO identity and message framing before network activity. The initial API takes prepared, canonical CRLF, 7-bit RFC5322 content: at least one syntactically valid header, valid continuation framing, a header/body separator, no NUL/bare CR/bare LF/non-ASCII, and content lines at most 998 octets. A missing final CRLF is appended through a small suffix. Keep this supported subset explicit. The input allocation is owned once; validation may scan it at construction, but polling neither rescans the whole message nor creates a second message-sized encoded copy. Incremental DATA dot stuffing needs a byte-exact independent oracle, including terminator lookalikes and chunk boundaries.

AUTH, SIZE and 8BITMIME are deferred in this first slice. There is no credentials API; the target is a relay that already permits the supplied sender/recipient, including the owner's authorized test server. A typical authenticated submission service will need a follow-up. Also deferred: PIPELINING, CHUNKING/BDAT, SMTPUTF8, OAuth, MX routing, connection pools, retry spool, MIME composition/attachments and mail reading. Existing MIME builders can supply suitably prepared 7-bit content; the SMTP crate does not claim to compose full mail formats.

Deadlines are finite and overflow-checked. Defaults are 30 seconds per command/phase and 120 seconds overall, configurable submission policy rather than RFC relay-server timeout recommendations. A slow final DATA response can result in Uncertain; this is a reason to size deadlines carefully, never to retry blindly. Distinguish total/phase timeout, NBReq transport inactivity, cancellation and Engine shutdown. Each poll performs at most 64 state/I/O steps and 16KiB of DATA progress, with staging at most 4KiB. Failure/drop must release connection permits and buffers.

Initial caps are 8MiB message including any appended CRLF, 100 recipients, 512-octet reply lines including CRLF and 8KiB/32 lines per complete reply. Retaining all recipient replies can use approximately 100 x 8KiB plus metadata, in addition to one message, one active reply, small staging and the separate NBReq TCP/TLS budget. These are per-operation logical bounds, not a process RAM ceiling.

Admission also bounds retained allocation capacity, so an overallocated input cannot bypass the stated limits. Request diagnostics redact the raw message; envelope diagnostics use metadata. Framing checks are intentionally not full RFC5322 semantic validation.

## Review questions and decisions

| ID | Question | State |
| --- | --- | --- |
| SQ-01 | Exact public API and nonblocking/manual progress contract | Accepted: owned connector/mode, submit/poll/cancel, stable retained outcome, caller drives manual Engine, blocking rejects manual mode |
| SQ-02 | Recipient rejection policy | Accepted: RequireAllRecipients default; AcceptedRecipients explicit opt-in; preserve all per-recipient states |
| SQ-03 | Message representation | Accepted: owned prepared 7-bit CRLF message, framing validation before network, optional final CRLF, incremental dot stuffing |
| SQ-04 | Irreversible/uncertain point and final250 precedence | Accepted: complete terminator admitted starts Uncertain; parsed final250 permanently records Accepted |
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

## References

- [SMTP protocol and DATA semantics — RFC5321](https://www.rfc-editor.org/rfc/rfc5321.html)
- [STARTTLS reset and security — RFC3207](https://www.rfc-editor.org/rfc/rfc3207.html)
- [SMTP authentication — RFC4954](https://www.rfc-editor.org/rfc/rfc4954.html)
- [Mail submission TLS — RFC8314](https://www.rfc-editor.org/rfc/rfc8314.html)
- [SIZE extension — RFC1870](https://www.rfc-editor.org/rfc/rfc1870.html)
- [8BITMIME — RFC1652](https://www.rfc-editor.org/rfc/rfc1652.html)
- Baseline [TCP TLS plan](nbreq_tcp_tls_plan.md), [TLS guide](../docs/tcp-tls.md), and [Wine follow-up](nbreq_tcp_tls_wine_followup.md).
