#!/usr/bin/env bash
# Validate the committed map-key CBOR vectors against their CDDL schemas
# using the `cbork` binary (whole-tool check).
#
# For each `cddl/vectors/project/map-key/<case>.cddl`, the matching
# `<case>.cbor` must validate and `<case>-negative.cbor` must not.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
vectors_dir="$repo_root/cddl/vectors/project/map-key"
cbork="${CBORK:-$repo_root/target/release/cbork}"

failures=0

for schema in "$vectors_dir"/*.cddl; do
    stem="$(basename "$schema" .cddl)"

    if ! "$cbork" validate --no-banner "$schema" "$vectors_dir/$stem.cbor" >/dev/null 2>&1; then
        echo "FAIL: $stem.cbor should validate against $stem.cddl" >&2
        failures=$((failures + 1))
    fi

    if "$cbork" validate --no-banner "$schema" "$vectors_dir/$stem-negative.cbor" >/dev/null 2>&1; then
        echo "FAIL: $stem-negative.cbor should NOT validate against $stem.cddl" >&2
        failures=$((failures + 1))
    fi
done

if [ "$failures" -ne 0 ]; then
    echo "$failures map-key vector(s) failed" >&2
    exit 1
fi

echo "all map-key vectors validated"
