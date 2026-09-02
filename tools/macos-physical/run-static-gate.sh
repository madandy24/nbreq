#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
SOURCE_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
export PATH="$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"

stage() {
    printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$1"
}

cd "$SOURCE_ROOT"
stage "F6_STATIC_START source=$SOURCE_ROOT"
stage "HOST"
test "$(uname -m)" = "arm64"
uname -a
sw_vers
sysctl -n machdep.cpu.brand_string 2>/dev/null || true
xcodebuild -version
xcrun --show-sdk-path

stage "TOOLCHAINS"
rustc +stable --version
cargo +stable --version
rustc +1.85.0 --version
cargo +1.85.0 --version

stage "FETCH_LOCKED_GRAPHS"
cargo +1.85.0 fetch --locked
cargo +1.85.0 fetch --locked --manifest-path tools/macos-physical/Cargo.toml

stage "STABLE_24_STAGE_GATE"
cargo +stable run --locked --offline --manifest-path tools/xtask/Cargo.toml -- verify --offline

stage "MSRV_24_STAGE_GATE"
cargo +1.85.0 run --locked --offline --manifest-path tools/xtask/Cargo.toml -- verify --offline

stage "DARWIN_HELPER"
cargo +1.85.0 test --locked --offline --manifest-path support/darwin/Cargo.toml

stage "PHYSICAL_PROBES_CHECK"
cargo +stable fmt --manifest-path tools/macos-physical/Cargo.toml --check
cargo +stable clippy --locked --offline --manifest-path tools/macos-physical/Cargo.toml \
    --all-targets -- -D warnings
cargo +1.85.0 check --locked --offline --manifest-path tools/macos-physical/Cargo.toml \
    --all-targets

stage "PHYSICAL_PROBES_BUILD"
cargo +1.85.0 build --locked --offline --release --manifest-path tools/macos-physical/Cargo.toml

stage "LIVE_SMOKE"
NBREQ_SOAK_SECONDS=2 NBREQ_SOAK_INTERVAL_SECONDS=1 \
    "$SCRIPT_DIR/target/release/nbreq-f6-soak"

stage "F6_STATIC_PASS"
