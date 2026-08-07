set dotenv-load := true

# Pinned aurora-controller-factory revision.
# This is *NOT* the audited commit on the reccomendation of the Aurora team
controller-commit := "10f67290653be3f04538ac24deb8edf154139b5a"

# Cache the controller source outside target, at the pinned commit.
setup-controller:
    #!/usr/bin/env bash
    set -euo pipefail
    DIR=".cache/aurora-controller-factory"
    if [ ! -d "$DIR/.git" ]; then
        mkdir -p .cache
        git clone https://github.com/aurora-is-near/aurora-controller-factory.git "$DIR"
    fi
    if ! git -C "$DIR" diff --quiet || ! git -C "$DIR" diff --cached --quiet; then
        echo "error: $DIR has local modifications; refusing to build the controller from an unverified tree" >&2
        exit 1
    fi
    # Fetch only if the pinned commit is not already present locally.
    git -C "$DIR" cat-file -e {{controller-commit}}^{commit} 2>/dev/null || git -C "$DIR" fetch origin
    git -C "$DIR" checkout --quiet --detach {{controller-commit}}

# Build aurora-controller using cargo make (puts wasm in res/)
build-controller: setup-controller
    #!/usr/bin/env bash
    set -euo pipefail
    ROOT_DIR="$PWD"
    TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT_DIR/target}"
    case "$TARGET_DIR" in
        /*) ;;
        *) TARGET_DIR="$ROOT_DIR/$TARGET_DIR" ;;
    esac
    export CARGO_TARGET_DIR="$TARGET_DIR"
    cd .cache/aurora-controller-factory
    cargo make build-and-rename
    cd "$ROOT_DIR"
    mkdir -p "$TARGET_DIR/near"
    cp .cache/aurora-controller-factory/res/aurora-controller-factory.wasm "$TARGET_DIR/near/"

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
    set -euo pipefail
    ROOT_DIR="$PWD"
    TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT_DIR/target}"
    case "$TARGET_DIR" in
        /*) ;;
        *) TARGET_DIR="$ROOT_DIR/$TARGET_DIR" ;;
    esac
    cargo clean
    rm -rf "$TARGET_DIR/near/"
    if [ -d .cache/aurora-controller-factory ]; then
        cd .cache/aurora-controller-factory
        CARGO_TARGET_DIR="$TARGET_DIR" cargo make clean
    fi

# Check code
check:
    cargo check --workspace --all-targets
    cargo clippy --workspace --all-targets -- -D warnings
    cargo fmt --all -- --check

# Format code
fmt:
    cargo fmt

# Full pipeline: check, build, test
all: check build test

# Deploy contracts (interactive or CLI mode)
# Usage: just deploy [testnet|mainnet|test] [signer_id] [--dry-run]
deploy *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    ARGS_STR="{{ARGS}}"

    # Dry runs are deliberately offline and do not build or contact a network.
    if [[ "$ARGS_STR" == *"--dry-run"* ]]; then
        cargo run --quiet --manifest-path deploy-cli/Cargo.toml -- deploy $ARGS_STR
        exit $?
    fi

    echo "Building contracts..."
    BUILD_OUTPUT=$(just build 2>&1) || {
        echo "$BUILD_OUTPUT"
        echo "Build failed. Aborting deployment."
        exit 1
    }
    echo "✓ Build complete"
    echo ""

    if [[ "$ARGS_STR" == "test" || "$ARGS_STR" == "test "* ]]; then
        # Test mode: auto-select testnet, only prompt for signer.
        REMAINING_ARGS="${ARGS_STR#test}"
        REMAINING_ARGS="${REMAINING_ARGS# }"
        cargo run --quiet --manifest-path deploy-cli/Cargo.toml -- deploy testnet $REMAINING_ARGS --test-mode
    else
        cargo run --quiet --manifest-path deploy-cli/Cargo.toml -- deploy $ARGS_STR
    fi

# Help
help:
    @just --list
