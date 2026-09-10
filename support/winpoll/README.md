# nbreq-winpoll

`nbreq-winpoll` is an implementation-detail support crate for NBReq's Windows socket readiness
and DNS adapter discovery boundary. It is published only so the `nbreq` crate can resolve its safe
wrapper from crates.io. Its API is not a separately supported consumer interface; depend on
`nbreq` instead.

NBReq proper forbids unsafe code. WinSock and IP Helper FFI are isolated here and exposed through
safe crate interfaces. DNS discovery returns owned interface identifiers, status, family metrics,
interface indices and DNS endpoints. It validates required fields using bounded byte slices and
never follows unused descriptive UTF-16 strings, including odd-addressed strings from old Wine.
The caller retains DNS server ranking, search-suffix and query policy.

Version 0.1.1 adds the DNS interface; existing polling behavior and API are unchanged. Both x86
and x64 Windows builds are covered. Wine compatibility observations require the actual Wine
version and any adjacent runtime shims to be recorded separately from native Windows tests.

Licensed under either Apache-2.0 or MIT, at your option.
