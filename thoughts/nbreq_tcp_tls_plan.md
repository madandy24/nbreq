# NBReq standalone TCP TLS

Opened 2026-09-26 (NZST). Working plan and evidence index. Update this checkpoint at every accepted stage.

## Resume checkpoint

| Field | Current state |
| --- | --- |
| Scope | Client-side TLS 1.2/1.3 for standalone TCP: immediate verified TLS and explicit upgrade of an unsplit plain connection. Preserve blocking, nonblocking and manual Engine operation. |
| Authority | Owner authorized coordinated Sol high implementation/testing and independent Astra xhigh review. Reviewer proposes fixes; workers implement; review repeats until findings are resolved. |
| Workspace | `target/worktrees/nbreq-tcp-tls`, branch `codex/tcp-tls`, starting at released-main reporting commit `6ada1167ea599365468d2ebde584fb711ec242a3`. Main and published `v0.2.0` remain separate. |
| Active stage | T1–T4 implementation and structural review complete: 20 transport integration tests, three public API cases, ten directed owner cases and examples are green. Source is freezing for the full verifier, packaging and platform matrix. T0 and runtime reds saved at `07858e3`. |
| Agents | `tls_worker`: Sol high implementation; `tls_tests`: Sol high independent tests; `tls_review`: Astra xhigh, read-only contract/code/evidence review. Root coordinates integration, remote queues and records. |
| Next acceptance | Capture directed registry/capacity logs and full Windows verifier against the committed checkpoint, then package examples/consumer and run the identical source on Windows x86, Linux, both Macs and actual Wine. Reviewer reinspects frozen diff and raw evidence. |
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
| T1 | Reusable TLS and immediate connect | Meaningful reds, verified TLS loopback/read/write, wrong-name/untrusted-root failures, no HTTP ALPN leakage, timeouts/cancel/manual mode; existing HTTPS regression checks pass; review findings resolved. | Implemented and structurally reviewed; frozen verifier pending |
| T2 | Explicit TLS upgrade | SMTP-style local negotiation, clean boundary or approved pre-read policy, rejection/ownership tests, no plaintext leakage or fallback, cancellation/deadline/drop races; review findings resolved. | Implemented and structurally reviewed; frozen verifier pending |
| T3 | Lifecycle, boundedness and adversarial coverage | Tiny queues, multi-record fragmentation, slow peers, close_notify/bare EOF, TLS1.2/1.3, mixed HTTP/TLS work, memory/retained-capacity evidence, full verifier and review. | Focused checks green; full verifier and final evidence review pending |
| T4 | Examples and independent consumers | Immediate-TLS and STARTTLS-style examples with bounded deadlines; docs/API/feature boundary and compatibility checks; examples build from packaged candidate. | Examples/guide/probe implemented; packaged execution pending |
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

Wine preparation rechecked by read-only bridge request `20260925-132425-6e02699b`: Linode still runs Wine5.0 (Ubuntu5.0-3ubuntu1); the private Win32 prefix `/tmp/nbreq-wine-dns-lab-20260910/prefix` exists. The previously audited app-local `bcryptprimitives.dll` helper at `/tmp/nbreq-wine-dns-gate-final-20260910/bcryptprimitives.dll` matches SHA256 `92d437dc538ef6ddfae7fc0b2140ec407a6dacfb0c899ec67a6812e84387de86`. It delegates ProcessPrng to Wine BCryptGenRandom for modern Rust startup; it is not a TLS/DNS fix. Record its use separately in eventual Wine results, keep it beside test executables, and make no prefix/system/application changes.

Local baseline host: Windows10.0.26200, x86_64-pc-windows-msvc Rust1.97.1/Cargo1.97.1. Installed toolchains include i686 stable and Rust1.85.0; the x64 toolchain also has the i686-pc-windows-msvc target. Final evidence must name the compiler actually used, not assume every machine's `stable` is identical.

Memory-fixture preflight: Linux request `20260925-141852-a0a564af` confirms OpenSSL/PythonSSL1.1.1f and 13GB free. ARM request `20260925-141905-9b1ab9ae` confirms 167GiB free, CLI LibreSSL3.3.6 and PythonSSL LibreSSL2.8.3. Follow-up `20260925-141941-0e0a7337` confirms its system Python lacks TLS1.3 and no alternate Python was present in the checked standard locations. The observation harness therefore offers an explicit verified TLS1.2 fixture for matched Linux/ARM observations; it never silently downgrades. Rust integration tests still exercise both TLS versions. No host runtime installation is needed.

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

