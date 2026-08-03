set dotenv-load := true

# Setup aurora-controller if not present
setup-controller:
    #!/usr/bin/env bash
    TARGET_DIR="${CARGO_TARGET_DIR:-.}"
    if [ ! -d "$TARGET_DIR/contracts/aurora-controller-factory" ]; then
        mkdir -p "$TARGET_DIR/contracts"
        git clone https://github.com/aurora-is-near/aurora-controller-factory.git "$TARGET_DIR/contracts/aurora-controller-factory"
    fi

# Build aurora-controller using cargo make (puts wasm in res/)
build-controller: setup-controller
    #!/usr/bin/env bash
    cd contracts/aurora-controller-factory
    cargo make build
    # Copy wasms back to repo root's target/near dir
    cd ../..
    mkdir -p target/near
    cp contracts/aurora-controller-factory/res/aurora-controller-factory.wasm target/near/ 2>/dev/null || true

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
        cd "$TARGET_DIR/contracts/aurora-controller-factory"
        cargo make clean
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
