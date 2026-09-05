#!/usr/bin/env bash
# Execute deterministic contracts for the thin ACRE mission addon in SQF-VM.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SQFVM_BIN="${SQFVM_BIN:-}"
cd "$ROOT"

if [[ -z "$SQFVM_BIN" ]]; then
    SQFVM_BIN="$("$ROOT/scripts/install-sqfvm.sh")"
fi
if [[ ! -x "$SQFVM_BIN" ]]; then
    echo "test-acre-addon-sqf: SQF-VM is not executable: $SQFVM_BIN" >&2
    exit 1
fi

output_file="$(mktemp)"
trap 'rm -f "$output_file"' EXIT

if ! "$SQFVM_BIN" \
    --automated \
    --suppress-welcome \
    --no-execute-print \
    --no-spawn-player \
    --no-work-print \
    --max-runtime 10000 \
    --define hasInterface=true \
    --input-sqf "$ROOT/tests/sqf/acre_addon_contracts.sqf" \
    >"$output_file" 2>&1; then
    cat "$output_file" >&2
    echo "test-acre-addon-sqf: SQF-VM process failed" >&2
    exit 1
fi

if grep -Eq '\[(ERR|FAT)\]|MumbleACRE_ACRE_SQF_FAIL:' "$output_file"; then
    cat "$output_file" >&2
    echo "test-acre-addon-sqf: SQF runtime error or failed fixture" >&2
    exit 1
fi
if ! grep -Fq 'MumbleACRE_ACRE_SQF_PASS:' "$output_file"; then
    cat "$output_file" >&2
    echo "test-acre-addon-sqf: completion marker missing" >&2
    exit 1
fi

grep -F 'MumbleACRE_ACRE_SQF_PASS:' "$output_file"
echo "test-acre-addon-sqf: PASS"
