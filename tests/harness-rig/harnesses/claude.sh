# Claude Code capture profile.
# Hook schema matches what `aiki init` writes to ~/.claude/settings.json
# (cli/src/config.rs install_claude_code_hooks_global).

harness_install() {
  npm install -g @anthropic-ai/claude-code
}

# host:container:ro mount that gives the container the user's existing auth.
# Falls back to ANTHROPIC_API_KEY (handled in capture.sh) if the file is absent.
harness_cred_mount() {
  echo "$HOME/.claude/.credentials.json:/home/node/.claude/.credentials.json:ro"
}

# Point every hook event at the capture shim.
harness_wire_hooks() {
  mkdir -p "$HOME/.claude"
  cat > "$HOME/.claude/settings.json" <<'JSON'
{
  "hooks": {
    "SessionStart":     [{"matcher": "", "hooks": [{"type": "command", "command": "aiki-capture-hook SessionStart"}]}],
    "UserPromptSubmit": [{"hooks": [{"type": "command", "command": "aiki-capture-hook UserPromptSubmit"}]}],
    "PreToolUse":       [{"matcher": "", "hooks": [{"type": "command", "command": "aiki-capture-hook PreToolUse"}]}],
    "PostToolUse":      [{"matcher": "", "hooks": [{"type": "command", "command": "aiki-capture-hook PostToolUse"}]}],
    "Stop":             [{"hooks": [{"type": "command", "command": "aiki-capture-hook Stop"}]}],
    "PreCompact":       [{"hooks": [{"type": "command", "command": "aiki-capture-hook PreCompact"}]}],
    "SessionEnd":       [{"hooks": [{"type": "command", "command": "aiki-capture-hook SessionEnd"}]}]
  }
}
JSON
}

harness_run() {
  # Headless, non-interactive, auto-approve so the hooks fire without prompting.
  claude --print --dangerously-skip-permissions "$1"
}

harness_events() {
  echo "SessionStart UserPromptSubmit PreToolUse PostToolUse Stop PreCompact SessionEnd"
}
