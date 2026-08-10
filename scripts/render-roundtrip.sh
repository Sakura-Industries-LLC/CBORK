#!/usr/bin/env bash

set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
BIN=${CBORK_BIN:-"$ROOT/target/release/cbork"}

if (($# != 1)); then
    printf 'usage: %s FILE.cddl\n' "${BASH_SOURCE[0]}" >&2
    exit 2
fi

schema=$1
if [[ ! -f "$schema" ]]; then
    printf 'error: CDDL file does not exist: %s\n' "$schema" >&2
    exit 2
fi
if [[ ${schema##*.} != cddl ]]; then
    printf 'error: expected a .cddl file: %s\n' "$schema" >&2
    exit 2
fi
if [[ ! -x "$BIN" ]]; then
    printf 'error: cbork binary is missing or not executable: %s\n' "$BIN" >&2
    exit 2
fi

TMP_ROOT="$ROOT/tmp"
mkdir -p "$TMP_ROOT"
RUN_DIR=$(mktemp -d "$TMP_ROOT/render-roundtrip.XXXXXX")
trap 'rm -rf "$RUN_DIR"' EXIT

first="$RUN_DIR/first.cddl"
second="$RUN_DIR/second.cddl"
log="$RUN_DIR/render.log"

printf 'Render round-trip: %s\n' "${schema#"$ROOT/"}"

if ! "$BIN" --quiet --no-banner render --no-comments "$schema" >"$first" 2>"$log"; then
    printf 'FAIL: initial render\n'
    cat "$log"
    exit 1
fi

if ! "$BIN" --quiet --no-banner lint "$first" >>"$log" 2>&1; then
    printf 'FAIL: rendered CDDL did not pass lint\n'
    cat "$log"
    exit 1
fi

if ! "$BIN" --quiet --no-banner render --no-comments "$first" >"$second" 2>>"$log"; then
    printf 'FAIL: second render\n'
    cat "$log"
    exit 1
fi

if ! cmp -s "$first" "$second"; then
    printf 'FAIL: second render differs\n'
    diff -u "$first" "$second" || true
    exit 1
fi

printf 'ok\n'
