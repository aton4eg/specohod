#!/bin/bash
set -euo pipefail   # strict mode

X1_DIR="${1:-X1}"   # default "X1"

if [ ! -d "$X1_DIR" ]; then
    echo "Error: dir '$X1_DIR' not found."
    exit 1
fi

for dir in "$X1_DIR"/*/; do
    # Skip of not dir
    [ -d "$dir" ] || continue

    YN=$(basename "$dir")
    MANIFEST="$X1_DIR/$YN/Cargo.toml"

    if [ ! -f "$MANIFEST" ]; then
        echo "Skip '$YN': missed Cargo.toml"
        continue
    fi

    echo ">>> Processing $YN ..."
    cargo run --release -- --manifest "$MANIFEST"
done