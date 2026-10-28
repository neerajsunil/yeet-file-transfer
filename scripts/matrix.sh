#!/usr/bin/env bash
# Runs every dataset x link x compression combination over loopback and prints one line each.
# Usage: matrix.sh <dest-dir> [limits...]   (default limits: 100m 500m 1g none)
DEST="$1"; shift
LIMITS=("$@")
[ $# -eq 0 ] && LIMITS=(100m 500m 1g none)
RUN="$(dirname "$0")/run.sh"
# Datasets: a folder of many small files, and big files that do / don't compress.
# Point these at your own data; several paths are separated by "|".
SMALL_FILES="${SMALL_FILES:-$HOME/.rustup/toolchains}"
COMPRESSIBLE="${COMPRESSIBLE:?set COMPRESSIBLE to files that compress well, e.g. game .zip/.pak archives}"
INCOMPRESSIBLE="${INCOMPRESSIBLE:?set INCOMPRESSIBLE to already-compressed files, e.g. video or game bundles}"

declare -A SETS=(
  [small-files]="$SMALL_FILES"
  [game-compressible]="$COMPRESSIBLE"
  [game-incompressible]="$INCOMPRESSIBLE"
)

printf "%-20s %-5s %-5s %8s %7s %9s %9s %6s\n" dataset link mode time wire% floor-raw floor-own eff
for name in small-files game-compressible game-incompressible; do
  IFS='|' read -ra srcs <<< "${SETS[$name]}"
  for limit in "${LIMITS[@]}"; do
    for mode in auto none; do
      out=$(COMPRESS=$mode "$RUN" "$DEST" "$limit" "${srcs[@]}" 2>&1)
      t=$(sed -n 's/^time \([0-9.]*\)s.*/\1/p' <<< "$out")
      wire=$(sed -n 's/.*on wire (\([0-9.]*\)%).*/\1/p' <<< "$out")
      fr=$(sed -n 's/.*uncompressed floor \([0-9.]*\)s.*/\1/p' <<< "$out")
      fo=$(sed -n 's/.*own-wire floor \([0-9.]*\)s.*/\1/p' <<< "$out")
      eff=$(sed -n 's/.*link efficiency \([0-9]*\)%.*/\1/p' <<< "$out")
      printf "%-20s %-5s %-5s %7ss %6s%% %8ss %8ss %5s%%\n" "$name" "$limit" "$mode" "$t" "$wire" "${fr:--}" "${fo:--}" "${eff:--}"
      [ -z "$t" ] && echo "$out"
    done
  done
done
rm -rf "$DEST"
