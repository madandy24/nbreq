# WP7 Rust-native HTTP/1.1 evidence

Status: **WP7 accepted on Windows and Ubuntu 20.04.** Ordinary `Engine::new` still does not select
this backend.

## Dependency and boundary

The private `native` feature pins `httparse` 1.10.1. It is a focused MIT/Apache-2.0 response-head
and chunk-size parser, not a runtime or HTTP client. NBReq continues to own every socket, request
policy, byte buffer, framing transition, timeout, cancellation, and terminal result. Dependency
unsafe code remains outside NBReq, whose own `unsafe_code = "forbid"` policy is unchanged.

The proving backend is intentionally narrower than the eventual native backend:

- cleartext `http` only, with literal IPv4/IPv6 addresses until WP8 DNS lands;
- an ASCII origin-form request target without raw spaces; fragments are never transmitted;
- one request per connection, with `Connection: close` synthesized only when the caller supplied no
  Connection field;
- buffered bodies only; pooling, redirects, streaming, DNS, and TLS remain WP8/WP9;
- available only through the opt-in testing seam; Cargo feature selection does not silently change
  the ordinary consumer constructor.

## Request serialization

The serializer emits the caller method, origin-form target, HTTP/1.1, byte-valued headers, a Host
field when absent, exact Content-Length for a nonempty buffered body when absent, and no invented
Content-Type or Expect field. Generated fields count against configured header bytes/count before
the output allocation. Multiple Host fields, invalid or mismatched Content-Length, and request
Transfer-Encoding are rejected rather than creating request-smuggling ambiguity.

## Incremental response state machine

The decoder accepts arbitrary fragmentation and uses `httparse` only for response heads and chunk
sizes. NBReq implements:

- up to eight informational heads followed by one final head, with per-head byte/count limits;
- HEAD and 204/205/304 no-body rules;
- identical repeated Content-Length values, fixed-length bodies, and premature-EOF detection;
- one `chunked` transfer coding, extensions, strict CRLF, bounded framing metadata, trailers with
  forbidden framing fields rejected, and trailers deliberately not exposed as ordinary headers;
- rejection of Transfer-Encoding plus Content-Length and unsupported transfer codings;
- close-delimited completion only on peer FIN, followed by explicit slot release;
- response body/header limits checked before owned buffers grow past their configured bound.

Total time remains fixed from request acceptance. Connect time is removed after successful
`SO_ERROR`; inactivity begins at acceptance and resets only on actual connect, partial/final write,
or received-byte progress. The reactor now reports a due deadline without destroying the slot, so
the HTTP owner can reject a stale connect/inactivity event refreshed by useful I/O in the same poll.
The protocol owner then either reinstalls the next deadline or commits the matching Connect,
Inactivity, Total, or Unknown timeout and closes the slot.

A body-bearing exchange that resets before any response byte is conservatively classified Send.
Once response bytes begin, socket read failures classify Receive. Malformed recognition/framing is
Http, fixed-length premature EOF is Receive, and incomplete chunk framing is Http, matching the
accepted curl corpus.

## Platform proof

`cargo test --offline --no-default-features --features native,test-support` passes:

- 70 unit tests;
- 4 shared public adversarial tests;
- 4 public-contract tests;
- 2 compile-fail doctests.

The shared corpus proves byte-at-a-time fixed and chunked responses, extensions/trailers, identical
and conflicting lengths, Transfer-Encoding ambiguity, invalid status/header/chunk syntax, empty and
premature responses, and ten observed-progress 64 MiB upload resets classified Send. Connection
reuse is deliberately curl-only in this work package and remains WP9 native work.

Native-specific tests additionally prove:

- exact request serialization including binary header values and generated-field limits;
- every possible two-part split across valid fixed, chunked, and close-delimited messages and
  malformed length/chunk cases;
- a byte-at-a-time informational plus chunked request through the real spawned Engine path;
- a real close-delimited response completing through peer FIN;
- the same canonical response through a manually driven Engine;
- cancellation and network close at nine parser boundaries: before response, status, headers,
  fixed body, chunk size/data/terminator, and trailers;
- stalled-response Inactivity and Total classification under 500 ms;
- useful response bytes refreshing inactivity until a longer response completes.

The dependency-free default remains 41 unit, 4 contract, and 2 doctests. Strict native/test-support
and all-feature clippy with warnings denied pass; formatting and the all-feature compile pass.
Current Windows Schannel cannot acquire credentials for registry access or the three pre-existing
curl TLS fixtures, so dependency resolution was performed from Cargo's already verified local
cache and no new curl execution claim is made. No curl source changed.

The exact corrective source archive for commit `cc96305`, SHA-256
`70BCAFA3AFE3701991A19E49C55972A2D1D5226B5E73869C154128126E31B95B`, was copied to
`gds-srv-test2`, Ubuntu 20.04.6 x86-64, and run with Rust/Cargo/Clippy 1.85.0. A fresh extracted
tree passes the same 70 unit, 4 shared adversarial, 4 public-contract, and 2 compile-fail doctests.
Strict native/test-support clippy with warnings denied and formatting both pass.

The first exact Ubuntu run usefully exposed a real event-batch ordering race: Linux could report
write progress and then a read-side reset in one reactor pass, after the reactor had already
removed the socket. The HTTP owner updated useful-progress state but then tried to re-arm the stale
slot, producing Internal instead of the portable Send result. Commit `81d715e` records terminal
failure slots before processing the batch, preserves their progress semantics, and suppresses only
the invalid deadline re-arm. A deterministic same-batch regression now guards that contract.

The subsequent stress run exposed a laboratory problem rather than a transport defect. On the
small Ubuntu host, repeated 64 MiB allocation/serialization could take longer than the fixture's
two-second accepted-socket read timeout. The server then reset before observing the request head,
so Connect was correct and the fixture's Send expectation was false. Commit `cc96305` gives only
that large-upload precondition a ten-second observation window; the request's five-second total
deadline remains unchanged, and the server still resets only after seeing the complete head plus
body progress.

The final exact tree then passes 25 consecutive runs of all 16 native HTTP module tests and 20
consecutive runs of the ten-trial 64 MiB upload-reset case: 200 observed-progress abortive uploads
with no failure. No NBReq or adversarial-test process remained after the soak.

## Acceptance and later boundaries

The conservative ASCII origin-form target, buffered-request Transfer-Encoding rejection, bounded
informational count, validated-but-unexposed trailers, and reset-stage policy are accepted for this
private cleartext proving backend. They are not yet a consumer-stable native policy freeze; DNS,
TLS, redirects, reuse, and streaming can supply evidence that deliberately revises them.

Every two-part split and the shared adversarial corpus are property coverage, not a fuzzer. A real
seeded fuzz campaign remains a WP8/WP9 hardening item. Bytes arriving after a complete response in
one decoder ingest are currently discarded; that is harmless for the one-request,
`Connection: close` slice and must be revisited before WP9 connection reuse. No Wine native-HTTP
claim is made.

WP7 is accepted. Deterministic refused-connect classification remains with WP8's resolver/connect
laboratory. WP8 may now add DNS and TLS without moving socket ownership, cancellation, framing, or
the accepted cleartext deadline semantics out of this foundation.
