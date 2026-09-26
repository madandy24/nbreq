# Wine TLS trust diagnostic

Unpublished, standalone Windows diagnostic. It compares rustls-platform-verifier
0.7.0 with explicit Windows CryptoAPI calls. It does not change NBReq or install
certificates into persistent stores. Generated P-256 CA/leaf keys and stores stay
in memory. Normal CryptoAPI chain retrieval and caching may occur: the chain flags
match the dependency, including revocation checks and a 10-second URL timeout.
Run each process with an external 90-second deadline and retain stdout/stderr.

Build the 32-bit Windows target from this nested workspace:

```text
cargo build --release --manifest-path tools/wine-tls-probe/Cargo.toml --locked --target i686-pc-windows-msvc
```

No arguments runs generated certificate controls. Public-chain mode reads one DER
leaf and up to eight ordered DER intermediates (each at most 64 KiB):

```text
nbreq-wine-tls-probe.exe --leaf leaf.der --intermediate intermediate1.der --intermediate intermediate2.der --name cavesvr3.caverock.com --captured-at 1790385309 --at-unix 1790385309
```

The opt-in command `nbreq-wine-tls-probe.exe --compact-ca-key-usage` retains the
same critical CA keyCertSign+cRLSign permissions but encodes their BIT STRING
without an unused trailing byte. It correctly signs a new CA/leaf pair; keys stay
in memory. No arguments retains the original rcgen encoding. Separate executions
use fresh keys/certificates, so compare decoded permissions and API behavior, not
certificate hashes across these two generated runs.

This compact variant also creates signed empty and revoked CRLs. Each CRL case
uses fresh memory stores and a fresh 48-byte chain engine to separate cached
results. The same generated leaf has serial 42; the revoked CRL lists it with
revocation time one hour before evaluation. CRLs span yesterday through 90 days
ahead, covering the leaf's expiry negative as well. Output records CRL hashes,
issuer root hash, serial, and thisUpdate/nextUpdate/revocation time. The empty CRL
case requires a valid positive, wrong-name rejection, and expiry rejection. The
revoked case must reject, ideally with CERT_E_REVOKED or CRYPT_E_REVOKED. The
untrusted case supplies the same issuer and empty CRL only in the additional
store while using a fresh default-root engine; it must still reject that CA.
These are raw CryptoAPI controls; the dependency verifier is unchanged and is
not supplied with an extra CRL through a new API.

`--captured-at` is required provenance text; it does not select validation time.
Optional `--at-unix` selects integral Unix seconds from 0 through 6,802,270,473,
bounded by the pinned verifier's intermediate timestamp arithmetic;
otherwise validation uses current time. Output records both evaluation time and
observed current time. The same evaluation time is passed to dependency and raw
CryptoAPI paths. Inputs are offline certificate files, but API chain retrieval may
use the network. The probe does not contact the mail server or authenticate.

Stdout is JSON Lines. The input record retains certificate order/fingerprints,
name, times, pointer width, structure sizes and field offsets. API records retain
constructor sizes, flags, BOOL results and immediate `GetLastError` on failure.
Chain trust bits and SSL policy error/chain/element indices are separate evidence.
Per-element output includes bounded certificate extension hashes and short DER
encodings for key usage/basic constraints/EKU. Missing `pRevocationInfo` is emitted
as null, meaning unavailable details, never evidence that checks did not run.
Public-chain mode additionally invokes SSL policy with flags zero and 0xF00 on
the same built chain, retaining all raw trust bits. No SECURITY_FLAG_IGNORE_*
flag is added, and no chain trust status is cleared or changed.
Successful policy API invocation alone does not mean validation succeeded;
`dw_error == 0` is acceptance. A `completed` event/exit zero only means the
diagnostic ran; it is never a claim that Wine TLS trust works. Known-invalid
acceptance emits `fatal` and exits nonzero. Other validation/constructor failures
remain diagnostic results for inspection, including unavailable positive controls.

The generated matrix compares default trust and exclusive-root engines, with
exact name, wrong name, and the same leaf one day after its explicit expiry.
Generated default trust must reject the fresh private CA. Dependency verification
also covers default exact/wrong name and extra-root exact/wrong name/expiry when
construction succeeds. Hostname and expiry controls are interpretable only after
the matching exact-name, valid-time positive control passes. Raw trust bits may
include revocation-unknown errors that the dependency's SSL policy intentionally
ignores; the positive control therefore uses final policy acceptance. Signature,
root trust, hostname and validity enforcement are retained.

