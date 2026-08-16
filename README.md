# NBReq

NBReq is a planned Rust HTTP client for programs that need concurrent network access, prompt cancellation, deterministic shutdown, and synchronous or callback-oriented APIs without adopting an async runtime.

The architecture contract, WP0 crate skeleton, and WP1 backend-independent lifecycle kernel are complete. WP2 now has a hardened private libcurl Multi transport vertical slice and a pinned Windows Schannel DLL build; cross-platform, DNS/connect, TLS-fixture, and exact GDS packaging gates remain before WP2 closes.

## Project documents

- [Initial product specification](thoughts/nbreq_initial_spec.md)
- [Delivery plan](thoughts/project_nbreq_plan.html)
- [WP2 curl pilot evidence](thoughts/wp2_curl_pilot_evidence.md)
- [DPWebRPC plan sample](thoughts/project_dpwebrpc_sample.html)

The ordinary public constructor still uses the deterministic non-networking scaffold. The curl backend is deliberately exposed only through the private `curl-pilot` feature plus opt-in test support while its proof work is unfinished; experimental backend types do not enter the public API.
