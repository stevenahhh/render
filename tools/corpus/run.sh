#!/bin/sh
# Scores the renderer on the frozen corpus; see tools/corpus/README.md.
cd "$(dirname "$0")/../.." && exec bun tools/corpus/run.ts "$@"
