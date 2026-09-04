#!/usr/bin/env bash
set -euo pipefail

FUZZ_MAX_TOTAL_TIME="${FUZZ_MAX_TOTAL_TIME:-300}"
FUZZ_MAX_LEN="${FUZZ_MAX_LEN:-1048576}"
FUZZ_TARGETS=(
    markdown_to_notebook
    script_to_notebook
    notebook_json
    export_dispatch
)

cd "$(dirname "${BASH_SOURCE[0]}")"

printf 'Building all fuzz targets...\n'
cargo +nightly fuzz build

printf 'Configuration: max_total_time=%s max_len=%s targets=%s\n' \
    "$FUZZ_MAX_TOTAL_TIME" "$FUZZ_MAX_LEN" "${#FUZZ_TARGETS[@]}"

for target in "${FUZZ_TARGETS[@]}"; do
    printf '\n=== START %s ===\n' "$target"
    cargo +nightly fuzz run "$target" -- \
        -max_total_time="$FUZZ_MAX_TOTAL_TIME" \
        -max_len="$FUZZ_MAX_LEN" \
        -print_final_stats=1
    printf '=== PASS %s ===\n' "$target"
done

printf '\nAll fuzz targets completed successfully.\n'