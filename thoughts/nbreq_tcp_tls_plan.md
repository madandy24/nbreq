# NBReq standalone TCP TLS

Opened 2026-09-26 (NZST). Working plan and evidence index. Update this checkpoint at every accepted stage.

## Resume checkpoint

| Field | Current state |
| --- | --- |
| Scope | Client-side TLS 1.2/1.3 for standalone TCP: immediate verified TLS and explicit upgrade of an unsplit plain connection. Preserve blocking, nonblocking and manual Engine operation. |
| Authority | Owner authorized coordinated Sol high implementation/testing and independent Astra xhigh review. Reviewer proposes fixes; workers implement; review repeats until findings are resolved. |
| Workspace | `target/worktrees/nbreq-tcp-tls`, branch `codex/tcp-tls`, starting at released-main reporting commit `6ada1167ea599365468d2ebde584fb711ec242a3`. Main and published `v0.2.0` remain separate. |
| Active stage | T1/T2 API skeleton and independent behavioural reds. T0 accepted by Astra; no implementation accepted yet. |
| Agents | `tls_worker`: Sol high implementation; `tls_tests`: Sol high independent tests; `tls_review`: Astra xhigh, read-only contract/code/evidence review. Root coordinates integration, remote queues and records. |
| Next acceptance | Runtime reds demonstrated for immediate TLS and same-socket upgrade. Implement real backend plus SR-01 through SR-04 fixes, then focused green evidence and independent re-review. |
| Release boundary | Local checkpoints and test uploads are in scope. No crate publication, GDS integration/deployment, mail submission/authentication, or full mail-module implementation. |
| Evidence lab | `C:/User/projects/nbreq/target/tcp-tls-20260926/`. Logs name their exact source/command and distinguish fixture, native host, Wine and live-server evidence. |

## Working contract (T0 accepted)

1. Reuse the native reactor owner, certificate verification and bounded handshake worker service. Extract/grow the reusable rustls seam with HTTPS regression checks. TCP must not inherit HTTP ALPN.
2. Immediate TLS does not publish a usable connection before certificate verification and handshake success. Its operation deadline covers queued work, DNS, TCP and TLS.
3. Upgrade requires exclusive ownership of the unsplit plain connection. No split-half upgrade or socket escape. Prefer a distinct verified `TlsConnection` with familiar read/send/split/finish operations; pending operations must support manual `drive_until`, cancellation and direct waiting.
4. Define admission versus accepted-upgrade failure ownership explicitly. After TLS has started, failure/cancellation closes the connection; no plaintext fallback. Caller protocol state must consume the complete upgrade response and must not reuse pre-TLS application data as authenticated input.
5. Define the queued-send/unread-receive boundary before implementation. Do not silently throw away caller bytes, send application plaintext after acceptance, or permit concurrently usable plain and TLS handles. Explicit bounded pre-read handling, or a documented strict clean-boundary rule, must pass independent review.
6. Verification is on by default and uses the configured TLS server identity, distinct from a literal connection endpoint. Reuse Engine custom CA roots. No new automatic verification bypass, SSLv2/3, TLS1.0/1.1, early application data or insecure downgrade.
7. Keep plaintext send credit occupied until its corresponding encrypted output drains. Bound wire input, retained decrypted input, encrypted output and pending handshakes; avoid large eager allocations and per-connection threads. Document rustls/platform allocations separately from NBReq-controlled queue bounds rather than claiming a process memory cap.
8. TLS control traffic can require writes during reads. Backpressure must not cause busy loops, uncontrolled buffering or starvation. Post-handshake records, cancellation and stale worker results must obey the same owner lifecycle as HTTPS.
9. Define orderly close, truncation, finish and half-close behaviour explicitly for TLS1.2 and TLS1.3. Orderly closure drains authenticated queued input before EOF. Abrupt TLS/transport failure uses existing TCP abort-discard semantics: unread queues are discarded, already-returned bytes remain with the caller. Never equate bare TCP EOF with authenticated TLS closure.
10. Preserve canonical exactly-once completion and callback dispatch outside network processing/locks. Wrong-mode waits, request admission, Engine shutdown and held-handle cleanup retain existing guarantees.

## Stages and acceptance

