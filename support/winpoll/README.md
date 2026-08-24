# nbreq-winpoll

`nbreq-winpoll` is an implementation-detail support crate for NBReq's Windows socket readiness
compatibility boundary. It is published only so the `nbreq` crate can resolve its audited safe
wrapper from crates.io. Its API is not a separately supported consumer interface; depend on
`nbreq` instead.

NBReq proper forbids unsafe code. The minimal WinSock FFI required by this wrapper is isolated
here and exposed to NBReq through a safe crate interface.

Licensed under either Apache-2.0 or MIT, at your option.
