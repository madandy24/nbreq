#!/bin/sh
# Run in an isolated source lab. No machine trust-store or system configuration changes.
set -eu
lab=$1
scope=$2
source_id=$3
export PATH="$HOME/.cargo/bin:/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin"
export CARGO_TARGET_DIR="$lab/target"
export CARGO_BUILD_JOBS=2
export CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_PROFILE_TEST_DEBUG=0
trap 'code=$?; printf "%s\n" "$code" > "$lab/run.exit"' EXIT
cd "$lab/nbreq"
python3 -c 'import hashlib,pathlib; p=pathlib.Path("m1-source.sha256"); rows=[line.split("  ",1) for line in p.read_text().splitlines()]; assert all(hashlib.sha256(pathlib.Path(name).read_bytes()).hexdigest()==digest for digest,name in rows); print("SOURCE_MANIFEST_PASS",len(rows))'
export RUSTUP_TOOLCHAIN=stable
cargo fetch --locked
cargo fetch --locked --manifest-path tools/f5-observe/memory/Cargo.toml
for toolchain in stable 1.85.0; do
    export RUSTUP_TOOLCHAIN=$toolchain
    rustc --version
    cargo run --offline --manifest-path tools/xtask/Cargo.toml -- verify --offline > "$lab/verify-$toolchain.log" 2>&1
    tail -n 2 "$lab/verify-$toolchain.log"
    cargo test --locked --offline --manifest-path tools/f5-observe/memory/meter/Cargo.toml
    cargo clippy --locked --offline --manifest-path tools/f5-observe/memory/meter/Cargo.toml --all-targets -- -D warnings
    cargo clippy --locked --offline --manifest-path tools/f5-observe/memory/Cargo.toml --all-targets --all-features -- -D warnings
done
export RUSTUP_TOOLCHAIN=stable
mkdir -p "$lab/bin"
cargo build --release --locked --offline --manifest-path tools/f5-observe/memory/Cargo.toml
cp "$CARGO_TARGET_DIR/release/nbreq-f5-memory" "$lab/bin/plain"
cargo build --release --locked --offline --manifest-path tools/f5-observe/memory/Cargo.toml --features alloc-meter
cp "$CARGO_TARGET_DIR/release/nbreq-f5-memory" "$lab/bin/meter"
python3 tools/f5-observe/memory/test_runner.py --plain "$lab/bin/plain" --meter "$lab/bin/meter" --output "$lab/runner-tests"
if test "$scope" = full; then
    python3 tools/f5-observe/memory/run.py --plain "$lab/bin/plain" --meter "$lab/bin/meter" --output "$lab/baseline" --source "$source_id"
else
    python3 tools/f5-observe/memory/run.py --plain "$lab/bin/plain" --meter "$lab/bin/meter" --output "$lab/baseline" --source "$source_id" --smoke
fi
python3 -c 'import hashlib,pathlib; rows=[line.split("  ",1) for line in pathlib.Path("m1-source.sha256").read_text().splitlines()]; assert all(hashlib.sha256(pathlib.Path(name).read_bytes()).hexdigest()==digest for digest,name in rows); print("SOURCE_MANIFEST_PASS",len(rows))'
printf 'M1_HOST_PASS\n'
