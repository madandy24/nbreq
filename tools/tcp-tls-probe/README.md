# TCP TLS smoke probe

This standalone crate uses NBReq's system-root verifier. Supply the destination explicitly; it has no built-in remote host, credentials, or mail-sending commands.

```text
cargo run --manifest-path tools/tcp-tls-probe/Cargo.toml -- imaps HOST 993
cargo run --manifest-path tools/tcp-tls-probe/Cargo.toml -- smtp HOST 25
```

`imaps` verifies the TLS certificate, reads the greeting, then sends only `CAPABILITY` and `LOGOUT`. `smtp` reads the greeting, sends `EHLO` and `STARTTLS`, verifies the upgraded connection, then sends a protected `EHLO` and `QUIT`. Each protocol line and reply count is capped. Connect, handshake, idle-read, and whole-probe deadlines are finite; a process supervisor should still cap the command itself.

For client-only memory sampling with a separate local TLS server, provide a literal socket address, a certificate identity, and a DER-encoded private CA root:

```text
cargo run --manifest-path tools/tcp-tls-probe/Cargo.toml -- hold 127.0.0.1:PORT 127.0.0.1 ROOT.der 16
```

The final argument is `16` or `32`. The probe prints `baseline`, `ready`, and `released` phases. It retains verified connections for five seconds with 16 KiB send and receive windows, checks the logical TLS reservation, then drops all connections and checks permit reclamation. It does not assert an RSS threshold; measure that externally while the local server runs in another process.

For a bounded client-only memory observation, build this probe first and run `python tools/tcp-tls-probe/memory.py --binary PATH_TO_PROBE --out NEW_OUTPUT_DIRECTORY`. The observer needs an OpenSSL executable on `PATH`, or an explicit `--openssl PATH`. It defaults to TLS 1.3; use `--tls-version 1.2` on a Python SSL runtime without TLS 1.3. It records each negotiated version, runs both 16 and 32 connection phases, saves raw client output and process samples, and deletes its generated private keys when finished. Keep its output local; never upload generated private keys if a run is interrupted before cleanup.