TLS reserves an extra 256 KiB against the existing shared queued-byte budget before immediate admission or upgrade freeze. The reservation is logical and lives until the I/O state releases its permits; it does not allocate the entire allowance. Following NR-08, the implementation keeps one exact 64 KiB wire staging Vec per live TLS transport to avoid reallocating merges. Other NBReq-owned bounds: 64 KiB handshake/control output Vec, 18 KiB reactor ciphertext length (conservatively up to 36 KiB capacity), exact 16/18 KiB reactor input, 16 KiB worker input copy, exact 34 KiB plaintext result/retained allocation, and a 16 KiB application batch retaining its existing send credits. Reviewer accepted conservative simultaneous staging capacities of 248 KiB, or 204 KiB during reactor growth after the control-result allocation is gone. The earlier 166 KiB length sum and 244 KiB capacity estimate are superseded. Accepted send Vecs are compacted to their length only after every refusal check; failed sends retain the original allocation. A partially staged large source Vec retains its send charge until completely staged, preventing refill against retained storage. Drain plaintext partially into tiny user windows. Ciphertext/control progress must never refund application credit early. Rustls outgoing buffering is limited to 64 KiB; its internal verifier/certificate/platform allocations are separately measured and are not covered by a whole-process memory guarantee. The existing 512 KiB cumulative incoming handshake guard is retained, with the shared two-worker/four-queued service. Cancelled worker buffers remain bounded by occupied worker tickets through disposal. Standalone client output above its 64 KiB cap fails explicitly; client certificates are out of scope. Final frozen-source capacity evidence remains an acceptance gate.

TLS1.3 finish drains accepted application output, sends/drains close_notify, then completes write shutdown; the reader remains usable. TLS1.2 peer close closes both application directions and rejects future sends. With no pending application output it replies close_notify. With pending application output it aborts the transport and reports a structured send failure, rather than skipping encrypted records (invalid sequence/framing) or sending further pending app records to reach a close alert. Already OS-accepted bytes cannot be recalled. Settle socket-drained batch credit before deciding whether output remains pending, including same-batch peer close. Locally initiated closure drains accepted output first. Orderly queued input drains before EOF. Bare TCP EOF or malformed TLS is a TLS failure using the documented abort-discard policy; explicit cancellation remains abortive in both versions.

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
| TF-01 | Fixture must observe a complete authenticated KeyUpdate response record; receiving a partial ciphertext record does not prove control progress. | Reviewer resolved after full framed-record consumption |
| TF-02 | Fixture large replies must stream within rustls output capacity. Distinguish raw EOF with unsent output from an actual partially transmitted TLS record. | Reviewer resolved: 16 KiB streaming and RawMidRecord mode |
| TF-03 | Hold the server transport open to verify client close_notify, TLS1.3 remaining read direction, and TLS1.2 peer-close reply. | Reviewer resolved fixture and assertions; execution pending |
| TF-04 | Controlled trailing plaintext/partial upgrade response and an owner gate for already-extracted cleartext events. | Public tests accepted in review; worker-owned internal extracted-event gate pending |
| TF-05 | Adversarial read/finish tests need explicit polling deadlines; a stalled read allowance can hide fixture EOF and defeat inactivity deadlines. | Reviewer resolved: bounded polling/manual tiny-window progress, no per-byte sleep or premature peer-event wait |
| TF-06 | Consuming-upgrade failure tests must prove permit release before shutdown; mid-record truncation test must require the specific Truncated category. | Reviewer resolved; execution pending |
| TDOC-01 | Clarify that local reader remains usable after TLS1.3 local finish. | Reviewer resolved |
| TDOC-02 | Distinguish passive direct waiters from blocking helper mode checks. | Reviewer resolved wording/API checks; regression evidence pending |
| SR-01 | TLS cancel/cancel_all snapshots can lose coverage when success moves pending TLS into live I/O between registry lock acquisitions. Commit cancellation under the original core lock, with cleanup/publication after unlocking. | Worker reports implemented; P1 stays open until tests/re-review |
| SR-02 | TLS terminal readiness must follow failed/cancelled upgrade I/O cleanup and permit release. Mirror canonical-commit / delivery-ready separation, including callback activation. | Worker reports implemented; P2 stays open until tests/re-review |
| SR-03 | Hostname TLS admission takes a DNS permit but must also update borrowed-resolution metrics and high water. | Worker reports implemented; P2 stays open until tests/re-review |
| SR-04 | TlsOptions constructor must validate TLS identity, including rustls numeric-final-label rejection; DNS lookup validation alone is too permissive. Keep native and no-feature behavior consistent. | Worker reports implemented; P2 stays open until tests/re-review |

Reviewer accepted T0 after independently checking this contract and baseline evidence. Particular code-review gates: prove capacities and overlapping copies against the reserve; cancelled worker buffers retain globally bounded worker tickets through disposal; ordinary TLS1.2 peer close replies close_notify, while pending-output failure follows the refined abortive policy above; freeze cleartext I/O before another owner pump and reject late cleartext already extracted from a reactor batch.

