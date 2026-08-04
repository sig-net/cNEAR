set dotenv-load := true

# Setup aurora-controller if not present
setup-controller:
    #!/usr/bin/env bash
    TARGET_DIR="${CARGO_TARGET_DIR:-.}"
    if [ ! -d "$TARGET_DIR/contracts/aurora-controller-factory" ]; then
        mkdir -p "$TARGET_DIR/contracts"
        git clone https://github.com/aurora-is-near/aurora-controller-factory.git "$TARGET_DIR/contracts/aurora-controller-factory"
    fi

# Build aurora-controller by invoking cargo-near directly (the controller's
# Makefile.toml wraps this same command; cargo-make is not required).
build-controller: setup-controller
    #!/usr/bin/env bash
    set -eu
    TARGET_DIR="${CARGO_TARGET_DIR:-.}"
    CONTRACT_DIR="$TARGET_DIR/contracts/aurora-controller-factory"
    mkdir -p target/near
    cargo near build non-reproducible-wasm \
        --manifest-path "$CONTRACT_DIR/contract/Cargo.toml" \
        --out-dir "$CONTRACT_DIR/res" \
        --no-embed-abi \
        --no-abi
    cp "$CONTRACT_DIR/res/aurora_controller_factory.wasm" target/near/aurora-controller-factory.wasm

# Build token contract with wasm-opt
build-token:
    cargo near build non-reproducible-wasm

# Build both token and controller
build: build-token build-controller

# Run unit tests
test-unit:
    cargo test --lib

# Run integration tests (requires built wasm)
test-integration: build
    cargo test --test controller_integration --release

# Run all tests
test: test-unit test-integration

# Build for release
release: build

# Clean build artifacts
clean:
    #!/usr/bin/env bash
    cargo clean
    TARGET_DIR="${CARGO_TARGET_DIR:-.}"
    rm -rf target/near/
    if [ -d "$TARGET_DIR/contracts/aurora-controller-factory" ]; then
        cargo clean --manifest-path "$TARGET_DIR/contracts/aurora-controller-factory/contract/Cargo.toml"
    fi

# Check code
check:
    cargo check
    cargo clippy -- -D warnings

# Format code
fmt:
    cargo fmt

# Full pipeline: check, build, test
all: check build test

# Deploy using the typed Rust binary (no near CLI, eval, or interpolated user arguments).
# The deploy binary itself provides the complete clap-based option set.
deploy:
    #!/usr/bin/env bash
    set -eu
    just build
    cargo run --manifest-path deploy/Cargo.toml --

# Ephemeral testnet deployment with cleanup of accounts created by this run.
deploy-test:
    #!/usr/bin/env bash
    set -eu
    just build
    cargo run --manifest-path deploy/Cargo.toml -- --network testnet --test-mode

# Explicit mainnet entrypoint; the binary still requires typed confirmation unless --yes is used directly.
deploy-mainnet:
    #!/usr/bin/env bash
    set -eu
    just build
    cargo run --manifest-path deploy/Cargo.toml -- --network mainnet

# Help
help:
    @just --list