| ID | Stage | Acceptance | Status |
| --- | --- | --- | --- |
| T0 | Contract and test design | Reviewer accepts concrete public API, lifecycle, byte-boundary, closure and memory contracts; workers have disjoint file ownership. | Accepted 2026-09-26 |
| T1 | Reusable TLS and immediate connect | Meaningful reds, verified TLS loopback/read/write, wrong-name/untrusted-root failures, no HTTP ALPN leakage, timeouts/cancel/manual mode; existing HTTPS regression checks pass; review findings resolved. | Pending |
| T2 | Explicit TLS upgrade | SMTP-style local negotiation, clean boundary or approved pre-read policy, rejection/ownership tests, no plaintext leakage or fallback, cancellation/deadline/drop races; review findings resolved. | Pending |
| T3 | Lifecycle, boundedness and adversarial coverage | Tiny queues, multi-record fragmentation, slow peers, close_notify/bare EOF, TLS1.2/1.3, mixed HTTP/TLS work, memory/retained-capacity evidence, full verifier and review. | Pending |
| T4 | Examples and independent consumers | Immediate-TLS and SMTP STARTTLS examples with bounded deadlines; docs/API/feature boundary and compatibility checks; examples build from packaged candidate. | Pending |
| T5 | Platform and live proof | Frozen source passes Windows x64/x86, Linux, Intel/ARM Macs on stable/MSRV as applicable, actual Wine separately, bounded owner-authorized live probes; final Astra review resolved. | Pending |

Each stage: contract review -> demonstrated reds -> worker implementation -> passing evidence -> independent code/evidence review -> worker fixes -> re-review -> accepted checkpoint. Compilation failure alone is not a behavioural red. Full-suite repeats require changed inputs or an unresolved concern.

Root owns remote bridge queues and source snapshots. Workers must not edit overlapping files concurrently or run evidence against a changing source tree. The reviewer inspects requirements, exact diff and raw logs independently of author conclusions. Findings keep explicit IDs and open/resolved status.

## Host readiness and live test authority

All three visible PuTTY bridges verified on 2026-09-25 UTC; owner enabled FULL CONTROL in each. Use the GDS bridge documentation at `C:/User/SecuritasNew/gds/doc/ai_agents/PUTTY_PLINK_BRIDGE.md`. Do not use direct Plink/Pscp to bypass the bridge.

| Host / queue | Verified readiness | Request |
| --- | --- | --- |
| Linux / `putty_bridge` | Saved session `gds-client-01i linode`, hostname `gds-srv-test2`, x86_64; stable plus default1.85 installed; Wine5.0 Ubuntu; 13GB free. Python default3.8; pure-Rust verifier is available, newer-Python helpers need explicit handling. | `20260925-123923-b5ac441e` |
| Intel / `putty_bridge_intel_mac` | Saved session `intel-mac`, macOS15.7.9 x86_64; stable1.98 and1.85; 51GiB free. Earlier timeouts followed an IP change; owner updated session and accepted host key, final check succeeded. | `20260925-124453-abe856a8` |
| ARM / `putty_bridge_scaleway_nbreq` | Saved session `scaleway-nbreq`, macOS26.6.1 arm64; stable1.98 and1.85; 167GiB free. Python default3.9. | `20260925-123930-d57dcf25` |

Owner explicitly offered `cavesvr3.caverock.com:25` (SMTP STARTTLS) and `:993` (IMAP immediate TLS). Read-only OpenSSL1.1.1f probes from Linode passed both with TLS1.3, verified hostname and chain, request `20260925-125101-807cbcd1`. This is endpoint readiness, not NBReq implementation evidence. No credentials, authentication, message submission or server changes. Use a few bounded protocol/handshake smoke checks; fault, load and hostile tests use local fixtures.

## Decisions and review log

| ID | Decision / finding | State |
| --- | --- | --- |
| TD-01 | Client TLS and local protocol fixtures first; full `nbsmtp` and POP/IMAP modules follow separately. | Accepted scope |
| TD-02 | Explicit unsplit ownership for upgrades; distinct TLS connection and concrete signatures below. | Accepted T0 |
| TD-03 | Share Engine network and bounded verifier services; generic TLS uses empty ALPN by default. | Accepted T0 |
| TD-04 | Strict clean cleartext boundary, all consumed upgrade failures abort, explicit version-specific TLS closure. | Accepted T0 |

### Accepted concrete API and lifecycle

`TlsOptions::new(server_name) -> Result<TlsOptions, Error>` validates the certificate/SNI identity independently of destination lookup, supporting DNS names and IP certificate identities (no SNI for IP literals). A `handshake_timeout(Duration)` setter establishes a finite timeout from TLS admission (including worker waiting), default 10 seconds; zero is rejected at admission without panicking. Immediate connect observes the earlier of this and the original connect deadline. No additional ALPN knobs or client-auth configuration in the first slice; generic ALPN is empty and HTTPS remains `http/1.1`.

Public operations:

