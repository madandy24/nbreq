# nbreq-darwin

Implementation-detail macOS support for [NBReq](https://github.com/madandy24/nbreq).

This crate reads a deliberately bounded view of Apple's System Configuration dynamic store,
rejects supplemental `/etc/resolver` routing that NBReq cannot yet represent, and returns owned
Rust values to NBReq. It is published only so the main `nbreq` crate can preserve
`unsafe_code = "forbid"`; it is not a standalone public API.
