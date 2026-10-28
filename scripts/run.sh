#!/usr/bin/env bash
# One transfer over loopback: run.sh <dest> <limit|none> [--verify] <src>...
# Starts a one-shot receiver, sends, then optionally hash-checks the result.
set -e
# Build first: cargo build --release -p yeet-cli (add --features timing for stage timings)
LB="${LB:-$(dirname "$0")/../target/release/yeet-cli}"
dest="$1"; limit="$2"; shift 2
verify=0; [ "$1" = "--verify" ] && { verify=1; shift; }
rm -rf "$dest"
"$LB" receive "$dest" --port 7878 --once ${RECV_THREADS:+--threads $RECV_THREADS} | grep -v listening &
lim=(); [ "$limit" != "none" ] && lim=(--limit "$limit")
"$LB" send "$@" --to 127.0.0.1:7878 "${lim[@]}" ${COMPRESS:+--compress $COMPRESS}
wait
if [ $verify = 1 ]; then
  for s in "$@"; do "$LB" verify "$s" "$dest/$(basename "$s")" | tail -1; done
fi
