#!/usr/bin/env bash

set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
ONE="$ROOT/scripts/render-roundtrip.sh"

mapfile -d '' schemas < <(
    find "$ROOT" \
        -path "$ROOT/.git" -prune -o \
        -path "$ROOT/target" -prune -o \
        -path "$ROOT/tmp" -prune -o \
        -type f -name '*.cddl' -print0 | sort -z
)

index=0
for schema in "${schemas[@]}"; do
    index=$((index + 1))
    printf '[%d/%d] ' "$index" "${#schemas[@]}"
    "$ONE" "$schema"
done

printf '\nRender round-trip passed for %d CDDL files.\n' "${#schemas[@]}"
