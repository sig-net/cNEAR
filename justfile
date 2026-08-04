set dotenv-load := true

# Setup aurora-controller if not present (downloaded into the repo-local
# .cache directory, which is gitignored).
setup-controller:
    #!/usr/bin/env bash
    set -eu
    if [ ! -d ".cache/aurora-controller-factory" ]; then
        mkdir -p .cache
        git clone https://github.com/aurora-is-near/aurora-controller-factory.git .cache/aurora-controller-factory
    fi

# Build aurora-controller by invoking cargo-near directly (the controller's
# Makefile.toml wraps this same command; cargo-make is not required).
# CARGO_TARGET_DIR is pinned to the same dir the cnear contract build uses
# (repo target/ by default, or the env value when set) so the controller
# shares one dependency cache instead of compiling its own under .cache/.
build-controller: setup-controller
    #!/usr/bin/env bash
    set -eu
    CONTRACT_DIR=".cache/aurora-controller-factory"
    mkdir -p target/near
    cargo near build non-reproducible-wasm \
        --manifest-path "$CONTRACT_DIR/contract/Cargo.toml" \
        --out-dir "$CONTRACT_DIR/res" \
        --no-embed-abi \
        --no-abi
    cp "$CONTRACT_DIR/res/aurora_controller_factory.wasm" target/near/aurora-controller-factory.wasm

# Build token contract with wasm-opt. cargo-near invokes nested Cargo commands,
# so clear any user-level sccache wrapper configuration for this build.
build-token:
    #!/usr/bin/env bash
    set -eu
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

# Clean build artifacts (leaves the .cache/ controller clone in place)
clean:
    #!/usr/bin/env bash
    cargo clean
    if [ -d ".cache/aurora-controller-factory" ]; then
        cargo clean --manifest-path ".cache/aurora-controller-factory/contract/Cargo.toml"
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
# `just deploy` with no arguments runs the fully interactive flow. The legacy
# positional form from deploy.sh is still accepted and mapped inside the binary:
# `just deploy test` → ephemeral testnet --test-mode, `testnet`/`mainnet` set the
# network, and a bare signer after the network word becomes --signer-id. All other
# arguments are forwarded verbatim (subcommand or flags), preserving quoting for
# values like `--total-supply "10 NEAR"`.
deploy *ARGS:
    #!/usr/bin/env bash
    set -eu
    just build
    cargo run --manifest-path deploy/Cargo.toml -- {{ARGS}}

# Ephemeral testnet deployment with cleanup of accounts created by this run
# (same as `just deploy test`).
deploy-test:
    #!/usr/bin/env bash
    set -eu
    just build
    cargo run --manifest-path deploy/Cargo.toml -- deploy --network testnet --test-mode

# Explicit mainnet entrypoint; the binary still requires typed confirmation unless --yes is used directly.
deploy-mainnet:
    #!/usr/bin/env bash
    set -eu
    just build
    cargo run --manifest-path deploy/Cargo.toml -- deploy --network mainnet

# Delete controller.<signer> and token.<signer> accounts, sending remaining
# balances to the signer. Prompts for network and signer interactively.
clean-accounts:
    #!/usr/bin/env bash
    set -eu
    cargo run --manifest-path deploy/Cargo.toml -- clean-accounts

# Help
help:
    @just --list
