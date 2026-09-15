# NBReq 0.2.0 publication

2026-09-15. The owner asked to continue after the next steps were stated as helper publication,
registry-only checks and nbreq publication. The Intel Mac observation remains recorded; the
owner does not consider it a blocker given the dedicated-host evidence. Host overload is not
asserted as its proven cause.

## Resume checkpoint

| Item | State |
| --- | --- |
| Source | Isolated clean checkout `target/worktrees/nbreq-publish-020` at `432509b8d2d198cb0139def322f2cc82cd4a76c6`, the frozen package source. Later main commits change excluded records/tools only. |
| Darwin 0.1.0 | Published 2026-09-15 08:17:12 UTC. API/index and downloaded archive verified against frozen SHA256 `31d7a69844b990331f85dc497f14d0b6a08f8ceaee402e7c6aa19919f3e3c839`. |
| Winpoll 0.1.1 | Published 2026-09-15 08:17:49 UTC. API/index and downloaded archive verified against frozen SHA256 `9e4fc64245514209beb6e3feccb4b980e1c259546a9e07543e3529e1d7053696`. |
| Root candidate | Publication dry run with no helper overrides passed. 95 files / 343,615 bytes, SHA256 `05d2504522350118ef702fb3aa01288bd5939da62dc5e1727807fd03813686ed`. Compared with the earlier archive, only helper registry sources/checksums in Cargo.lock and README CRLF checkout normalization differ. All dependency versions, source and README text are unchanged. |
| Registry proof | Fresh candidate consumers with registry helpers pass all 36 local stages on stable/Rust 1.85, including four independent graphs, 116 tests, eight negative feature probes and all 16 example cases. A manual read-only workflow covers both toolchains on Windows/Linux/Intel Mac/ARM Mac; fully registry-only mode follows root publication. |
| Remaining | Finish candidate registry checks, publish root, verify the downloaded/indexed root hash and fully registry-resolved consumers, then inspect crates.io README/docs.rs and update release records. |
| Evidence | Local raw directory `target/release-publication-20260915`: helper dry runs, registry API/index receipts, downloaded archives, root package comparison and consumer logs/locks. Cargo credentials were used through Cargo; no credential values are included. |

Dry runs used the normal Cargo publication path. The first controller looked for the Darwin
archive directly under `package/`; Cargo placed publication archives under `tmp-crate/` and
`tmp-registry/`. The successful dry run was reused and both staged archives were verified.
Root comparison separately identified and verified the harmless README line-ending difference;
it was not treated as a source change or silently omitted from the new package identity.

The first registry workflow (`34947171514`, source `6cfdf0a`) passed both Ubuntu jobs and all
eight package-verification stages, but its new extraction guard rejected six Windows/Mac temp
paths before consumer tests. It compared resolved destinations with an unresolved temp root
(Mac temp aliases and Windows short/long paths). Canonicalize the freshly created root too;
retain the containment check and both earlier path-component guards. This is a verification
runner correction, with no library/package change. The failed run and its artifacts are retained.
