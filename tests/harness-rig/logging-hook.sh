#!/usr/bin/env bash
# Universal capture shim. A harness's hook config invokes this with the event
# name as $1; it appends the raw stdin payload (the native hook JSON) to a
# fixture file under the mounted output dir, then returns an allow/no-op so the
# agent's run is not altered.
#
#   <harness hook> -> aiki-capture-hook <Event>
#   writes -> /out/<harness>/<Event>.jsonl
set -euo pipefail
event="${1:-unknown}"
harness="${AIKI_RIG_HARNESS:-harness}"
dir="/out/${harness}"
mkdir -p "$dir"

# One JSON object per line: appending lets a repeated event accumulate variety.
cat >> "${dir}/${event}.jsonl"
echo "" >> "${dir}/${event}.jsonl"

# No stdout + exit 0 = allow/continue for every harness's gateable hooks, so the
# capture run proceeds normally instead of being blocked.
exit 0
