# NBReq standalone TCP TLS

Opened 2026-09-26 (NZST). Working plan and evidence index. Update this checkpoint at every accepted stage.

## Resume checkpoint

| Field | Current state |
| --- | --- |
| Scope | Client-side TLS 1.2/1.3 for standalone TCP: immediate verified TLS and explicit upgrade of an unsplit plain connection. Preserve blocking, nonblocking and manual Engine operation. |
| Authority | Owner authorized coordinated Sol high implementation/testing and independent Astra xhigh review. Reviewer proposes fixes; workers implement; review repeats until findings are resolved. |
| Workspace | `target/worktrees/nbreq-tcp-tls`, branch `codex/tcp-tls`, starting at released-main reporting commit `6ada1167ea599365468d2ebde584fb711ec242a3`. Main and published `v0.2.0` remain separate. |
| Active stage | Native implementation and documentation accepted by independent Astra xhigh review on2026-09-26. Production source frozen at `bede85c32a41b7099c073e9585517d0ae4027342`; all implementation findings closed. All four native hosts passed full24-step verifiers and Rust1.85 checks. Packaged examples/consumer and live Linux IMAPS/STARTTLS passed. Wine5 compatibility remains open; optional ARM memory observation is fixture-limited. |
| Agents | `tls_worker`: Sol high implementation; `tls_tests`: Sol high independent tests; `tls_review`: Astra xhigh, read-only contract/code/evidence review. Root coordinates integration, remote queues and records. |
| Next acceptance | Native implementation is ready for owner review. Before claiming Wine compatibility, resolve [the Wine trust follow-up](nbreq_tcp_tls_wine_followup.md) on the intended Wine version. Full Wine acceptance remains open. Choose the next release version/merge separately; no publication or main-tree merge performed. |
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
| T1 | Reusable TLS and immediate connect | Meaningful reds, verified TLS loopback/read/write, wrong-name/untrusted-root failures, no HTTP ALPN leakage, timeouts/cancel/manual mode; existing HTTPS regression checks pass; review findings resolved. | Frozen verifier passed; independent source and directed-evidence review accepted |
| T2 | Explicit TLS upgrade | SMTP-style local negotiation, clean boundary or approved pre-read policy, rejection/ownership tests, no plaintext leakage or fallback, cancellation/deadline/drop races; review findings resolved. | Frozen verifier passed; independent source and directed-evidence review accepted |
| T3 | Lifecycle, boundedness and adversarial coverage | Tiny queues, multi-record fragmentation, slow peers, close_notify/bare EOF, TLS1.2/1.3, mixed HTTP/TLS work, memory/retained-capacity evidence, full verifier and review. | Native source/capacity/race proofs and Windows/Linux memory evidence accepted |
| T4 | Examples and independent consumers | Immediate-TLS and STARTTLS-style examples with bounded deadlines; docs/API/feature boundary and compatibility checks; examples build from packaged candidate. | Passed and independently reviewed at bede85c; no publication |
| T5 | Platform and live proof | Frozen source passes Windows x64/x86, Linux, Intel/ARM Macs on stable/MSRV as applicable, actual Wine separately, bounded owner-authorized live probes; final Astra review resolved. | Native and live Linux checks accepted. Actual Wine transport proofs pass, but platform-trust/public/live compatibility fails on Wine5 and remains open. Optional ARM memory observation is fixture-limited. |

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
| TR-01 | Freeze exact API, strict upgrade boundary and consumed-connection failure ownership. | Resolved: public and owner boundary/failure tests reviewed and green |
| TR-02 | Ciphertext progress cannot refund plaintext directly; bounded application-batch ledger and control records with zero credit. Define fixed extra reserve. | Resolved: capacity ledger, held credit and worker-ticket evidence accepted |
| TR-03 | Flush post-handshake control output without a user send; test KeyUpdate while application idle. | Resolved: authenticated full-record public and worker-restoration proofs |
| TR-04 | Distinguish TLS1.2 bidirectional peer closure from TLS1.3 directional closure; report truncation. | Resolved: directed pending-output/credit and public closure/truncation proofs |
| TR-05 | Existing TCP abort discards unread bytes; avoid falsely promising drain-before-error without retaining permits. | Contract resolved: existing abort-discard semantics; orderly close still drains |
| TR-06 | Empty standalone ALPN, unchanged HTTPS ALPN, separate TLS identity from endpoint. | Resolved: frozen public tests and HTTPS regressions passed |
| TR-07 | Preserve bounded shared workers, generation identity, deferred EOF, cancellation and deadlines. | Resolved: reviewed directed owner proofs and existing worker regressions |
| TF-01 | Fixture must observe a complete authenticated KeyUpdate response record; receiving a partial ciphertext record does not prove control progress. | Reviewer resolved after full framed-record consumption |
| TF-02 | Fixture large replies must stream within rustls output capacity. Distinguish raw EOF with unsent output from an actual partially transmitted TLS record. | Reviewer resolved: 16 KiB streaming and RawMidRecord mode |
| TF-03 | Hold the server transport open to verify client close_notify, TLS1.3 remaining read direction, and TLS1.2 peer-close reply. | Resolved: fixture/assertions reviewed; frozen execution passed |
| TF-04 | Controlled trailing plaintext/partial upgrade response and an owner gate for already-extracted cleartext events. | Resolved: public cases and deterministic extracted-event owner proof |
| TF-05 | Adversarial read/finish tests need explicit polling deadlines; a stalled read allowance can hide fixture EOF and defeat inactivity deadlines. | Reviewer resolved: bounded polling/manual tiny-window progress, no per-byte sleep or premature peer-event wait |
| TF-06 | Consuming-upgrade failure tests must prove permit release before shutdown; mid-record truncation test must require the specific Truncated category. | Resolved: reviewed assertions; frozen execution passed |
| TDOC-01 | Clarify that local reader remains usable after TLS1.3 local finish. | Reviewer resolved |
| TDOC-02 | Distinguish passive direct waiters from blocking helper mode checks. | Resolved: wording/API checks and frozen regressions accepted |
| SR-01 | TLS cancel/cancel_all snapshots can lose coverage when success moves pending TLS into live I/O between registry lock acquisitions. Commit cancellation under the original core lock, with cleanup/publication after unlocking. | Resolved: implementation rereviewed; registry race tests and frozen verifier passed |
| SR-02 | TLS terminal readiness must follow failed/cancelled upgrade I/O cleanup and permit release. Mirror canonical-commit / delivery-ready separation, including callback activation. | Resolved: implementation rereviewed; failed-upgrade cleanup/callback proofs passed |
| SR-03 | Hostname TLS admission takes a DNS permit but must also update borrowed-resolution metrics and high water. | Resolved: borrowed-resolution metrics regression passed |
| SR-04 | TlsOptions constructor must validate TLS identity, including rustls numeric-final-label rejection; DNS lookup validation alone is too permissive. Keep native and no-feature behavior consistent. | Resolved: native/minimal validation tests passed |

