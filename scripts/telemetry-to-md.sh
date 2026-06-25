#!/usr/bin/env bash

RUN=${1:-runs/latest}
OUT=chat.md

: > "$OUT"

for f in "$RUN"/telemetry/*.txt; do
    printf "\n## %s\n\n\`\`\`\n" "$(basename "$f")" >> "$OUT"
    cat "$f" >> "$OUT"
    printf "\n\`\`\`\n" >> "$OUT"
done

echo "Wrote $OUT from $RUN"