| Backend finding | Requirement | State |
| --- | --- | --- |
| NR-01 (P1) | TLS1.2 pending-output peer close must abort without flushing further app ciphertext or splicing a later-sequence alert; already socket-drained batch credit must not cause a false pending-output failure. | Structurally resolved by reviewer; final directed evidence pending |
| NR-02 (P2) | A nonzero but nonrepresentable handshake timeout must be rejected at admission, not overflow to an absent deadline. | Structurally resolved; Duration::MAX test green in development suite |
| NR-03 (P2) | Upgrade must clamp the reactor ciphertext queue to 18 KiB independently of the original plaintext send window. | Structurally resolved; final capacity evidence pending |
| NR-04 (P2) | Owner upgrade boundary recheck must catch cleartext FIN extracted before transition, not only released/aborted I/O. | Structurally resolved; worker directed late-event test requires final evidence review |
| NR-05 (P2) | Deferred handshake EOF followed by an incomplete worker result must fail promptly, not wait for timeout. | Structurally resolved; final directed evidence pending |
| NR-06 (P2) | Promote queued post-handshake controls even when Finished+KeyUpdate arrive in one worker input and no further network input follows. | Structurally resolved; final directed evidence pending |
| NR-07 (P2) | Authenticated read closure is terminal, distinct from backpressure; it must not resume read inactivity and abort a still-usable TLS1.3 writer. | Structurally resolved; public test proves EOF, quiet beyond read timeout, then successful protected send/finish |
| NR-08 (P2) | Prove actual buffer capacities and temporary merge/copy overlap fit the TLS reserve; length caps alone are insufficient. | Structurally resolved, including accepted caller Vec normalization. Ledger accepted conservatively at 248 KiB, 204 KiB during reactor growth; frozen evidence pending |
| EX-01 | Example fixture must enforce its absolute lifetime after accept and interrupt an accepted socket before joining. | Reviewer resolved, including non-retryable ConnectionAborted on absolute expiry; examples rerun green |
| EX-02 | Explain that one-byte application reads do not prevent reactor prefetch; upgrade rechecks NBReq's queue. Make example TLS1.3 directional-close dependence explicit. | Reviewer resolved |
| PRB-01 | Hold probe must not expire its own read inactivity deadline while deliberately idle; recheck live connection/reservation counts after hold. | Reviewer resolved; memory observer review and execution pending |
| MH-01 | Evidence-write failures must not skip owned child/thread/fixture cleanup. | Reviewer resolved: independent cleanup precedes artifact writes |
| MH-02 | Connection setup must not contaminate the idle baseline; closing must not contaminate the held phase. | Reviewer resolved: explicit connecting/closing markers and phase-order checks |
| MH-03 | Expected process-disappearance sampling races must not falsely fail successful completion or hide real sampler failures. | Reviewer resolved: ignore only after a fresh child-exit confirmation |
| PLAT-01 | A supervisor metadata-write failure after launch must still terminate its owned command; source integrity must be rechecked after nested Cargo commands. | Reviewer resolved; injected write-failure and timeout cleanup checks both reaped their child; end-of-gate manifest revalidation added |

The lab's `platform.py` launches bounded isolated host gates and records compiler, source manifest, exact commands and raw logs. Its memory-command deadline is 420 seconds, beyond the fixture-generation plus two observation deadlines, so outer timeout does not preempt normal owned-child cleanup. `collect.py` selects logs/JSON/text evidence while excluding builds and generated fixture certificates/keys. `snapshot.py` creates an explicit committed NBReq-only source whitelist, never the GDS workspace or build directories.

Existing shared-worker tests cover the two-thread/six-ticket bound, running/queued/completed ticket retention, cancelled input capacity through execution, queued cancellation and deadlines, and joined shutdown. Existing HTTPS lifecycle tests cover actual gated verifier saturation, HTTP/manual progress, cancellation and deferred certificate-flight FIN. These remain necessary regression evidence, but do not substitute for directed tests of the new standalone owner restoration path. Those tests and public callback/waiter/split coverage are being added before the source freeze.

Directed test authority notes: a physical FIN sent while certificate verification is held does not imply the reactor observed FIN while its read allowance is zero. The first such test's expectation was invalid; the replacement sequences an already-extracted `PeerClosed` event before worker restoration. Likewise, stock rustls refuses a server KeyUpdate before it receives the client's Finished, so it cannot construct the proposed literal Finished-plus-KeyUpdate flight through its ordinary server API. Reviewer approved a narrower proof: an authenticated idle KeyUpdate passes through a real worker session and normal standalone owner restoration, then its entire response authenticates at the server without an application write. Label that worker-restoration proof accurately; retain the separate public end-to-end idle-KeyUpdate test.

