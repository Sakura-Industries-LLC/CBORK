#!/usr/bin/env bash

set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
ONE="$ROOT/scripts/render-roundtrip.sh"
BIN=${CBORK_BIN:-"$ROOT/target/release/cbork"}

mapfile -d '' schemas < <(
    find "$ROOT" \
        -path "$ROOT/.git" -prune -o \
        -path "$ROOT/target" -prune -o \
        -path "$ROOT/tmp" -prune -o \
        -path "$ROOT/test" -prune -o \
        -path "$ROOT/plans" -prune -o \
        -path "$ROOT/cddl/vectors/project/negative" -prune -o \
        -path "$ROOT/cddl/vectors/project/semantic-errors" -prune -o \
        -path "$ROOT/cddl/vectors/project/bugs" -prune -o \
        -type f -name '*.cddl' -print0 | sort -z
)

# Candidate files that cannot round-trip by design:
# * the compiler's own postlude — it is injected into every compile,
#   not a standalone user schema;
# * `render_plug_inline.cddl` — a render fixture whose source
#   deliberately leaves names unresolved (its render shape is covered
#   by a dedicated unit test).
SKIP_EXACT=(
    "$ROOT/crates/cbork-cddl-parser/src/grammar/postlude.cddl"
    "$ROOT/cddl/vectors/project/positive/render_plug_inline.cddl"
)

index=0
for schema in "${schemas[@]}"; do
    index=$((index + 1))
    # Absolute imports/includes (`;# import "/"`, `;# include "/"`)
    # resolve relative to a harness-provided repository root and cannot
    # render standalone.
    if grep -qE ';# (import|include) "/' "$schema"; then
        continue
    fi
    if [[ " ${SKIP_EXACT[*]} " == *" $schema "* ]]; then
        continue
    fi
    printf '[%d/%d] ' "$index" "${#schemas[@]}"
    "$ONE" "$schema"
done

printf '\nRender round-trip passed for %d CDDL files.\n' "${#schemas[@]}"
