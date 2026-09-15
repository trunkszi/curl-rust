#!/bin/sh
set -ex
RUSTFLAGS=-Dwarnings cargo fmt -- --check
RUSTFLAGS=-Dwarnings cargo build --all-targets --target "$TARGET"
if [ "${TARGET%%-*}" = "x86_64" ] && command -v nm >/dev/null 2>&1; then
    for archive in $(find "target/$TARGET" -name libcurl.a -type f); do
        ! nm -g "$archive" | grep -E '(^|[[:space:]])(pthread_create|thrd_create|CreateThread)$'
    done
fi
RUSTFLAGS=-Dwarnings cargo test --target "$TARGET"
RUSTFLAGS=-Dwarnings cargo test --target "$TARGET" --no-run --features http2
RUSTFLAGS=-Clink-dead-code cargo run --manifest-path systest/Cargo.toml --target "$TARGET"
cargo doc --no-deps --target "$TARGET"
