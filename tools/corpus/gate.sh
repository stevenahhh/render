#!/bin/sh
# usage: sh tools/corpus/gate.sh <results dir>
# Fails (exit 1) when a corpus run scores under the floors in tools/corpus/floor.json.
# Run the corpus first: sh tools/corpus/run.sh --jobs 6, then pass its results directory.
set -e
dir="${1:?usage: sh tools/corpus/gate.sh <results dir>}"
here="$(cd "$(dirname "$0")" && pwd)"
exec bun -e '
const fs = require("fs");
const s = JSON.parse(fs.readFileSync(process.argv[1] + "/summary.json", "utf8"));
const f = JSON.parse(fs.readFileSync(process.argv[2], "utf8")).floor;
const got = { sitesPass: s.sites.pass, sitesMean: s.sites.mean, wptReftests: s.wpt.reftests.pass, wptTestharness: s.wpt.testharness.pass };
let bad = 0;
for (const k of Object.keys(f)) { const ok = got[k] >= f[k]; if (!ok) bad++; console.log((ok ? "ok   " : "FAIL ") + k + " " + got[k] + " (floor " + f[k] + ")"); }
process.exit(bad ? 1 : 0);
' "$dir" "$here/floor.json"
