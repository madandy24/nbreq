# WP4 adversarial HTTP laboratory evidence

Status: curl-pilot protocol/error slice proved on Windows 10 and against Ubuntu 20.04 system curl
on 2026-08-17. The Wine rerun and the later native-backend corpus remain open; this document does
not close all of WP4.

## Public test boundary

`tests/http_adversarial.rs` drives only NBReq's public `Engine`, `Client`, `Request`, `Response`,
`ExecuteError`, and portable error-detail types. It does not construct a curl handle or match a curl
error number. The same requests and assertions are intended to move to the native backend without
changing their consumer-visible contract.

The local scripted server supports:

- byte-at-a-time response output;
- valid fixed-length and chunked responses, including a chunk extension and trailer;
- invalid status lines, header names, header values, conflicting content lengths,
  transfer-encoding/content-length ambiguity, and chunk sizes;
- fixed-length and chunked premature EOF plus a response that closes without sending any bytes;
- an abortive close while a large buffered POST is still transmitting; and
- two sequential HTTP/1.1 responses on one accepted keep-alive connection.

All fixtures use loopback sockets, finite deadlines, and protocol/socket events rather than remote
services. The upload-abort case uses an abortive close and a 64 MiB configured body so Windows
cannot normally finish queueing the body before the reset; ten trials run in one test. NBReq also
uses curl's observed uploaded-byte count to classify a receive-coded reset as `Send` when the
buffered request body was not fully transmitted.

## Portable mappings now proved

| Controlled condition | Public result |
|---|---|
| Valid byte-fragmented fixed-length response | completed `Response` |
| Valid chunk extension and trailer | completed body; trailer representation remains unspecified |
| Invalid status/header/content-length/chunk syntax | `Transport` / `TransportStage::Http` |
| Incomplete chunked framing | `Transport` / `TransportStage::Http` |
| Short fixed-length body | `Transport` / `TransportStage::Receive` |
| Empty response | `Transport` / `TransportStage::Receive` |
| Reset before the buffered body is fully uploaded | `Transport` / `TransportStage::Send` |
| Two sequential requests on one accepted HTTP/1.1 socket | both complete, proving pilot reuse |

Response header names and values now receive the same backend-neutral token/control-byte checks as
request headers. NBReq also rejects differing repeated content lengths and any response carrying
both transfer encoding and content length while permitting repeated identical lengths; this avoids
the different curl 7.68/8.21 classifications observed by the Ubuntu/Windows corpus. The pinned curl
binding exposes its missing named predicate for libcurl's stable
`CURLE_WEIRD_SERVER_REPLY` code; the underlying `curl-sys` constant retains its historical
FTP-prefixed name. Curl codes remain private diagnostics and do not enter public assertions.

## Existing laboratory coverage retained

`src/curl_tests.rs` already provides the staged barriers and generated fixtures for slow headers,
stalled response bodies, TLS ClientHello/handshake cancellation, response limits, informational
heads, redirects, inactivity/total deadlines, callback versus blocking parity, wakeup, shutdown,
and runtime capability recording. WP1's deterministic in-memory suite retains queue, terminal-race,
callback pressure, reentrancy, cancel-all, and callback-domain shutdown coverage.

## Current run

On the Windows development host:

- the default build passes 41 unit, 4 public-contract, and 2 doctests;
- the ordinary dynamic curl-pilot build passes 58 unit, 5 public adversarial, 4 public-contract,
  and 2 doctests;
- the all-features/vendored build passes 55 non-TLS unit, 5 public adversarial, 4 public-contract,
  and 2 doctests under the restricted execution token;
- the three all-features Schannel fixtures fail under that restricted token before ClientHello, the
  already-recorded environment limitation whose identical binary must be run under a normal
  Windows account; and
- all-target/all-feature clippy with warnings denied, formatting, and diff whitespace checks pass.

On updated Ubuntu 20.04 using Rust 1.85 and dynamic system libcurl 7.68.0/OpenSSL 1.1.1f:

- the full curl-pilot build passes 59 unit, 5 public adversarial, 4 public-contract, and 2 doctests;
- all-target curl-pilot clippy with warnings denied and formatting checks pass;
- the runtime capability probe records asynchronous DNS and IPv6 support without a vendored curl;
  and
- slow-header, stalled-body, and TLS-handshake cancellation maxima are approximately 0.297 ms,
  0.300 ms, and 1.684 ms respectively, all comfortably inside the provisional 100 ms gate.

The first Ubuntu run exposed a real portability difference: curl 7.68 classified conflicting
response framing differently from curl 8.21. NBReq now validates repeated Content-Length and
Transfer-Encoding/Content-Length ambiguity itself, and the clean revised run above passes. This is
exactly the backend-neutral normalization the shared laboratory is intended to force.

The revised self-verifying bundle for commit `072c9d0` was then extracted into a clean directory and
run under an ordinary account on Windows 10 Pro 22H2 x64 build 19045.7663:

- every packaged hash passed, including the separately packaged adversarial executable and pinned
  curl 8.21.0 Schannel DLL;
- 58 unit, 5 public adversarial, and 4 public-contract tests passed;
- the generated TLS policy/no-verify fixtures passed;
- slow-header, stalled-body, and TLS-handshake cancellation maxima were 1.8914 ms, 1.6895 ms, and
  2.2833 ms respectively; and
- all 25 fresh-process DLL load/use/exit iterations passed.

The transcript is `target/curl-pilot/win10-proof-1112.txt`, SHA-256
`7DE1360BA6BBF8CC1215D54997074C6D43B79B20E9C330089E3D06FDF10552C5`. The upload case performs ten
reset trials. Only the revised Wine-5 rerun remains in the curl-pilot platform pass.

## Deliberate remainder

- Curl DNS and connect cancellation remain finite-deadline pilot limitations. Deterministic resolver
  and connect-stage control belongs to the native reactor work.
- Trailers are accepted by the pilot but have no portable public representation yet; callers must
  not rely on them appearing in `Response::headers()`.
- Parser property tests and fuzz targets begin with the native parser in WP7.
- The native backend must run this corpus unchanged on Windows and Linux and add its raw parser,
  resolver, connect, pooling-pressure, and ambiguous-close cases.
- GDS integration is separately review-gated in `thoughts/gds_curl_pilot_integration_plan.md`.