Reviewer accepted T0 after independently checking this contract and baseline evidence. Particular code-review gates: prove capacities and overlapping copies against the reserve; cancelled worker buffers retain globally bounded worker tickets through disposal; ordinary TLS1.2 peer close replies close_notify, while pending-output failure follows the refined abortive policy above; freeze cleartext I/O before another owner pump and reject late cleartext already extracted from a reactor batch.

| Backend finding | Requirement | State |
| --- | --- | --- |
| NR-01 (P1) | TLS1.2 pending-output peer close must abort without flushing further app ciphertext or splicing a later-sequence alert; already socket-drained batch credit must not cause a false pending-output failure. | Resolved: real encrypted-unsent abort/socket-close and sent/authenticated same-batch credit proofs accepted |
| NR-02 (P2) | A nonzero but nonrepresentable handshake timeout must be rejected at admission, not overflow to an absent deadline. | Resolved: Duration::MAX regression passed in frozen suite |
| NR-03 (P2) | Upgrade must clamp the reactor ciphertext queue to 18 KiB independently of the original plaintext send window. | Resolved: frozen capacity evidence accepted |
| NR-04 (P2) | Owner upgrade boundary recheck must catch cleartext FIN extracted before transition, not only released/aborted I/O. | Resolved: deterministic extracted cleartext FIN test accepted |
| NR-05 (P2) | Deferred handshake EOF followed by an incomplete worker result must fail promptly, not wait for timeout. | Resolved: already-observed PeerClosed then incomplete worker result proof accepted |
| NR-06 (P2) | Promote queued post-handshake controls after worker restoration without requiring another network event or user send. | Resolved: authenticated idle KeyUpdate restored from a real worker session; full response authenticated by server. This is not a literal coalesced Finished+KeyUpdate wire-flight test; see authority note below. |
| NR-07 (P2) | Authenticated read closure is terminal, distinct from backpressure; it must not resume read inactivity and abort a still-usable TLS1.3 writer. | Resolved: public frozen test proves EOF, quiet beyond read timeout, then protected send/finish |
| NR-08 (P2) | Prove actual buffer capacities and temporary merge/copy overlap fit the TLS reserve; length caps alone are insufficient. | Resolved: accepted caller Vec normalization; conservative ledger 248 KiB, 204 KiB during reactor growth; frozen capacity tests and evidence accepted |
| EX-01 | Example fixture must enforce its absolute lifetime after accept and interrupt an accepted socket before joining. | Reviewer resolved, including non-retryable ConnectionAborted on absolute expiry; examples rerun green |
| EX-02 | Explain that one-byte application reads do not prevent reactor prefetch; upgrade rechecks NBReq's queue. Make example TLS1.3 directional-close dependence explicit. | Reviewer resolved |
| PRB-01 | Hold probe must not expire its own read inactivity deadline while deliberately idle; recheck live connection/reservation counts after hold. | Resolved: Windows memory observer and execution reviewed; additional platform observations follow |
| MH-01 | Evidence-write failures must not skip owned child/thread/fixture cleanup. | Reviewer resolved: independent cleanup precedes artifact writes |
| MH-02 | Connection setup must not contaminate the idle baseline; closing must not contaminate the held phase. | Reviewer resolved: explicit connecting/closing markers and phase-order checks |
| MH-03 | Expected process-disappearance sampling races must not falsely fail successful completion or hide real sampler failures. | Reviewer resolved: ignore only after a fresh child-exit confirmation |
| PLAT-01 | A supervisor metadata-write failure after launch must still terminate its owned command; source integrity must be rechecked after nested Cargo commands. | Reviewer resolved; injected write-failure and timeout cleanup checks both reaped their child; end-of-gate manifest revalidation added |

