//! External release consumer; no private nbreq modules or GDS dependency.
#[cfg(all(test, feature = "native"))]
mod common_http;
#[cfg(all(test, feature = "native"))]
mod fixture;
#[cfg(all(test, feature = "v020"))]
mod v020;
#[cfg(all(test, feature = "native", feature = "v020"))]
mod v021;
#[cfg(all(test, feature = "v020"))]
mod v030;
