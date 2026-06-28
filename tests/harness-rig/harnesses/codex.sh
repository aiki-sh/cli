# Codex capture profile.
# hooks.json shape matches aiki's own writer (cli/src/config.rs merge_codex_hooks):
# a top-level "hooks" object, camelCase event keys, each an ARRAY of
# {"hooks":[{"type":"command","command":...}]} entries. Codex exposes 4 events.

harness_install() {
  npm install -g @openai/codex
}

harness_cred_mount() {
  echo "$HOME/.codex/auth.json:/home/node/.codex/auth.json:ro"
}

harness_wire_hooks() {
  mkdir -p "$HOME/.codex"
  # hooks.json keys are PascalCase (codex's schema). Command -> capture shim.
  cat > "$HOME/.codex/hooks.json" <<'JSON'
{
  "hooks": {
    "SessionStart":     [{"hooks": [{"type": "command", "command": "aiki-capture-hook SessionStart"}]}],
    "UserPromptSubmit": [{"hooks": [{"type": "command", "command": "aiki-capture-hook UserPromptSubmit"}]}],
    "PreToolUse":       [{"hooks": [{"type": "command", "command": "aiki-capture-hook PreToolUse"}]}],
    "Stop":             [{"hooks": [{"type": "command", "command": "aiki-capture-hook Stop"}]}]
  }
}
JSON
  # Codex hides hooks behind a feature flag (default off); enable it. Hook trust
  # is a separate gate that headless `codex exec` silently skips, so we pass
  # --dangerously-bypass-hook-trust at run time (harness_run) rather than
  # pre-computing per-hook trusted_hash values.
  cat > "$HOME/.codex/config.toml" <<'TOML'
[features]
codex_hooks = true
TOML
}

harness_run() {
  # Headless exec mode; /work is not a git repo, so skip the check.
  # --dangerously-bypass-hook-trust so codex runs our enabled hooks (otherwise a
  # headless exec silently skips untrusted hooks).
  codex exec --dangerously-bypass-approvals-and-sandbox --dangerously-bypass-hook-trust --skip-git-repo-check "$1"
}

harness_events() {
  echo "SessionStart UserPromptSubmit PreToolUse Stop"
}
