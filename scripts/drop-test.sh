#!/usr/bin/env bash
# Sends through a proxy that breaks the first connection, then verifies the result.
# Usage: drop-test.sh <dest> <drop-after-mb> <reset|stall> <src>...
set -e
# Build first: cargo build --release -p yeet-cli
LB="${LB:-$(dirname "$0")/../target/release/yeet-cli}"
dest="$1"; mb="$2"; mode="$3"; shift 3
rm -rf "$dest"
"$LB" receive "$dest" --port 7878 &
rpid=$!
"$LB" proxy 7900 127.0.0.1:7878 --drop-after-mb "$mb" $([ "$mode" = stall ] && echo --stall) &
ppid=$!
sleep 0.5
"$LB" send "$@" --to 127.0.0.1:7900 --limit 1g
kill $ppid $rpid 2>/dev/null || true
for s in "$@"; do "$LB" verify "$s" "$dest/$(basename "$s")" | tail -1; done
