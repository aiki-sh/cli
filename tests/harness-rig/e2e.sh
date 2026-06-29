#!/usr/bin/env bash
# Build the live-e2e image (aiki + harness + jj) and run the existing #[ignore]
# e2e tests against the REAL agent, in an isolated Apple container. This is the
# functional layer: it asserts aiki behaviour (task closed, [aiki] provenance,
# file-in-jj-history, tokens), unlike capture.sh which only records payloads.
#
#   ./e2e.sh claude          # runs e2e_claude_provenance_* with mounted creds
#   ./e2e.sh codex
#   TESTFILTER=e2e_codex ./e2e.sh codex      # custom test filter
#   RUNTIME=podman ./e2e.sh claude           # fallback runtime
set -euo pipefail

HARNESS="${1:?usage: ./e2e.sh <harness>   (e.g. claude, codex)}"
RIG_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$RIG_DIR/../../.." && pwd)"
PROFILE="$RIG_DIR/harnesses/$HARNESS.sh"
RUNTIME="${RUNTIME:-container}"
# Default filter: per-harness provenance, or the cross-agent suite for `multi`.
if [ "$HARNESS" = "multi" ]; then
  TESTFILTER="${TESTFILTER:-e2e_multi}"
else
  TESTFILTER="${TESTFILTER:-e2e_${HARNESS}_provenance}"
fi

[ -f "$PROFILE" ] || { echo "no profile: $PROFILE"; exit 1; }
command -v "$RUNTIME" >/dev/null 2>&1 || { echo "'$RUNTIME' not found (install Apple container + 'container system start', or RUNTIME=podman)."; exit 1; }
# shellcheck disable=SC1090
source "$PROFILE"

# Auth. The multi rig needs BOTH agents' creds (no single API-key path); each
# single-agent rig prefers an API key env var, else mounts its credential file.
AUTH_ARGS=()
if [ "$HARNESS" = "multi" ]; then
  while IFS= read -r cred; do
    [ -n "$cred" ] || continue
    host="${cred%%:*}"
    [ -f "$host" ] || { echo "multi: missing cred $host; aborting"; exit 1; }
    AUTH_ARGS+=(-v "$cred"); echo "auth: mounting $host (read-only)"
  done < <(harness_cred_mounts)
else
  case "$HARNESS" in
    claude) KEYVAR=ANTHROPIC_API_KEY ;;
    codex)  KEYVAR=OPENAI_API_KEY ;;
    *)      KEYVAR="" ;;
  esac
  if [ -n "${KEYVAR:-}" ] && [ -n "${!KEYVAR:-}" ]; then
    AUTH_ARGS=(-e "$KEYVAR=${!KEYVAR}"); echo "auth: $KEYVAR (env var)"
  else
    CRED="$(harness_cred_mount)"; HOST_FILE="${CRED%%:*}"
    [ -f "$HOST_FILE" ] || { echo "no creds at $HOST_FILE and $KEYVAR unset; aborting"; exit 1; }
    AUTH_ARGS=(-v "$CRED"); echo "auth: mounting $HOST_FILE (read-only)"
  fi
fi

# Stage a clean copy of cli/ to build from, because Apple `container`:
#   - scans target/ even with a .dockerignore (and races a live cargo build), and
#   - the repo root has symlink loops that hang the context scanner.
# The stage MUST live under $HOME: Apple container's build VM cannot read /tmp
# or /private/tmp (mktemp), and an unreadable context silently transfers empty.
echo "==> stage a clean cli/ build context under \$HOME"
STAGE_ROOT="$HOME/.cache/aiki-harness-e2e"
STAGE="$STAGE_ROOT/cli"
rm -rf "$STAGE_ROOT" && mkdir -p "$STAGE"
trap 'rm -rf "$STAGE_ROOT"' EXIT
rsync -a --exclude='target/' --exclude='CLAUDE.md' --exclude='node_modules/' --exclude='.dockerignore' "$REPO_ROOT/cli/" "$STAGE/"

echo "==> build aiki-e2e-$HARNESS (first build compiles aiki for linux; slow, then cached)"
"$RUNTIME" build -t "aiki-e2e-$HARNESS" --build-arg HARNESS="$HARNESS" -f "$RIG_DIR/Dockerfile.e2e" "$STAGE"

echo "==> run live e2e ($TESTFILTER) against the real $HARNESS"
# Prepend the rust cargo bin dir defensively: depending on shell/login mode the
# image's ENV PATH can be reset, and the test driver re-invokes `cargo test`.
"$RUNTIME" run --rm "${AUTH_ARGS[@]}" "aiki-e2e-$HARNESS" \
  "export PATH=/usr/local/cargo/bin:\$PATH; cd /src && cargo test --test e2e -- --ignored --nocapture --test-threads=1 $TESTFILTER"