The lab's `platform.py` launches bounded isolated host gates and records compiler, source manifest, exact commands and raw logs. Its memory-command deadline is 420 seconds, beyond the fixture-generation plus two observation deadlines, so outer timeout does not preempt normal owned-child cleanup. `collect.py` selects logs/JSON/text evidence while excluding builds and generated fixture certificates/keys. `snapshot.py` creates an explicit committed NBReq-only source whitelist, never the GDS workspace or build directories.

Existing shared-worker tests cover the two-thread/six-ticket bound, running/queued/completed ticket retention, cancelled input capacity through execution, queued cancellation and deadlines, and joined shutdown. Existing HTTPS lifecycle tests cover actual gated verifier saturation, HTTP/manual progress, cancellation and deferred certificate-flight FIN. These remain necessary regression evidence, but do not substitute for directed tests of the new standalone owner restoration path. Ten directed standalone owner tests and public callback/waiter/split coverage are now in the frozen passing suites.

Directed test authority notes: a physical FIN sent while certificate verification is held does not imply the reactor observed FIN while its read allowance is zero. The first such test's expectation was invalid; the replacement sequences an already-extracted `PeerClosed` event before worker restoration. Likewise, stock rustls refuses a server KeyUpdate before it receives the client's Finished, so it cannot construct the proposed literal Finished-plus-KeyUpdate flight through its ordinary server API. Reviewer approved a narrower proof: an authenticated idle KeyUpdate passes through a real worker session and normal standalone owner restoration, then its entire response authenticates at the server without an application write. Label that worker-restoration proof accurately; retain the separate public end-to-end idle-KeyUpdate test.

## Evidence

Permanent repository evidence: [archive](evidence/nbreq-tcp-tls-20260926.tar.gz) and [member/hash index](evidence/nbreq_tcp_tls_artifacts.json). The archive is854,907 bytes, contains349 evidence/source-provenance files (2,668,061 uncompressed bytes), and has SHA256 `73a3276d285b2cd37e2c72853f6376790eba157309280af0e00ed52309c7fcf0`. All archived member hashes were verified after packing. It excludes executables, build directories, generated keys, GDS source and credentials. The original local lab retains binaries and intermediate artifacts for further diagnosis.

Final independent review on2026-09-26 accepted native production `bede85c` and the documentation after repeated worker fix/re-review cycles. No implementation, capacity, lifecycle or test-authority finding remains open. This acceptance does **not** approve Wine5 deployment or assert every platform check passed. The separate Wine trust follow-up and unavailable ARM memory observation remain explicit. The final checkpoint after `bede85c` changes documentation and adds evidence only; no repeat of passing native suites is needed for that delta.

