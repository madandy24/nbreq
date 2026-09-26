# TCP TLS smoke probe

This standalone crate defaults to NBReq's platform verifier. Supply the destination explicitly; it has no built-in remote host, credentials, or mail-sending commands.

```text
cargo run --manifest-path tools/tcp-tls-probe/Cargo.toml -- imaps HOST 993
cargo run --manifest-path tools/tcp-tls-probe/Cargo.toml -- smtp HOST 25
```

To select portable Mozilla trust explicitly, enable the optional tool feature and pass
the trailing flag. Merely compiling the feature leaves platform trust as the default:

```text
cargo run --manifest-path tools/tcp-tls-probe/Cargo.toml --features bundled-roots -- imaps HOST 993 --bundled-roots
cargo run --manifest-path tools/tcp-tls-probe/Cargo.toml --features bundled-roots -- smtp HOST 25 --bundled-roots
```

The flag fails before connecting if the feature is absent. Portable verification checks
certificate signatures, validity and hostname using the bundle; it does not import OS
trust/distrust or revocation policy. Update `webpki-roots` in the application dependency
graph and rebuild/redeploy to update the bundle. There is no automatic fallback.
The `hold` command retains its original platform-plus-private-root behavior and does
not accept this flag.

`imaps` verifies the TLS certificate, reads the greeting, then sends only `CAPABILITY` and `LOGOUT`. `smtp` reads the greeting, sends `EHLO` and `STARTTLS`, verifies the upgraded connection, then sends a protected `EHLO` and `QUIT`. Each protocol line and reply count is capped. Connect, handshake, idle-read, and whole-probe deadlines are finite; a process supervisor should still cap the command itself.

For client-only memory sampling with a separate local TLS server, provide a literal socket address, a certificate identity, and a DER-encoded private CA root:

```text
cargo run --manifest-path tools/tcp-tls-probe/Cargo.toml -- hold 127.0.0.1:PORT 127.0.0.1 ROOT.der 16
```

The final argument is `16` or `32`. The probe prints `baseline`, `ready`, and `released` phases. It retains verified connections for five seconds with 16 KiB send and receive windows, checks the logical TLS reservation, then drops all connections and checks permit reclamation. It does not assert an RSS threshold; measure that externally while the local server runs in another process.

For a bounded client-only memory observation, build the probe and the separate `tools/tcp-tls-fixture` crate first, then run:

```text
python tools/tcp-tls-probe/memory.py --binary PATH_TO_PROBE --fixture-binary PATH_TO_TCP_TLS_FIXTURE --out NEW_OUTPUT_DIRECTORY --tls-version 1.3
```

The observer uses Python 3.8 or newer and samples only the probe PID. The Rust fixture creates its CA and leaf private keys in memory, writes only the public DER root, binds an ephemeral IPv4 loopback port, and serves exactly 16 or 32 verified TLS connections per run. Select `--tls-version 1.2` for the other explicit protocol. The observer checks the fixture's owned PID, loopback address, counts and negotiated versions, retains raw logs and process samples, and reaps both children before publishing success. It imposes no RSS pass/fail threshold; the samples are whole-client-process observations rather than per-connection allocation accounting.