## Evidence

| Check | Source / command | Result |
| --- | --- | --- |
| Windows baseline | `6ada1167ea599365468d2ebde584fb711ec242a3`; `cargo run --manifest-path tools/xtask/Cargo.toml --target-dir C:/User/projects/nbreq/target/tcp-tls-20260926/baseline-xtask -- verify --offline` | All 24 steps passed in 164.894s; `baseline-windows.log` in the evidence lab. |
| Skeleton compile | API/admission scaffold on baseline; `cargo check --all-features` and `cargo test --offline --test tcp_tls --no-run` | Passed; real TLS backend deliberately absent. Development warnings remain until implementation/test use lands. |
| First backend compile | Development tree after `07858e3`; worker `cargo check --all-features` | Passed with actual TLS owner/worker/event wiring. Integration execution and independent source review underway; no success path accepted yet. |
| First focused green | Development tree after `07858e3`; bounded `cargo test --offline --test tcp_tls -- --nocapture --test-threads=1`; raw log `target/tls-test-checkpoint/focused.log` in feature worktree | 16/16 passed in 1.36s; SHA256 `51c4e17b8454c307596a390408113fc0cd1bf3bbbebb7594bca0ec03323192dd`. Root inspected raw log/result. Development evidence only; open review gates remain and final frozen-source runs will supersede it. |
| First example smoke | Development tree; `cargo check --offline --examples`, then bounded C04/C05 runs | Both verified and echoed 24 protected bytes. Logs in `target/tls-test-checkpoint/`; C04 SHA256 `fea8765be3c37d6c72ea41ed5e287c1154ff2a4231edbff0a58e087e327b2403`, C05 `c35367bf1d5ab06831f96ddd6fd5c9568a08db996b174d547f1ec14dfd2ed00c`. Final packaged/frozen rerun pending. |
| Expanded development run | `target/tls-test-checkpoint/focused-expanded.log` | 18/19 passed. New raw-EOF test incorrectly required unread plaintext delivery before failure; TR-05 already selected abort-discard. Corrected expectation must permit a valid prefix, possibly empty, followed by Truncated/Receive, never successful EOF. Preserve failed log; backend policy is unchanged. |
| Hardened development run | `target/tls-test-checkpoint/focused-19.log` and result JSON | 19/19 reported passed after NR hardening; further directional read-timer and source race/capacity tests in progress. C04/C05 also passed after bounded fixture changes. Final frozen-source evidence remains pending. |
| Directional closure development run | `target/tls-test-checkpoint/focused-worker-latest.log` | 20/20 passed in 1.78s, including TLS1.3 read-close followed by idle time beyond read timeout and authenticated write. SHA256 `dc4a60a64d34715cde8e2b24bb0ddd8c977e42cb1133a750801cfd3cfa659339`. Reviewer inspected earlier 20-test log and example reruns; frozen-source evidence remains pending. |
| Standalone verifier gates | `target/tls-test-checkpoint/standalone-owner-gates-3.log` | Three real certificate-verifier gate cases passed: cancellation, timeout, and shutdown close the socket before verifier release; shutdown joins after release. Further owner-path cases follow. |
| Directed owner development checkpoint | `target/tls-test-checkpoint/standalone-owner-10b.log` | 10/10 passed without warnings. Adds real encrypted TLS1.2 unsent-output abort and socket closure, real sent/authenticated same-batch credit, already-observed FIN through an incomplete worker result, authenticated idle KeyUpdate through worker restoration, stale Finished isolation after same-index/new-generation reuse, and HTTP progress beside blocked standalone verification. Reviewer accepted the preceding nine-case proof and requested the mixed-case portability adjustment now applied. |
| Independent probe lint/build | Evidence lab `probe-clippy-dev.log`, `probe-release-dev.log` | Warning-denied all-target lint and release build passed. Windows memory-harness execution follows separately; this is development evidence before source freeze. |
| Windows client-only memory development observation | `memory-windows-dev/report.json`, per-count raw samples/phases and wrapper log; optimized probe SHA256 `43a9cd116a2535775dac5152a0ecd8e362d399d40b30a51473a2c86f6322016c` | Reviewer independently verified 16/32 authenticated TLS1.2 sessions, five-second held phases, selected 16 KiB windows, exact reservations 4,718,592/9,437,184 bytes followed by zero, clean exit and key removal. For 32 sessions: median RSS 9,670,656 -> 16,334,848 -> 13,996,032 bytes (baseline -> held -> released); median private memory 1,699,840 -> 6,184,960 -> 3,801,088 bytes. Idle loopback observation, not a RAM ceiling or load benchmark. |
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