For x86, engine sizes are derived from field offsets: current 52 bytes, pre-Win8
48 bytes, and 40 bytes omitting exclusive-root fields. The 40-byte case is strictly
a constructor control, never proof that an extra root was used. Each engine size
also has a null-root constructor control. Chain parameters compare current 52
bytes and pre-strong-sign 44 bytes. Public-chain mode compares default trust and
wrong hostname at both chain-parameter sizes; it does not invent a private root
or an expiry time for an unparsed external certificate.

Generated P-256 failures must not be generalized to an independently captured
RSA public chain. Retain the public capture command, timestamp, ordered DER hashes
and independent hostname verification alongside probe output. Native Windows is
the positive reference; compare its actual JSON results with Wine results, not
exit codes alone. An app-local compatibility DLL, if used by the runner, must be
identified separately and must not be installed into the shared Wine prefix.

## Source basis

The local dependency source is
`rustls-platform-verifier-0.7.0/src/verification/windows.rs`, SHA-256
`6f861a61ccd4624d50ebd055bc316414499f8611d5eb0794978e0cac8554bc78`.
Default constructor success does not verify a server certificate. The extra-root
constructor opens a memory store, adds DER roots, then creates a chain engine with
`hExclusiveRoot` and the current structure size.

[Wine 5.0 chain.c](https://raw.githubusercontent.com/wine-mirror/wine/wine-5.0/dlls/crypt32/chain.c)
SHA-256 `fd7e9db8bf3002e5518e6775454c391ef63c3f2ca858344d92960b5a30d5c5f0`,
lines 232–236, accepts only the sizes of its old full engine configuration or
`CERT_CHAIN_ENGINE_CONFIG_NO_EXCLUSIVE_ROOT`; other sizes set `E_INVALIDARG`.
[Wine 5.0 wincrypt.h](https://raw.githubusercontent.com/wine-mirror/wine/wine-5.0/include/wincrypt.h)
SHA-256 `fbba1a6b60457245b3f02999798fa9741ad630048dc9859b8408a0df42d3b134`,
lines 3370–3384, ends the engine configuration at `hExclusiveRootTrustedPeople`,
without `dwExclusiveFlags`. Thus the 32-bit Wine 5.0 source accepts 40/48-byte
configurations while the modern Windows declaration is 52 bytes. The diagnostic
tests this source-derived hypothesis without changing the verifier dependency.

[Microsoft's structure documentation](https://learn.microsoft.com/en-us/windows/win32/api/wincrypt/ns-wincrypt-cert_chain_engine_config)
identifies the Windows 8 `dwExclusiveFlags` addition.
[Wine MR 10347 discussion](https://list.winehq.org/hyperkitty/list/wine-gitlab@list.winehq.org/thread/ZLFHCJKI2UJEH2PX7P5RDWPRBUAOIQS5/)
tracks updating that structure. The source-size hypothesis concerns construction;
it does not explain a later default-trust SSL policy rejection by itself.

API interpretation follows the primary documentation for
[CertGetCertificateChain](https://learn.microsoft.com/en-us/windows/win32/api/wincrypt/nf-wincrypt-certgetcertificatechain)
and [CertVerifyCertificateChainPolicy](https://learn.microsoft.com/en-us/windows/win32/api/wincrypt/nf-wincrypt-certverifycertificatechainpolicy).

Wine 5.0 `chain.c` lines 3453–3545 reads SSL callback `fdwChecks`, but does not
apply `CERT_CHAIN_POLICY_PARA.dwFlags`; offline revocation therefore fails before
hostname checking despite the dependency's 0xF00 unknown-revocation policy flags.
The broad SSL callback revocation-ignore flag also suppresses revoked status and
is deliberately absent from this probe. `CRYPT_KeyUsageValid`, lines 1723–1800,
reads the last decoded key-usage byte. rcgen 0.13.2 writes nine bits (CA content
`0303070600`), while the compact equivalent is `03020106`; selected extension
output exposes that difference without removing key-usage enforcement.

[Wine 5.0 cryptnet_main.c](https://raw.githubusercontent.com/wine-mirror/wine/wine-5.0/dlls/cryptnet/cryptnet_main.c)
SHA-256 `ba5d3267fcb5e95e24cf0cbc9d815e351581ae186951fc47f57219b75238798a`,
lines 1728–1783, can use the additional store's CRL when the issuer has cRLSign,
checking issuer signature/AKI before revocation. This motivates the in-memory
strict-positive attempt. The CRL controls are evidence only if the actual
positive and corresponding rejection results support them; constructor success
or unavailable revocation details are insufficient.
