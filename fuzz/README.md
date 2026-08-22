# NBReq fuzz targets

These targets are deliberately separate from NBReq's ordinary dependency graph. They require a
nightly Rust toolchain and `cargo-fuzz`; default, native, all-feature, and release builds do not
compile or depend on libFuzzer.

On Windows, run from a Visual Studio Developer PowerShell so the instrumented executable can find
`clang_rt.asan_dynamic-x86_64.dll`. A plain PowerShell reports `STATUS_DLL_NOT_FOUND` unless the
matching `VC\\Tools\\MSVC\\...\\bin\\Hostx64\\x64` directory is added to that session's `Path`.

Run the buffered native response parser/state-machine target from the repository root:

```text
cargo +nightly fuzz run native_response_decoder fuzz/corpus/native_response_decoder -- -max_len=65536 -timeout=10
```

Run the streaming decoder/backpressure state machine similarly:

```text
cargo +nightly fuzz run native_streaming_response_decoder fuzz/corpus/native_streaming_response_decoder -- -max_len=65536 -timeout=10
```

The DNS wire/policy target is bounded to the resolver's 4 KiB packet ceiling:

```text
cargo +nightly fuzz run native_dns_response fuzz/corpus/native_dns_response -- -max_len=4100 -timeout=10
```

The first six input bytes select bounded limits and fragmentation. The remainder is passed to the
production decoder whole, byte-by-byte, and in irregular chunks. A mismatch in terminal response,
portable error, reuse decision, or exact consumed-byte boundary is a finding.

Corpus files created during a local campaign remain ignored. Only reviewed `*.seed` inputs belong
in source control; promote a useful generated input by giving it a descriptive `.seed` name.

Seeds contain no captured traffic, credentials, or private data.