```text
TcpConnector::start_tls(request, options, callback) -> Result<TlsConnectHandle, Error>
TcpConnector::submit_tls(request, options) -> Result<PendingTlsConnect, Error>
TcpConnector::execute_tls(request, options) -> Result<TlsConnection, ExecuteError>
TcpConnection::start_tls(self, options, callback) -> Result<TlsConnectHandle, Error>
TcpConnection::submit_tls(self, options) -> Result<PendingTlsConnect, Error>
TcpConnection::into_tls(self, options) -> Result<TlsConnection, ExecuteError>
```

One TLS establishment completion/wait family serves immediate connect and upgrade, with `Completed(TlsConnection)`, `Failed(Error)` and `Cancelled`. Waiter-local timeout returns the still-live pending operation. `TlsConnection`, `TlsReader`, `TlsWriter` preserve the existing byte-I/O shapes and non-Clone ownership. The pending type supports the sealed `WaiterTarget` contract.

Every consuming upgrade error aborts the connection, including pre-admission errors. No ownership-return error type or automatic retry is introduced. Admission atomically freezes plaintext I/O and rejects unread input, queued/pump output, existing finish/FIN/abort/transition. The owner rechecks the boundary; data already extracted from a reactor batch cannot be silently delivered across the transition. A caller using its own protocol read buffer must account for all prefetched bytes before requesting upgrade. No split-half upgrade exists.

Immediate TLS consumes one existing TCP connection permit and one TCP connect outcome, published only after TLS verification. Upgrade keeps the original live RequestId, socket, connection/queue permits and cancellation handles; it is not a second TCP connection and does not increment TCP connect counts. TLS establishment has one terminal outcome even if cancel/stop races its worker result. A parallel TLS establishment state and sink retain canonical completion until verified success; the owner may create private byte-I/O state at TCP connection but never publishes it before verification. Cancellation must reach both a pending TLS state and any already-created live byte-I/O state.

TLS reserves an extra 256 KiB against the existing shared queued-byte budget before immediate admission or upgrade freeze. The reservation is logical, not an eager allocation, and lives until the I/O state releases its permits. NBReq-owned staging caps: 64 KiB handshake/control output, 18 KiB reactor ciphertext output, 18 KiB reactor input event, 16 KiB worker input (moved), 34 KiB retained plaintext, and one 16 KiB application batch retaining its existing send credits. Even counting all those simultaneously is 166 KiB; implementation must check capacities and temporary overlapping allocations against the reserve. Drain plaintext partially into tiny user windows. Ciphertext/control progress must never refund application credit early. Rustls outgoing buffering is limited to 64 KiB; its internal verifier/certificate/platform allocations are separately measured and are not covered by a whole-process memory guarantee. The existing 512 KiB cumulative incoming handshake guard is retained, with the shared two-worker/four-queued service. Standalone client output above its 64 KiB cap fails explicitly; client certificates are out of scope.

TLS1.3 finish drains accepted application output, sends/drains close_notify, then completes write shutdown; the reader remains usable. TLS1.2 peer close closes both application directions, replies close_notify, and rejects future sends; pending unsent accepted application output causes explicit failure instead of successful finish. Locally initiated closure still drains accepted output first. Orderly queued input drains before EOF. Bare TCP EOF or malformed TLS is a TLS failure using the documented abort-discard policy; explicit cancellation remains abortive in both versions.

### Independent review findings