### Frozen implementation checkpoint

Production checkpoint: `bede85c32a41b7099c073e9585517d0ae4027342`. The preceding commits are `07858e3` (compiling skeleton and runtime reds), `017e87b` (implementation/test/docs), and `ff708e0` (formatting). The final commit contains only reviewed lint-equivalent changes. The 24-step Windows gate ran with a stable binary diff hash `581497c65b89af7b65e25139c83cdd322e2f1177` before/after; the committed delta has the same hash. The worktree was clean for packaging and remote snapshot creation.

The remote source archive contains 129 explicitly selected NBReq files, no GDS files or generated keys. Archive SHA256 `8be7f10827806779fa460370d5055103a8a57f24a7c8d2700f5d9e78122f4f48`; source-manifest SHA256 `7d476f855306661fe03cfc813a4f86dda972b4b79f897e2c5d9253e1f39d8bb5`. Successful platform runs verified source files before and after execution. ARM's later independent rerun also verified the original failed gate's source remained unchanged.

| Frozen evidence | Result and provenance |
| --- | --- |
| Windows x64 stable | Rust1.97.1; all24 verifier steps passed in134.522s. Raw logs in `C:/User/projects/nbreq/target/tls-test-checkpoint/frozen-working-581497c/`; exact source mapping in `final-evidence-bede85c.txt`. Reviewer independently checked the diff hash and all24 ordered PASS records. |
| Windows MSRV | Rust1.85 all-feature tests passed, including443 unit tests, public TLS20+3 and32 doctests. Pre/post source manifests identical, aggregate `d312c23e3c1cf19102dbe4f3ea38c98f42555778b2176fb35886d95185f34dd6`; their four changed-file hashes match final commit. Raw `windows-msrv/`. |
| Windows x86 | Full all-feature suite passed at ff708e0; final lint-equivalent bede85c rebuild and focused owner10/public23 passed. Final four Wine EXEs are PE I386; archiveSHA `e324feb8ba4aa966eb7479410327f9816aded196912ab9bffa5a36967f608a63`. Raw `windows-x86/`, with member/compiler manifests independently reviewed. |
| Packaged consumer | Clean bede85c packageSHA `258856bcbcc05b4de25d5f8817f9cbe4518d343abc0ffa8e832cff3ba3ebab31`,104 regular members. Both packaged C04/C05 examples echoed24 protected bytes; independent extracted-package consumer resolved and built with registry support crates. Raw `package-bede85c/`. This is an unreleased development package, not a replacement publication of0.2.0. |
| Linux | x86_64 Rust1.98 full24/24 in554.874s, Rust1.85 all-feature suite, probe strict lint/build and both examples passed. Whole gate including memory/live passed964.724s. Raw `linux-bede85c/`, archiveSHA `63b517966851e2edb78f4ba8dab344903df2ec7fc09900f948264ceca3cd89f6`. |
| Intel Mac | macOS15.7.9 x86_64 Rust1.98 full24/24 in300.168s, Rust1.85 all-feature suite, probe lint/build and examples passed. Whole gate519.111s. Raw `intel-bede85c/`, archiveSHA `60f735ce9af18d12d0af5082ef17ca4d740f9b9f32ecd2ce65b3c7c0a24bc6c1`. |
| Apple Silicon Mac | macOS26.6.1 arm64 Rust1.98 full24/24 in118.234s, Rust1.85 all-feature suite, probe lint/build and examples passed. The top-level gate correctly records failure in the separate Python/LibreSSL memory fixture. Native feature evidence accepted; no successful ARM memory observation claimed. Raw `arm-bede85c/`, archiveSHA `4a33c10f7c1949a18593dc0151ad0c25d231e11e103ba36fe1c87334893aa8e5`. |
| Live NBReq from Linux | `cavesvr3.caverock.com:993` verified IMAP greeting/CAPABILITY/LOGOUT; port25 verified STARTTLS followed by protected EHLO/QUIT. Native NBReq release probeSHA `f43abdad13da6594a7019f36c553d6c68c21affd7169b086e9b28e435fb33c98`. No authentication, mail submission or server changes. Probe does not expose negotiated TLS version; earlier OpenSSL TLS1.3 readiness is separate. |
| Linux client memory | Same frozen release probe, local verified TLS1.2 server, selected16KiB send/receive windows,16 and32 sessions. For32: median RSS3,960,832 ->5,115,904 ->5,328,896 bytes; private3,047,424 ->4,931,584 ->4,915,200 (baseline ->held ->released).98 held samples across5s; about0.02 CPU seconds during hold. Logical reservations9,437,184 ->0. Reviewer recomputed raw samples and phase/count/version assertions. Idle observation, not a RAM ceiling or performance comparison. |

