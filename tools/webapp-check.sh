#!/bin/sh
# Checks the web-app fixtures in examples/webapp against EXPECTED.tsv.
#
#   sh tools/webapp-check.sh
#
# The renderer has no text dump, so each fixture writes every marker as
# <div data-m="MARKER">MARKER</div>. For each fixture the renderer runs once
# with `--boxes`, given one selector [data-m="MARKER"] per expected marker; a
# marker passes when its line reports a laid-out box (`i x y w h | ...`), and
# fails on `i ?` (no such element) or `i - | ...` (element without a box).
set -u
root=$(cd "$(dirname "$0")/.." && pwd)
bin="$root/target/release/render"
dir="$root/examples/webapp"
tsv="$dir/EXPECTED.tsv"
[ -x "$bin" ] || { echo "webapp-check: missing $bin (cargo build --release)" >&2; exit 2; }
[ -f "$tsv" ] || { echo "webapp-check: missing $tsv" >&2; exit 2; }

tmp=$(mktemp -d "${TMPDIR:-/tmp}/webapp-check.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
trap 'exit 130' INT TERM
: > "$tmp/results"

for file in $(cut -f1 "$tsv" | awk 'NF && !seen[$0]++'); do
  awk -F '\t' -v f="$file" '$1 == f { print $2 }' "$tsv" > "$tmp/markers"
  awk '{ print "[data-m=\"" $0 "\"]" }' "$tmp/markers" > "$tmp/selectors"
  "$bin" "$dir/$file" --boxes "$tmp/selectors" --width 800 --height 600 > "$tmp/boxes" 2> "$tmp/stderr" || true
  awk -v f="$file" '
    FNR == NR { if ($1 ~ /^[0-9]+$/ && $2 ~ /^-?[0-9.]+$/) found[$1] = 1; next }
    { print ((FNR - 1) in found ? "PASS" : "FAIL") " " f " " $0 }
  ' "$tmp/boxes" "$tmp/markers" | tee -a "$tmp/results"
done

total=$(wc -l < "$tmp/results" | tr -d ' ')
passed=$(grep -c '^PASS ' "$tmp/results")
echo "webapp-check: $passed/$total passed"
[ "$passed" -eq "$total" ]
