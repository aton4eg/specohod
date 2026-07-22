#!/usr/bin/env bash
set -euo pipefail

# Message with timestamp
ts_echo() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $*"
}

usage() {
    ts_echo "Usage: $0 <X1> <API_KEY> <MODEL> <TOTAL_TIMEOUT_SECONDS>"
    ts_echo "Example: $0 ./projects my-secret-key gpt-4o 1800"
    exit 1
}

if [ $# -ne 4 ]; then
    usage
fi

X1="$1"
API_KEY="$2"
MODEL="$3"
TOTAL_TIMEOUT="$4"

if [ ! -d "$X1" ]; then
    ts_echo "Error: '$X1' is not a directory."
    exit 1
fi

if ! [[ "$TOTAL_TIMEOUT" =~ ^[0-9]+$ ]] || [ "$TOTAL_TIMEOUT" -le 0 ]; then
    ts_echo "Error: TOTAL_TIMEOUT must be a positive integer."
    exit 1
fi

if [ -z "$MODEL" ]; then
    ts_echo "Error: MODEL must not be empty."
    exit 1
fi

if ! command -v setsid &>/dev/null; then
    ts_echo "Error: 'setsid' command not found. Install util-linux."
    exit 1
fi

CARGO_PID=""
WATCHDOG_PID=""

cleanup() {
    [ -n "$WATCHDOG_PID" ] && kill "$WATCHDOG_PID" 2>/dev/null || true
    [ -n "$CARGO_PID" ] && kill -TERM -"$CARGO_PID" 2>/dev/null || true
}

trap cleanup EXIT

handle_signal() {
    ts_echo "Interrupted. Cleaning up..."
    cleanup
    exit 1
}
trap handle_signal TERM INT

{
    sleep "$TOTAL_TIMEOUT"
    ts_echo "Global timeout reached. Terminating..."
    kill -TERM $$
} &
WATCHDOG_PID=$!

processed=0
failed=0

for dir in "$X1"/*/; do
    [ -d "$dir" ] || continue
    subdir="${dir%/}"
    ts_echo "==> Processing: $subdir"

    # Set RUST_LOG=trace 
    # output (stdout and stderr) goes to trace.log inside project directory
    RUST_LOG=trace setsid cargo run --release -- \
        --root "$subdir" \
        --api-key "$API_KEY" \
        --model "$MODEL" \
        > "$subdir/trace.log" 2>&1 &
    CARGO_PID=$!

    wait "$CARGO_PID"
    exit_code=$?
    CARGO_PID=""

    if [ $exit_code -eq 0 ]; then
        ts_echo "    Finished successfully."
        processed=$(( processed + 1 ))
    else
        if [ $exit_code -gt 128 ]; then
            ts_echo "    Killed by signal (exit code $exit_code)."
        else
            ts_echo "    Failed with exit code $exit_code."
        fi
        failed=$(( failed + 1 ))
    fi
    ts_echo ""
done

ts_echo "Done. Processed: $processed, Failed/Killed: $failed"