The original package verification attempt under an ancestor Cargo workspace failed due to harness layout; the final attempt extracted into an isolated temporary root. An intermediate package run correctly failed its clean-source assertion when lint fixes landed during it. Both failed attempts remain distinct from the clean final package result. Initial formatting/strict-Clippy failures also remain preserved; final gates passed after worker fixes and reviewer confirmation.

### Compatibility investigations during final verification

**Wine5:** the first actual32-bit Wine public transport suite executed and exited101:19 cases failed during the existing Windows extra-root configuration, one rejected an untrusted certificate as `CertificateInvalid` rather than the expected `CertificateUnknownIssuer`. The three public API tests fail at the same extra-root setup. `platform_with_extra_roots` is unchanged from baseline6ada116. Independent Wine execution passed all10 directed standalone owner tests (explicitly trusted WebPKI test CA with cryptographic/identity checks), three registry cases and six worker cases. A mistyped I/O filter selected zero tests and is a harness failure, not a pass; the corrected full I/O regression module subsequently passed13/13. Both platform-trust live mail probes reached certificate verification and failed with `CertificateInvalid`.

The same independent I386 constructor diagnostic, with pinned dependencies and in-memory P256 CA, returned both default/extra-root constructors `Ok` on native Windows; under Wine, default construction returned `Ok` but additional-root construction returned `Err(General("Invalid parameter. (os error -2147024809)"))`. Its WebPKI root store accepted the CA. This isolates that setup failure to the platform-verifier/dependency boundary without involving NBReq transport. It does not prove the exact rejected Win32 parameter or explain the separate live platform-trust `CertificateInvalid`. Existing unchanged HTTPS additional-root tests in a final-source binary passed1 malformed-certificate rejection and failed3 Engine setup cases under Wine, versus4/4 on native Windows. These are not old-baseline binary executions. No verifier bypass, fallback or root-store mutation was introduced.

These results do not establish working end-to-end TLS under this Wine5 prefix. Raw `wine-bede85c/`, `wine-diagnostics/`, `wine-roots/`, and `wine-verifier-constructor/evidence/` retain all failures, native controls, compiler/PE hashes and observations. Final comparison archiveSHA `45e552f26a4eac84f91b83e8fce746dc0a65943c9071c4452c307464e9e9dd2f`. Follow [the Wine trust handoff](nbreq_tcp_tls_wine_followup.md) before making a compatibility claim.

**ARM memory fixture:** Python3.9.6 linked to LibreSSL2.8.3 fails its server handshake with `NO_SHARED_CIPHER`. A reviewed explicit P256 curve setting did not resolve it; the same source and client binary hashes were verified on both sides of that repeat. The unproven three-line change was reverted from tracked `memory.py`; its exact helper and failed evidence remain in the lab. First independent CLI diagnostic did not reach TLS because LibreSSL3.3.6 uses `-groups`, not the supplied `-curves`; it provides no handshake evidence. The corrected independent LibreSSL3.3.6 client also failed against the Python server: TLS1.2, ECDHE-ECDSA-AES128-GCM-SHA256, P256;100 bytes sent,7 received, alert40, no peer certificate, server `NO_SHARED_CIPHER`. Raw `arm-openssl-r2/`, archiveSHA `45f85e0ba582f02c11e816dbb851ab916aa1be1f1627ba5e591168454a348638`. This isolates an unusable optional fixture independently of NBReq, without identifying its internal LibreSSL cause. Reviewer accepted the fixture limitation; no successful ARM memory measurement is claimed. A future observation needs a portable Rust server fixture or validated newer Python/OpenSSL. Rust fixture tests already cover TLS1.2 and1.3 on ARM.

The reviewer accepted the original bounded platform command supervisor and found an additional detached-launch metadata-write cleanup gap in its future-use path. All original launches succeeded, so existing matrix evidence is unaffected. Workers fixed and fault-tested the local helper, retaining exact original `platform-bede85c.py` for matrix provenance. Wine and ARM diagnostic supervisors also passed review/fix/re-review for metadata cleanup, termination and honest failure reporting.

### Earlier development evidence

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
