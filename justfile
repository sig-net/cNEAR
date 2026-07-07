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

# Deploy contracts (interactive or CLI mode)
# Usage: just deploy [testnet|mainnet|test] [signer_id] [--dry-run]
deploy *ARGS:
    #!/usr/bin/env bash
    set -e
    
    # Build contracts with suppressed output (show only on error)
    echo "Building contracts..."
    BUILD_OUTPUT=$(just build 2>&1)
    BUILD_EXIT=$?
    
    if [ $BUILD_EXIT -ne 0 ]; then
        echo "$BUILD_OUTPUT"
        echo "Build failed. Aborting deployment."
        exit $BUILD_EXIT
    fi
    echo "✓ Build complete"
    echo ""
    
    # Run deployment
    ARGS_STR="{{ARGS}}"
    if [[ "$ARGS_STR" == "test" || "$ARGS_STR" == "test "* ]]; then
        # Test mode: auto-select testnet, only prompt for signer
        REMAINING_ARGS="${ARGS_STR#test}"
        REMAINING_ARGS="${REMAINING_ARGS# }"
        ./scripts/deploy.sh testnet $REMAINING_ARGS --test-mode
    else
        ./scripts/deploy.sh {{ARGS}}
    fi

# Help
help:
    @just --list
