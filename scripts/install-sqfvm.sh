#!/usr/bin/env bash
# Install the pinned SQF-VM release used by local checks and CI.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION="v2026.04.03-ed9f5f5"
ASSET="sqfvm_linux_x64_gcc.zip"
SHA256="bb4e3bb415305d2ef2c4cdb75e98f442baea3a4575887c6570a11435a780615b"
BIN_SHA256="fdb10bd59226d43fadc90532e293fc0ae5978ec2065fb78cba52e62be9b97c7b"
URL="https://github.com/SQFvm/runtime/releases/download/${VERSION}/${ASSET}"
CACHE_ROOT="${SQFVM_CACHE_DIR:-$ROOT/target/tools/sqfvm}"
INSTALL_DIR="$CACHE_ROOT/$VERSION"
SQFVM_BIN="$INSTALL_DIR/sqfvm"

if [[ "$(uname -s)" != "Linux" || "$(uname -m)" != "x86_64" ]]; then
    echo "install-sqfvm: only Linux x86_64 is supported by this pinned CI tool" >&2
    exit 1
fi

if [[ -x "$SQFVM_BIN" ]]; then
    installed_sha="$(sha256sum "$SQFVM_BIN" | cut -d' ' -f1)"
    if [[ "$installed_sha" == "$BIN_SHA256" ]]; then
        printf '%s\n' "$SQFVM_BIN"
        exit 0
    fi
    echo "install-sqfvm: cached binary checksum mismatch: $SQFVM_BIN" >&2
    exit 1
fi

for command_name in curl sha256sum unzip; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "install-sqfvm: required command not found: $command_name" >&2
        exit 1
    fi
done

temp_dir="$(mktemp -d)"
trap 'rm -rf "$temp_dir"' EXIT

echo "install-sqfvm: downloading SQF-VM $VERSION" >&2
curl --fail --location --silent --show-error "$URL" --output "$temp_dir/$ASSET"

actual_sha="$(sha256sum "$temp_dir/$ASSET" | cut -d' ' -f1)"
if [[ "$actual_sha" != "$SHA256" ]]; then
    echo "install-sqfvm: checksum mismatch for $ASSET" >&2
    echo "  expected: $SHA256" >&2
    echo "  actual:   $actual_sha" >&2
    exit 1
fi

unzip -q "$temp_dir/$ASSET" -d "$temp_dir/unpacked"
mkdir -p "$INSTALL_DIR"
install -m 0755 "$temp_dir/unpacked/sqfvm_linux_x64_gcc/sqfvm" "$SQFVM_BIN"

printf '%s\n' "$SQFVM_BIN"
