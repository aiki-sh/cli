#!/usr/bin/env bash
# Capture a harness's native hook payloads into cli/tests/fixtures/<harness>/,
# running the agent in an isolated Apple `container` (no Docker Desktop).
#
#   ./capture.sh claude
#   ./capture.sh codex
#   RUNTIME=podman ./capture.sh claude     # fallback runtime
#   ANTHROPIC_API_KEY=... ./capture.sh claude   # key instead of mounted creds
set -euo pipefail

HARNESS="${1:?usage: ./capture.sh <harness>   (e.g. claude, codex)}"
RIG_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$RIG_DIR/../../.." && pwd)"
PROFILE="$RIG_DIR/harnesses/$HARNESS.sh"
OUT="$REPO_ROOT/cli/tests/fixtures/$HARNESS"
PROMPT="${PROMPT:-Create a file called hello.txt containing the word hello. Then you are done.}"
RUNTIME="${RUNTIME:-container}"   # Apple container; override with podman/docker

[ -f "$PROFILE" ] || { echo "no profile: $PROFILE (add harnesses/$HARNESS.sh)"; exit 1; }
command -v "$RUNTIME" >/dev/null 2>&1 || { echo "'$RUNTIME' not found. Install Apple container and run 'container system start', or set RUNTIME=podman."; exit 1; }
# shellcheck disable=SC1090
source "$PROFILE"

# Auth: prefer an API key env var if set, else mount the existing credential file.
case "$HARNESS" in
  claude) KEYVAR=ANTHROPIC_API_KEY ;;
  codex)  KEYVAR=OPENAI_API_KEY ;;
  *)      KEYVAR="" ;;
esac

AUTH_ARGS=()
if [ -n "${KEYVAR:-}" ] && [ -n "${!KEYVAR:-}" ]; then
  AUTH_ARGS=(-e "$KEYVAR=${!KEYVAR}")
  echo "auth: $KEYVAR (env var)"
else
  CRED="$(harness_cred_mount)"          # host:container:ro
  HOST_FILE="${CRED%%:*}"
  [ -f "$HOST_FILE" ] || { echo "no creds at $HOST_FILE and $KEYVAR is unset; aborting"; exit 1; }
  AUTH_ARGS=(-v "$CRED")
  echo "auth: mounting $HOST_FILE (read-only)"
fi

echo "==> build aiki-rig-$HARNESS"
"$RUNTIME" build -t "aiki-rig-$HARNESS" --build-arg HARNESS="$HARNESS" -f "$RIG_DIR/Dockerfile" "$RIG_DIR"

mkdir -p "$OUT"
echo "==> run capture ($HARNESS) -> $OUT"
# '|| true' so a non-zero agent exit still lets us collect whatever fired.
"$RUNTIME" run --rm \
  -e "AIKI_RIG_HARNESS=$HARNESS" \
  -v "$OUT:/out/$HARNESS" \
  "${AUTH_ARGS[@]}" \
  "aiki-rig-$HARNESS" \
  "source /harness.sh && harness_wire_hooks && harness_run \"$PROMPT\" || true"

echo "==> captured fixtures in $OUT:"
shopt -s nullglob
got=0
for f in "$OUT"/*.jsonl; do
  echo "  $(basename "$f"): $(grep -cE '\S' "$f" 2>/dev/null || echo 0) payload(s)"
  got=1
done
[ "$got" = 1 ] || echo "  (none - the hook config shape may be wrong for $HARNESS; see harnesses/$HARNESS.sh)"