| Finding | Requirement | State |
| --- | --- | --- |
| TR-01 | Freeze exact API, strict upgrade boundary and consumed-connection failure ownership. | Contract resolved; implementation tests required |
| TR-02 | Ciphertext progress cannot refund plaintext directly; bounded application-batch ledger and control records with zero credit. Define fixed extra reserve. | 256 KiB design accepted; capacity/overlap and worker-ticket evidence required |
| TR-03 | Flush post-handshake control output without a user send; test KeyUpdate while application idle. | Required acceptance |
| TR-04 | Distinguish TLS1.2 bidirectional peer closure from TLS1.3 directional closure; report truncation. | Design accepted; pending client-output/reply evidence required |
| TR-05 | Existing TCP abort discards unread bytes; avoid falsely promising drain-before-error without retaining permits. | Contract resolved: existing abort-discard semantics; orderly close still drains |
| TR-06 | Empty standalone ALPN, unchanged HTTPS ALPN, separate TLS identity from endpoint. | Required acceptance |
| TR-07 | Preserve bounded shared workers, generation identity, deferred EOF, cancellation and deadlines. | Required acceptance |
| TF-01 | Fixture must observe a complete authenticated KeyUpdate response record; receiving a partial ciphertext record does not prove control progress. | Open; tests worker implementing reviewer recommendation |
| TF-02 | Fixture large replies must stream within rustls output capacity. Distinguish raw EOF with unsent output from an actual partially transmitted TLS record. | Open; tests worker implementing reviewer recommendation |
| TF-03 | Hold the server transport open to verify client close_notify, TLS1.3 remaining read direction, and TLS1.2 peer-close reply. | T3 evidence gate |
| TF-04 | Controlled trailing plaintext/partial upgrade response and an owner gate for already-extracted cleartext events. | T2 evidence gate |
| TDOC-01 | Clarify that local reader remains usable after TLS1.3 local finish. | Reviewer resolved |
| TDOC-02 | Distinguish passive direct waiters from blocking helper mode checks. | Reviewer resolved wording/API checks; regression evidence pending |
| SR-01 | TLS cancel/cancel_all snapshots can lose coverage when success moves pending TLS into live I/O between registry lock acquisitions. Commit cancellation under the original core lock, with cleanup/publication after unlocking. | Open P1; assigned to implementation worker |
| SR-02 | TLS terminal readiness must follow failed/cancelled upgrade I/O cleanup and permit release. Mirror canonical-commit / delivery-ready separation, including callback activation. | Open P2; assigned to implementation worker |
| SR-03 | Hostname TLS admission takes a DNS permit but must also update borrowed-resolution metrics and high water. | Open P2; assigned to implementation worker |
| SR-04 | TlsOptions constructor must validate TLS identity, including rustls numeric-final-label rejection; DNS lookup validation alone is too permissive. Keep native and no-feature behavior consistent. | Open P2; assigned to implementation worker |

Reviewer accepted T0 after independently checking this contract and baseline evidence. Particular code-review gates: prove capacities and overlapping copies against the reserve; cancelled worker buffers retain globally bounded worker tickets through disposal; TLS1.2 pending-output failure still sends its close_notify response; freeze cleartext I/O before another owner pump and reject late cleartext already extracted from a reactor batch.

## Evidence

| Check | Source / command | Result |
| --- | --- | --- |
| Windows baseline | `6ada1167ea599365468d2ebde584fb711ec242a3`; `cargo run --manifest-path tools/xtask/Cargo.toml --target-dir C:/User/projects/nbreq/target/tcp-tls-20260926/baseline-xtask -- verify --offline` | All 24 steps passed in 164.894s; `baseline-windows.log` in the evidence lab. |
| Skeleton compile | API/admission scaffold on baseline; `cargo check --all-features` and `cargo test --offline --test tcp_tls --no-run` | Passed; real TLS backend deliberately absent. Development warnings remain until implementation/test use lands. |
| Immediate runtime red | `cargo test --offline --test tcp_tls immediate_tls_verifies_ip_identity_and_exchanges_encrypted_bytes -- --exact --nocapture` | Exit101, verified-connection assertion receives `Failed(Unsupported)`; `t0-red-immediate.log`, SHA256 `541509153d76c13ae6584cbae412ad232aab3b6253813367a76a4df2438c4159`. |
| Upgrade runtime red | `cargo test --offline --test tcp_tls starttls_consumes_the_unsplit_plain_connection_and_keeps_the_socket -- --exact --nocapture` | Exit101 after successful cleartext negotiation, upgrade receives `Failed(Unsupported)`; `t0-red-starttls.log`, SHA256 `28ac0ebc221526d9dd291aac9147a451f246312c19c0c764009ded82ba9629a4`. |

Root read both raw red logs before checkpointing. Test source SHA256 `5091570653e2ee74495645ec9b2e68dcb252af34ddc4fe9225fdd93a3fbf12c0`; fixture `af749346aa2cf324fa93e33272971dc25be67c93c72a6a193fa8f016479300f4`. These failures demonstrate missing behavior through a compiling API; they do not establish correctness of the unfinished registry or fixture. Open review findings remain explicit above.

## References

- Existing TCP contract: `src/tcp/mod.rs`, `src/tcp/io.rs`, `src/registry.rs`, `src/waiter.rs`.
- Native TLS/session/worker seam: `src/backend/native_tls.rs`, `src/backend/native_tls/worker.rs`.
- Shared network owner: `src/backend/native_http.rs`.
- SMTP upgrade resets protocol knowledge: [RFC3207](https://www.rfc-editor.org/rfc/rfc3207.html).
- Immediate mail TLS: [RFC8314](https://www.rfc-editor.org/rfc/rfc8314.html).
- IMAP upgrade boundary: [RFC9051](https://www.rfc-editor.org/rfc/rfc9051.html#section-6.2.1).
- Historical Pascal model: `C:/User/SecuritasNew/lib/GPI.pas:1620`; its protocol/legacy fallback policy is not the new NBReq contract.
