# NBReq

NBReq is a planned Rust HTTP client for programs that need concurrent network access, prompt cancellation, deterministic shutdown, and synchronous or callback-oriented APIs without adopting an async runtime.

The architecture contract, WP0 crate skeleton, and WP1 backend-independent lifecycle kernel are complete. The next transport work uses libcurl Multi as a private proving backend before progressing toward the Rust-native HTTP/1.1 engine.

## Project documents

- [Initial product specification](thoughts/nbreq_initial_spec.md)
- [Delivery plan](thoughts/project_nbreq_plan.html)
- [DPWebRPC plan sample](thoughts/project_dpwebrpc_sample.html)

The crate currently accepts and coordinates requests through a deterministic non-networking scaffold backend. It deliberately does not perform HTTP yet.
