#!/usr/bin/env bash
set -euo pipefail

# Compatibility entry point. The deployment implementation lives in Rust.
exec cargo run --quiet --manifest-path deploy-cli/Cargo.toml -- deploy "$@"
