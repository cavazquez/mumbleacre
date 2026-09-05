#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
for tool in cargo armake2 python3; do command -v "$tool" >/dev/null || { echo "Falta $tool" >&2; exit 1; }; done
cargo build --locked --release --target x86_64-pc-windows-gnu -p mumbleacre-plugin
mkdir -p dist/windows-x64/@mumbleacre/addons
cp target/x86_64-pc-windows-gnu/release/mumbleacre_plugin.dll dist/windows-x64/
armake2 build acre-addon dist/windows-x64/@mumbleacre/addons/mumbleacre_acre.pbo
cp mod.cpp dist/windows-x64/@mumbleacre/
python3 scripts/package.py
