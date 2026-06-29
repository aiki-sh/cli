# Multi-agent e2e profile: installs BOTH claude and codex so cross-agent
# orchestration tests (e.g. claude builds, codex reviews) can run in one
# container. The e2e rig wires real aiki hooks via the baked `aiki init`, so
# only the CLIs are needed here (no harness_wire_hooks/harness_run like the
# capture rig). e2e.sh special-cases HARNESS=multi to mount BOTH creds.

harness_install() {
  npm install -g @anthropic-ai/claude-code @openai/codex
}

# Single-mount accessor (kept for interface compatibility); multi auth is handled
# by harness_cred_mounts below, which e2e.sh consumes for HARNESS=multi.
harness_cred_mount() {
  echo "$HOME/.claude/.credentials.json:/home/node/.claude/.credentials.json:ro"
}

# Both agents' cred mounts, one per line.
harness_cred_mounts() {
  echo "$HOME/.claude/.credentials.json:/home/node/.claude/.credentials.json:ro"
  echo "$HOME/.codex/auth.json:/home/node/.codex/auth.json:ro"
}
