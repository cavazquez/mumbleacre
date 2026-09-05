#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets --target x86_64-pc-windows-gnu -- -D warnings
./scripts/verify-acre-addon.sh
./scripts/test-acre-addon-sqf.sh
python3 scripts/verify-tree.py
