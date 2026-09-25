# Standalone TCP TLS on Wine: follow-up

The frozen production commit `bede85c32a41b7099c073e9585517d0ae4027342`
was exercised under Wine 5.0 (Ubuntu 5.0-3ubuntu1) with the existing private
Win32 prefix at `/tmp/nbreq-wine-dns-lab-20260910/prefix`. The run used an
app-local ProcessPrng shim with SHA-256
`92d437dc538ef6ddfae7fc0b2140ec407a6dacfb0c899ec67a6812e84387de86`.
Raw command, host, result and test logs are in
`C:/User/projects/nbreq/target/tcp-tls-20260926/wine-bede85c/` and
`C:/User/projects/nbreq/target/tcp-tls-20260926/wine-diagnostics/`. The
independent constructor, unchanged HTTPS-roots comparison and corrected I/O
filter are in `C:/User/projects/nbreq/target/tcp-tls-20260926/wine-roots/`.

The public TLS suite failed 20/20 under this Wine environment. Nineteen cases
stopped while configuring additional trust roots; the remaining case returned
`CertificateInvalid` for an untrusted certificate where the test expected
`CertificateUnknownIssuer`. All three public API cases also stopped at the
additional-root setup. Both live default-trust mail probes reached TLS
verification and returned `CertificateInvalid`. The
`platform_with_extra_roots` constructor is unchanged from baseline `6ada116`. An
independent i686 constructor check returned `Ok` for both default and extra
roots on native Windows. Under this Wine prefix, default trust returned `Ok`,
while extra roots returned
`Err(General("Invalid parameter. (os error -2147024809)"))` at the
platform-verifier dependency boundary; a separate WebPKI root store accepted
the same CA. The unchanged HTTPS additional-roots suite passed its malformed
certificate rejection case and failed its other three cases at Engine
additional-root configuration. This narrows the extra-root setup failure but
does not identify which Win32 parameter or Wine behavior caused it, nor
explain the separate default-trust live `CertificateInvalid` results.

Ten standalone-owner tests passed under Wine using a verified WebPKI test CA.
Three registry and six worker tests separately passed lifecycle and accounting
checks. These results do not establish working platform trust or full public
TLS support in this Wine 5.0 prefix. The first I/O filter selected zero tests
and provides no passing evidence; a corrected filter subsequently passed 13
I/O tests. Native-platform source acceptance is tracked separately; this Wine
result is also separate from the earlier fixed DNS misalignment issue.

Before claiming Wine compatibility, rerun the public and live checks under
the intended target Wine release or a newer controlled Wine environment and
compare the platform-trust behavior. Do not add a verification bypass,
plaintext fallback, `catch_unwind` workaround, or mutation of system trust or
the shared private prefix based on these observations.
