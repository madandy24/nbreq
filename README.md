# NBReq

NBReq is a planned Rust HTTP client for programs that need concurrent network access, prompt cancellation, deterministic shutdown, and synchronous or callback-oriented APIs without adopting an async runtime.

The architecture contract and WP0 crate skeleton are complete. WP1 will implement the backend-independent request registry and lifecycle kernel. The first transport work will use libcurl Multi as a private proving backend before progressing toward the Rust-native HTTP/1.1 engine.

## Project documents

- [Initial product specification](thoughts/nbreq_initial_spec.md)
- [Delivery plan](thoughts/project_nbreq_plan.html)
- [DPWebRPC plan sample](thoughts/project_dpwebrpc_sample.html)

The crate currently contains only the public boundary and a private non-networking scaffold backend. It deliberately does not perform HTTP yet.
