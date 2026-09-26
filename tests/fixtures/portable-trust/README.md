# Offline public certificate chain

These three DER files contain only public certificates, captured from the owner's
`cavesvr3.caverock.com:993` endpoint on 2026-09-26 during the authorized Wine TLS
investigation. The verified OpenSSL capture returned verification code 0. No
credentials, private keys, application traffic, TLS sessions, or root-store edits
are included. Tests never contact this endpoint.

The leaf is `cavesvr3.caverock.com`, issued by Let's Encrypt YR1; files 1 and 2
are the peer-supplied intermediate chain. The test pins verification time to Unix
`1790385456`, within the captured leaf's validity, so normal expiry does not make
this historical trust-source regression test fail. This proves certificate-chain
verification only; generated loopback integration tests separately prove actual
TLS possession of a server private key.

SHA-256:

| File | Digest |
| --- | --- |
| `cavesvr3-chain-0.der` | `3f70df7657880a95ceed2089b7a79844c1ca2df239921742965a0fc52a533623` |
| `cavesvr3-chain-1.der` | `13949634d99cd6fd6aa80bc034fefacceb1969feef986586713ecdbb05758d3f` |
| `cavesvr3-chain-2.der` | `072639d0b140d5bffae16ad9c3f6cc6086040621f51ee61a6d46a8915c07cf76` |

The original local investigation evidence is in
`target/wine-tls-trust-20260926/public-chain-capture-receipt.json`; it is not a
runtime or packaging dependency of these tests.
The retained investigation index is
`thoughts/evidence/nbreq_wine_tls_trust_artifacts.json` in the source repository
only; that investigation archive is intentionally excluded from the published crate.
