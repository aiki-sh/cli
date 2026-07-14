use anyhow::{Context, Result};
use serde_json::json;
use std::fs;
use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// Save the current git core.hooksPath configuration before installing aiki hooks
///
/// This preserves the previous hooks path so that aiki hooks can chain to it.
/// The path is saved to `.aiki/.previous_hooks_path`.
///
/// Three states are handled:
/// 1. Not set (git config returns empty) - saves ".git/hooks" (Git's default)
/// 2. Empty string - saves "EMPTY"
/// 3. Valid path - saves the actual path
pub fn save_previous_hooks_path(repo_root: &Path) -> Result<()> {
    let aiki_dir = repo_root.join(".aiki");
    let previous_path_file = aiki_dir.join(".previous_hooks_path");

    // Get current core.hooksPath value
    let output = Command::new("git")
        .args(["config", "core.hooksPath"])
        .current_dir(repo_root)
        .output()
        .context("Failed to run git config core.hooksPath")?;

    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            // A custom hooks path is configured - save it
            fs::write(&previous_path_file, &path)
                .context("Failed to write .previous_hooks_path")?;
            println!("✓ Saved previous hooks path: {}", path);
        } else {
            // Empty string - save "EMPTY" to distinguish from not-set
            fs::write(&previous_path_file, "EMPTY")
                .context("Failed to write .previous_hooks_path")?;
            println!("✓ Saved previous hooks path: EMPTY");
        }
    } else {
        // Config key doesn't exist - no previous hooks path to save
        // Don't create .previous_hooks_path file at all
        println!("✓ No previous hooks path configured");
    }

    Ok(())
}

/// Get the absolute path to the aiki binary (cached).
///
/// Uses the cached `AIKI_BINARY_PATH` from the cache module.
/// The path is resolved once per process using `which aiki` or
/// falling back to `std::env::current_exe()`.
#[must_use]
pub fn get_aiki_binary_path() -> String {
    (*crate::cache::AIKI_BINARY_PATH).clone()
}

/// POSIX-sh walk-up that locates a repo `.aiki/` starting from `$PWD`, while
/// **excluding aiki's own global home** (`${AIKI_HOME:-$HOME/.aiki}`). The global
/// home is a directory literally named `.aiki`, so without this exclusion every
/// non-aiki repo under `$HOME` would falsely match. On success it leaves the
/// repo root in `$d` and the resolved global home in `$h`; otherwise it
/// `exit 0`s silently before the aiki binary ever loads.
///
/// Pure parameter-expansion POSIX (`${d%/*}` + `! { … }` grouping), so it runs
/// under `sh`/`dash`/`bash`/`zsh`/`ksh`. Factored into one constant so the gate
/// string is identical across every editor — drift here is a silent bug.
const BASH_GATE_WALKUP: &str = "d=$PWD; h=${AIKI_HOME:-$HOME/.aiki}; \
while [ -n \"$d\" ] && [ \"$d\" != / ] && ! { [ -d \"$d/.aiki\" ] && [ \"$d/.aiki\" != \"$h\" ]; }; do d=${d%/*}; done; \
[ -n \"$d\" ] && [ -d \"$d/.aiki\" ] && [ \"$d/.aiki\" != \"$h\" ] || exit 0";

/// Wrap a raw `aiki hooks stdin …` command in the **session-start** bash gate:
/// gates on a repo `.aiki/` only (not the per-user marker), so a
/// cloned-but-not-enabled (Dormant) repo still reaches the Rust gate, which
/// emits the "not active" discovery signal on SessionStart.
fn gate_session_start(raw_command: &str) -> String {
    format!("{BASH_GATE_WALKUP}; exec {raw_command}")
}

/// Wrap a raw `aiki hooks stdin …` command in the **full** bash gate: gates on
/// a repo `.aiki/` AND the per-user enable marker, so a Dormant repo exits
/// before the aiki binary loads. Used for every event except session-start.
fn gate_with_marker(raw_command: &str) -> String {
    format!("{BASH_GATE_WALKUP}; [ -f \"$h/.init/repos$d/enabled\" ] || exit 0; exec {raw_command}")
}

/// Install global Git hooks in ~/.aiki/githooks/
pub fn install_global_git_hooks() -> Result<()> {
    let home_dir = dirs::home_dir().context("Could not find home directory")?;
    let githooks_dir = home_dir.join(".aiki/githooks");

    // Create directory if it doesn't exist
    fs::create_dir_all(&githooks_dir).context("Failed to create ~/.aiki/githooks directory")?;

    // Read hook template (embedded in binary)
    let template = include_str!("../templates/prepare-commit-msg.sh");

    // For global hook, we read the previous path at runtime from .aiki/.previous_hooks_path
    // The template already handles this - we replace the placeholder with a shell command
    let hook_content = template.replace(
        "PREVIOUS_HOOK=\"__PREVIOUS_HOOK_PATH__\"",
        "PREVIOUS_HOOK=\"$(cat .aiki/.previous_hooks_path 2>/dev/null || echo '')\"",
    );

    let hook_file = githooks_dir.join("prepare-commit-msg");
    fs::write(&hook_file, hook_content).context("Failed to write prepare-commit-msg hook")?;

    // Make hook executable (Unix/macOS/Linux)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&hook_file)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&hook_file, perms)?;
    }

    println!("✓ Installed Git hooks at {}", githooks_dir.display());
    Ok(())
}

/// True if `cmd` invokes the aiki hooks handler, i.e. some whitespace-delimited
/// word is the aiki binary (basename `aiki` or `aiki.exe`, with or without a
/// path prefix) and is immediately followed by `hooks stdin`. Mirrors the
/// detection in doctor's `is_aiki_hooks_command_with_params` so init's
/// append-and-filter idempotency holds for `.exe` and path-prefixed forms.
pub(crate) fn command_invokes_aiki_hook(cmd: &str) -> bool {
    let words: Vec<&str> = cmd.split_whitespace().collect();
    words.iter().enumerate().any(|(i, w)| {
        (w.ends_with("aiki") || w.ends_with("aiki.exe"))
            && words.get(i + 1) == Some(&"hooks")
            && words.get(i + 2) == Some(&"stdin")
    })
}

/// Install global Claude Code hooks in ~/.claude/settings.json
/// Returns true if a Claude/Codex-shaped hook entry is one of ours, i.e. its
/// nested `hooks[].command` invokes the aiki hooks handler (per
/// `command_invokes_aiki_hook`). Used for idempotent, non-destructive
/// installation (append-and-filter).
fn entry_has_aiki_command(entry: &serde_json::Value) -> bool {
    entry
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|hooks| {
            hooks.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(command_invokes_aiki_hook)
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// Human-readable name of a JSON value's type, used to make "wrong shape"
/// error messages actionable (so the user knows what they need to fix).
fn json_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Ensure a Claude Code hook entry for `event` exists in `settings`, appending
/// our command without disturbing pre-existing entries (the user's or another
/// tool's). Idempotent: if our entry is already present (detected via the
/// `aiki hooks stdin` substring) nothing is pushed.
///
/// Returns the number of pre-existing non-aiki entries in this event's array,
/// so the caller can report what it preserved. Errors (rather than silently
/// overwriting) if `hooks.<event>` is present with an unexpected JSON shape.
fn ensure_claude_hook_entry(
    settings: &mut serde_json::Value,
    event: &str,
    matcher: &str,
    command: &str,
    timeout: u64,
) -> Result<usize> {
    // Read via `.get()` chaining so a missing key is NOT auto-vivified (Index
    // would). Absent or null means "no entries yet" and we create a fresh
    // array. A present-but-wrong-typed value means the file was hand-edited
    // into a shape we don't understand: refuse to overwrite the user's data.
    let existing = match settings.get("hooks").and_then(|h| h.get(event)) {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(serde_json::Value::Array(arr)) => arr.clone(),
        Some(other) => anyhow::bail!(
            "~/.claude/settings.json hooks.{} is not an array (found {}); refusing to overwrite. Fix or remove it and re-run.",
            event,
            json_type_name(other)
        ),
    };

    // Filter-and-append: drop any of OUR existing entries (old bare command or a
    // stale duplicate) and re-append the canonical one. This preserves every
    // non-aiki entry untouched while MIGRATING a pre-gate `aiki hooks stdin`
    // command to the inline-gated form, and collapses duplicates. Idempotent:
    // re-running with the canonical entry already present reproduces it exactly.
    let mut merged: Vec<serde_json::Value> = existing
        .iter()
        .filter(|entry| !entry_has_aiki_command(entry))
        .cloned()
        .collect();
    let preserved = merged.len();
    merged.push(json!({
        "matcher": matcher,
        "hooks": [{
            "type": "command",
            "command": command,
            "timeout": timeout
        }]
    }));
    // Safe: callers guarantee settings["hooks"] is an object before calling
    // (see merge_claude_hooks), so this Index-assign cannot panic.
    settings["hooks"][event] = json!(merged);

    Ok(preserved)
}

/// Ensure a Codex hook entry for `event_key` exists in the hooks map, appending
/// our command without disturbing pre-existing entries. Idempotent: if our entry
/// is already present (detected via the `aiki hooks stdin` substring) nothing is
/// pushed.
///
/// Returns the number of pre-existing non-aiki entries in this event's array.
/// Errors (rather than silently overwriting) if `event_key` is present with an
/// unexpected JSON shape.
fn ensure_codex_hook_entry(
    hooks_map: &mut serde_json::Map<String, serde_json::Value>,
    event_key: &str,
    command: &str,
) -> Result<usize> {
    // Absent or null means "no entries yet" (create fresh). A present-but-
    // wrong-typed value means a hand-edited shape we don't understand: refuse
    // to overwrite the user's data rather than clobbering it.
    let existing = match hooks_map.get(event_key) {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(serde_json::Value::Array(arr)) => arr.clone(),
        Some(other) => anyhow::bail!(
            "~/.codex/hooks.json hooks.{} is not an array (found {}); refusing to overwrite. Fix or remove it and re-run.",
            event_key,
            json_type_name(other)
        ),
    };

    // Filter-and-append (see `ensure_claude_hook_entry`): preserve non-aiki
    // entries, replace our own with the canonical gated command (migrating the
    // pre-gate form), and collapse duplicates. Idempotent.
    let mut merged: Vec<serde_json::Value> = existing
        .iter()
        .filter(|entry| !entry_has_aiki_command(entry))
        .cloned()
        .collect();
    let preserved = merged.len();
    merged.push(json!({
        "hooks": [{
            "type": "command",
            "command": command,
        }]
    }));
    hooks_map.insert(event_key.to_string(), json!(merged));

    Ok(preserved)
}

/// Ensure a Cursor hook entry for `hook_name` exists, replacing any of OUR own
/// entries (migrating the pre-gate command to the inline-gated form) and
/// preserving every other entry. Cursor uses a flat `{ "command": "..." }`
/// entry shape. Idempotent.
fn ensure_cursor_hook_entry(hooks: &mut serde_json::Value, hook_name: &str, command: &str) {
    let existing = hooks["hooks"][hook_name]
        .as_array()
        .cloned()
        .unwrap_or_default();

    let mut merged: Vec<serde_json::Value> = existing
        .into_iter()
        .filter(|entry| {
            !entry
                .get("command")
                .and_then(|c| c.as_str())
                .map(command_invokes_aiki_hook)
                .unwrap_or(false)
        })
        .collect();
    merged.push(json!({ "command": command }));
    hooks["hooks"][hook_name] = json!(merged);
}

/// Validate the existing Codex `hooks_json` shape and append our hook entries
/// for every event, without clobbering pre-existing entries. Operates on an
/// in-memory value so it can be unit-tested without touching disk. Returns the
/// count of pre-existing non-aiki entries preserved across all events.
///
/// Errors (rather than silently overwriting) if the top-level `hooks` value is
/// present with an unexpected JSON shape, or if any event array is malformed.
fn merge_codex_hooks(hooks_json: &mut serde_json::Value) -> Result<usize> {
    // The root must be a JSON object (or null/absent = new file). A concrete
    // non-object root is a hand-edited shape we don't understand: refuse to
    // overwrite rather than panicking on the `hooks_json["hooks"] = ...`
    // Index-assign below (serde_json's IndexMut only auto-vivifies Null, and
    // panics on array/string/number/bool).
    match &*hooks_json {
        serde_json::Value::Object(_) | serde_json::Value::Null => {}
        other => anyhow::bail!(
            "~/.codex/hooks.json root is not an object (found {}); refusing to overwrite. Fix or remove it and re-run.",
            json_type_name(other)
        ),
    }

    // `hooks` must be an object. Absent or null is fine (create fresh); any
    // other concrete type means a hand-edited shape we don't understand, so
    // refuse to overwrite it.
    match hooks_json.get("hooks") {
        None | Some(serde_json::Value::Null) => {
            hooks_json["hooks"] = json!({});
        }
        Some(serde_json::Value::Object(_)) => {}
        Some(other) => anyhow::bail!(
            "~/.codex/hooks.json hooks is not an object (found {}); refusing to overwrite. Fix or remove it and re-run.",
            json_type_name(other)
        ),
    }

    let hook_events = [
        ("SessionStart", "sessionStart"),
        ("UserPromptSubmit", "userPromptSubmit"),
        ("PreToolUse", "preToolUse"),
        ("Stop", "stop"),
    ];

    // The object shape is guaranteed above, so as_object_mut is infallible here
    // (kept as a belt-and-suspenders guard).
    let hooks_map = hooks_json["hooks"]
        .as_object_mut()
        .context("'hooks' in ~/.codex/hooks.json is not an object")?;

    let mut codex_preserved = 0usize;
    for (event_key, event_arg) in &hook_events {
        // FROZEN CONTRACT (Codex hook trust): Codex records hook trust as a hash
        // over the hook command. Changing this command string revokes an
        // interactive user's one-time `/hooks` approval and re-prompts them.
        // Treat any edit as a deliberate, re-trust-forcing change, not a
        // refactor. Pinned by `codex_hook_command_strings_are_frozen`. See
        // ops/now/codex-hooks-feature-flag-migration.md (Step 6).
        let raw = format!("aiki hooks stdin --codex {}", event_arg);
        let command = if *event_key == "SessionStart" {
            gate_session_start(&raw)
        } else {
            gate_with_marker(&raw)
        };
        codex_preserved += ensure_codex_hook_entry(hooks_map, event_key, &command)?;
    }
    Ok(codex_preserved)
}

/// Validate the existing Claude Code `settings` shape and append our gated hook
/// entries for every event, without clobbering pre-existing entries (the user's
/// or another tool's). Operates on an in-memory value so it can be unit-tested
/// without touching disk. Returns the count of pre-existing non-aiki entries
/// preserved across all events.
///
/// Errors (rather than silently overwriting) if the top-level `hooks` value is
/// present with an unexpected JSON shape, or if any event array is malformed.
fn merge_claude_hooks(settings: &mut serde_json::Value) -> Result<usize> {
    // The root must be a JSON object (or null/absent = new file). A concrete
    // non-object root is a hand-edited shape we don't understand: refuse to
    // overwrite rather than panicking on the `settings["hooks"] = ...`
    // Index-assign below (serde_json's IndexMut only auto-vivifies Null, and
    // panics on array/string/number/bool).
    match &*settings {
        serde_json::Value::Object(_) | serde_json::Value::Null => {}
        other => anyhow::bail!(
            "~/.claude/settings.json root is not an object (found {}); refusing to overwrite. Fix or remove it and re-run.",
            json_type_name(other)
        ),
    }

    // `hooks` must be an object. Absent or null is fine (create fresh); any
    // other concrete type means a hand-edited shape we don't understand, so
    // refuse to overwrite it. Checking here also guarantees the Index-assign in
    // ensure_claude_hook_entry has an object parent and cannot panic.
    match settings.get("hooks") {
        None | Some(serde_json::Value::Null) => {
            settings["hooks"] = json!({});
        }
        Some(serde_json::Value::Object(_)) => {}
        Some(other) => anyhow::bail!(
            "~/.claude/settings.json hooks is not an object (found {}); refusing to overwrite. Fix or remove it and re-run.",
            json_type_name(other)
        ),
    }

    // Tool matcher for Pre/PostToolUse hooks (covers all file, shell, web, and MCP tools)
    let tool_matcher =
        "Edit|Write|MultiEdit|NotebookEdit|Read|Glob|Grep|LS|Bash|WebFetch|WebSearch|mcp__.*";

    // Append our hook entry for each event without clobbering pre-existing
    // entries (the user's or another tool's). Empty matcher matches all
    // sources (startup, resume, compact, clear); the tool matcher scopes
    // Pre/PostToolUse. Idempotent: re-running detects our entry and skips it.
    let claude_hooks: [(&str, &str, u64); 7] = [
        ("SessionStart", "", 10),
        ("PreCompact", "", 10),
        ("UserPromptSubmit", "", 10),
        ("PreToolUse", tool_matcher, 5),
        ("PostToolUse", tool_matcher, 5),
        ("Stop", "", 5),
        ("SessionEnd", "", 5),
    ];

    let mut preserved = 0usize;
    for (event, matcher, timeout) in claude_hooks {
        // Use bare "aiki" so hooks resolve via PATH, not a stale absolute path
        // (e.g. a workspace temp binary that no longer exists).
        let raw = format!("aiki hooks stdin --claude {}", event);
        // SessionStart gates on `.aiki/` only (so Dormant repos still reach the
        // Rust gate for the discovery signal); every other event also requires
        // the per-user marker.
        let command = if event == "SessionStart" {
            gate_session_start(&raw)
        } else {
            gate_with_marker(&raw)
        };
        preserved += ensure_claude_hook_entry(settings, event, matcher, &command, timeout)?;
    }
    Ok(preserved)
}

pub fn install_claude_code_hooks_global() -> Result<()> {
    let home_dir = dirs::home_dir().context("Could not find home directory")?;
    let settings_path = home_dir.join(".claude/settings.json");

    // Create ~/.claude if it doesn't exist
    if let Some(parent) = settings_path.parent() {
        fs::create_dir_all(parent).context("Failed to create ~/.claude directory")?;
    }

    // Load existing settings or create new
    let mut settings: serde_json::Value = if settings_path.exists() {
        let content =
            fs::read_to_string(&settings_path).context("Failed to read ~/.claude/settings.json")?;
        serde_json::from_str(&content).context("Failed to parse ~/.claude/settings.json")?
    } else {
        json!({})
    };

    let preserved = merge_claude_hooks(&mut settings)?;

    // Write updated settings
    let content =
        serde_json::to_string_pretty(&settings).context("Failed to serialize settings.json")?;
    fs::write(&settings_path, content).context("Failed to write ~/.claude/settings.json")?;

    println!(
        "✓ Installed Claude Code hooks at {}",
        settings_path.display()
    );
    if preserved > 0 {
        println!(
            "  - Preserved {} pre-existing hook entr{} from your config",
            preserved,
            if preserved == 1 { "y" } else { "ies" }
        );
    }
    println!("  - SessionStart: Auto-initialize repositories, context re-injection");
    println!("  - PreCompact: Pre-compaction state persistence");
    println!("  - UserPromptSubmit: Track turn start");
    println!("  - PreToolUse: Track tool permissions");
    println!("  - PostToolUse: Track AI-assisted changes");
    println!("  - Stop: Track turn completion");
    println!("  - SessionEnd: Track session termination");

    Ok(())
}

/// Install global Cursor hooks in ~/.cursor/hooks.json
pub fn install_cursor_hooks_global() -> Result<()> {
    let home_dir = dirs::home_dir().context("Could not find home directory")?;
    let hooks_path = home_dir.join(".cursor/hooks.json");
    // Use bare "aiki" so hooks resolve via PATH, not a stale absolute path.
    let aiki_path = "aiki";

    // Create ~/.cursor if it doesn't exist
    if let Some(parent) = hooks_path.parent() {
        fs::create_dir_all(parent).context("Failed to create ~/.cursor directory")?;
    }

    // Read existing hooks or create new
    let mut hooks: serde_json::Value = if hooks_path.exists() {
        let content =
            fs::read_to_string(&hooks_path).context("Failed to read ~/.cursor/hooks.json")?;
        serde_json::from_str(&content).context("Failed to parse ~/.cursor/hooks.json")?
    } else {
        json!({
            "version": 1,
            "hooks": {}
        })
    };

    // Ensure hooks object exists
    if hooks.get("hooks").is_none() {
        hooks["hooks"] = json!({});
    }

    // Cursor has no SessionStart event (beforeSubmitPrompt fires every turn), so
    // every hook gets the full marker gate — a Dormant repo simply never invokes
    // aiki under Cursor. `aiki_path` is bare "aiki" so it resolves via PATH.
    let cursor_hooks = [
        "beforeSubmitPrompt",
        "afterFileEdit",
        "beforeShellExecution",
        "afterShellExecution",
        "beforeMCPExecution",
        "afterMCPExecution",
        "stop",
        "sessionEnd",
    ];

    for hook_name in cursor_hooks {
        let raw = format!("{} hooks stdin --cursor {}", aiki_path, hook_name);
        ensure_cursor_hook_entry(&mut hooks, hook_name, &gate_with_marker(&raw));
    }

    // Write updated hooks
    let content = serde_json::to_string_pretty(&hooks).context("Failed to serialize hooks.json")?;
    fs::write(&hooks_path, content).context("Failed to write ~/.cursor/hooks.json")?;

    println!("✓ Installed Cursor hooks at {}", hooks_path.display());
    println!("  - beforeSubmitPrompt: Track turn start");
    println!("  - afterFileEdit: Track AI-assisted changes");
    println!("  - beforeShellExecution: Track shell permissions");
    println!("  - afterShellExecution: Track shell completions");
    println!("  - beforeMCPExecution: Track MCP permissions");
    println!("  - afterMCPExecution: Track MCP completions");
    println!("  - stop: Track turn completion");
    println!("  - sessionEnd: Track session termination");

    Ok(())
}

/// Install global Codex hooks in ~/.codex/config.toml
///
/// Adds OTel receiver config in `~/.codex/config.toml` and native Codex hook
/// definitions in `~/.codex/hooks.json`.
///
/// The exporter field is a tagged enum in codex's config:
/// - Unit variants: "none", "statsig"
/// - Struct variants: { "otlp-http": { endpoint, protocol } }
///
/// If [otel] already exists with a different exporter endpoint, warns but doesn't overwrite.
/// log_user_prompt is always safe to set/update regardless of existing config.
/// Write the Codex hooks feature flag, migrating off the deprecated name.
///
/// Codex renamed `[features].codex_hooks` to `[features].hooks`. This inserts the
/// new flag and removes any legacy one in the same pass, so a re-init silently
/// migrates existing users and clears Codex's deprecation warning. Idempotent.
fn enable_codex_hooks_feature(
    config_table: &mut toml::map::Map<String, toml::Value>,
) -> Result<()> {
    let features = config_table
        .entry("features")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .context("features section is not a table")?;
    features.insert("hooks".to_string(), toml::Value::Boolean(true));
    features.remove("codex_hooks"); // migrate off the deprecated flag name
    Ok(())
}

pub fn install_codex_hooks_global() -> Result<()> {
    let home_dir = dirs::home_dir().context("Could not find home directory")?;
    let config_path = home_dir.join(".codex/config.toml");

    // Create ~/.codex if it doesn't exist
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).context("Failed to create ~/.codex directory")?;
    }

    // Read existing config or create new
    let mut config: toml::Value = if config_path.exists() {
        let content =
            fs::read_to_string(&config_path).context("Failed to read ~/.codex/config.toml")?;
        toml::from_str(&content).context("Failed to parse ~/.codex/config.toml")?
    } else {
        toml::Value::Table(toml::map::Map::new())
    };

    let config_table = config
        .as_table_mut()
        .context("Config root is not a table")?;

    // Configure [otel] section
    // Codex's OtelExporterKind is a tagged enum:
    //   "none" | "statsig" (unit variants)
    //   { "otlp-http": { endpoint, protocol, ... } } (struct variant)
    // So we must write: [otel.exporter.otlp-http] with endpoint/protocol inside
    let aiki_endpoint = "http://127.0.0.1:19876/v1/logs";

    let existing_otel = config_table.get("otel").and_then(|v| v.as_table()).cloned();

    if let Some(ref otel) = existing_otel {
        // [otel] already exists - check if exporter is compatible
        let existing_endpoint = get_otlp_http_endpoint(otel);

        if let Some(ref ep) = existing_endpoint {
            if ep != aiki_endpoint {
                // Different endpoint: warn and only update log_user_prompt + disable traces
                eprintln!(
                    "⚠️  [otel.exporter.otlp-http] already has endpoint = \"{}\"\n   Aiki's OTel receiver listens on {}",
                    ep, aiki_endpoint
                );
                eprintln!("   To use aiki, update your endpoint to: {}", aiki_endpoint);

                if let Some(otel) = config_table.get_mut("otel").and_then(|v| v.as_table_mut()) {
                    otel.insert(
                        "trace_exporter".to_string(),
                        toml::Value::String("none".to_string()),
                    );
                    otel.insert("log_user_prompt".to_string(), toml::Value::Boolean(true));
                }
            } else {
                // Same endpoint: ensure trace_exporter is disabled and log_user_prompt is set
                if let Some(otel) = config_table.get_mut("otel").and_then(|v| v.as_table_mut()) {
                    otel.insert(
                        "trace_exporter".to_string(),
                        toml::Value::String("none".to_string()),
                    );
                    otel.insert("log_user_prompt".to_string(), toml::Value::Boolean(true));
                }
            }
        } else if otel.get("exporter").and_then(|v| v.as_str()).is_some() {
            // Has exporter as a unit variant (e.g., "none" or "statsig") - replace with our struct
            if let Some(otel) = config_table.get_mut("otel").and_then(|v| v.as_table_mut()) {
                otel.insert(
                    "exporter".to_string(),
                    build_otlp_http_exporter(aiki_endpoint),
                );
                otel.insert(
                    "trace_exporter".to_string(),
                    toml::Value::String("none".to_string()),
                );
                otel.insert("log_user_prompt".to_string(), toml::Value::Boolean(true));
                // Remove legacy flat fields if present from old aiki versions
                otel.remove("endpoint");
                otel.remove("protocol");
            }
        } else {
            // No exporter configured: add our struct variant
            if let Some(otel) = config_table.get_mut("otel").and_then(|v| v.as_table_mut()) {
                otel.insert(
                    "exporter".to_string(),
                    build_otlp_http_exporter(aiki_endpoint),
                );
                otel.insert(
                    "trace_exporter".to_string(),
                    toml::Value::String("none".to_string()),
                );
                otel.insert("log_user_prompt".to_string(), toml::Value::Boolean(true));
                // Remove legacy flat fields if present from old aiki versions
                otel.remove("endpoint");
                otel.remove("protocol");
            }
        }
    } else {
        // No [otel] section: create with aiki's full defaults
        let mut otel_table = toml::map::Map::new();
        // Enable log exporter (semantic events like codex.user_prompt, codex.tool_result)
        // exporter is a tagged enum: { otlp-http = { endpoint, protocol } }
        otel_table.insert(
            "exporter".to_string(),
            build_otlp_http_exporter(aiki_endpoint),
        );
        // Disable trace exporter (we only want logs, not distributed tracing spans)
        otel_table.insert(
            "trace_exporter".to_string(),
            toml::Value::String("none".to_string()),
        );
        otel_table.insert("log_user_prompt".to_string(), toml::Value::Boolean(true));
        config_table.insert("otel".to_string(), toml::Value::Table(otel_table));
    }

    // Remove legacy notify config if present
    config_table.remove("notify");

    // Enable the hooks feature (off by default in Codex), migrating off the
    // deprecated `codex_hooks` flag name.
    enable_codex_hooks_feature(config_table)?;

    // Codex native hooks inherit the session sandbox. Add ~/.aiki so hook
    // handlers can write global session state under workspace-write mode.
    ensure_codex_writable_root(config_table)?;

    // Write updated config atomically to prevent corruption from concurrent
    // `aiki init` calls (e.g. multiple agent sessions starting at once).
    let content = toml::to_string_pretty(&config).context("Failed to serialize config.toml")?;
    atomic_write_file(&config_path, content.as_bytes())
        .context("Failed to write ~/.codex/config.toml")?;

    // Write hooks.json where Codex discovers native hook definitions.
    let hooks_path = home_dir.join(".codex/hooks.json");

    // Read the existing hooks.json so we append-not-overwrite, preserving any
    // pre-existing entries from the user or other tools. A malformed file errors
    // rather than being silently clobbered.
    let mut hooks_json: serde_json::Value = if hooks_path.exists() {
        let content =
            fs::read_to_string(&hooks_path).context("Failed to read ~/.codex/hooks.json")?;
        serde_json::from_str(&content).context("Failed to parse ~/.codex/hooks.json")?
    } else {
        serde_json::json!({ "hooks": {} })
    };

    let codex_preserved = merge_codex_hooks(&mut hooks_json)?;

    let hooks_content =
        serde_json::to_string_pretty(&hooks_json).context("Failed to serialize hooks.json")?;
    atomic_write_file(&hooks_path, hooks_content.as_bytes())
        .context("Failed to write ~/.codex/hooks.json")?;

    println!("✓ Installed Codex config at {}", config_path.display());
    println!("  - [otel.exporter]: Log events → {}", aiki_endpoint);
    println!("  - [otel.trace_exporter]: Disabled (no trace spans)");
    println!("  - [sandbox_workspace_write]: writable_roots includes ~/.aiki");
    println!("  - log_user_prompt: true (prompt content capture enabled)");
    println!("✓ Installed Codex hooks at {}", hooks_path.display());
    if codex_preserved > 0 {
        println!(
            "  - Preserved {} pre-existing hook entr{} from your config",
            codex_preserved,
            if codex_preserved == 1 { "y" } else { "ies" }
        );
    }
    println!(
        "  - SessionStart, UserPromptSubmit, PreToolUse, Stop"
    );

    Ok(())
}

/// Build the exporter struct variant for otlp-http
///
/// Produces a TOML table representing:
/// ```toml
/// [otel.exporter.otlp-http]
/// endpoint = "..."
/// protocol = "binary"
/// ```
fn build_otlp_http_exporter(endpoint: &str) -> toml::Value {
    let mut otlp_http = toml::map::Map::new();
    otlp_http.insert(
        "endpoint".to_string(),
        toml::Value::String(endpoint.to_string()),
    );
    otlp_http.insert(
        "protocol".to_string(),
        toml::Value::String("binary".to_string()),
    );

    let mut exporter = toml::map::Map::new();
    exporter.insert("otlp-http".to_string(), toml::Value::Table(otlp_http));
    toml::Value::Table(exporter)
}

/// Extract the endpoint from an existing [otel.exporter.otlp-http] struct variant
fn get_otlp_http_endpoint(otel: &toml::map::Map<String, toml::Value>) -> Option<String> {
    otel.get("exporter")
        .and_then(|v| v.as_table())
        .and_then(|exp| exp.get("otlp-http"))
        .and_then(|v| v.as_table())
        .and_then(|http| http.get("endpoint"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Ensure ~/.aiki is writable inside Codex's workspace-write sandbox.
///
/// Codex native hooks execute inside the session sandbox, so they need the
/// global Aiki directory added as an extra writable root in order to create
/// session files and update the global JJ repo.
fn ensure_codex_writable_root(
    config_table: &mut toml::map::Map<String, toml::Value>,
) -> Result<()> {
    let global_aiki = crate::global::global_aiki_dir();
    let global_aiki = global_aiki
        .to_str()
        .context("Global aiki directory contains invalid UTF-8")?
        .to_string();

    let sandbox = config_table
        .entry("sandbox_workspace_write".to_string())
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));

    let sandbox_table = sandbox
        .as_table_mut()
        .context("sandbox_workspace_write must be a table")?;

    let writable_roots = sandbox_table
        .entry("writable_roots".to_string())
        .or_insert_with(|| toml::Value::Array(Vec::new()));

    let writable_roots = writable_roots
        .as_array_mut()
        .context("sandbox_workspace_write.writable_roots must be an array")?;

    let already_present = writable_roots
        .iter()
        .any(|v| v.as_str().is_some_and(|s| s == global_aiki));

    if !already_present {
        writable_roots.push(toml::Value::String(global_aiki));
    }

    Ok(())
}

/// Write a file atomically by writing to a temp file then renaming.
///
/// `rename()` is atomic on POSIX — the target is either the old content or the
/// new content, never a partial mix. This prevents corruption when multiple
/// processes write the same config file concurrently.
fn atomic_write_file(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("Cannot write to a file with no parent directory")?;

    let tmp_path = parent.join(format!(
        ".{}.tmp.{}.{:?}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("config"),
        std::process::id(),
        std::thread::current().id(),
    ));

    // Write to temp file, fsync, then rename
    let mut file = fs::File::create(&tmp_path).context("Failed to create temporary config file")?;
    file.write_all(content)
        .context("Failed to write temporary config file")?;
    file.sync_all()
        .context("Failed to sync temporary config file")?;
    drop(file);

    fs::rename(&tmp_path, path).context("Failed to rename temporary config file")?;
    Ok(())
}

// ===========================================================================
// `aiki remove --global` — machine-wide teardown (Phase C)
// ===========================================================================

/// True if a hook entry invokes aiki, in either the nested Claude/Codex shape
/// (`{ "hooks": [{ "command": "..." }] }`) or the flat Cursor shape
/// (`{ "command": "..." }`). Recognizes gated, bare, `.exe`, and path-prefixed
/// commands via the tokenized predicate.
fn entry_is_aiki_owned_any_shape(entry: &serde_json::Value) -> bool {
    if let Some(cmd) = entry.get("command").and_then(|c| c.as_str()) {
        if command_invokes_aiki_hook(cmd) {
            return true;
        }
    }
    entry_has_aiki_command(entry)
}

/// Strip every aiki-owned hook entry from a JSON hooks file (Claude/Codex/Cursor
/// all use a top-level `hooks` map of `event -> [entry]`). Preserves non-aiki
/// entries, drops arrays we empty, and removes the file if `hooks` ends up empty
/// and was the only key. Returns true if the file changed. No-op if absent.
fn remove_aiki_hooks_from_file(path: &Path, label: &str) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let content = fs::read_to_string(path).with_context(|| format!("Failed to read {label}"))?;
    let mut value: serde_json::Value =
        serde_json::from_str(&content).with_context(|| format!("Failed to parse {label}"))?;

    let Some(hooks) = value.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return Ok(false);
    };

    let keys: Vec<String> = hooks.keys().cloned().collect();
    let mut changed = false;
    let mut emptied = Vec::new();
    for key in &keys {
        if let Some(arr) = hooks.get_mut(key).and_then(|v| v.as_array_mut()) {
            let before = arr.len();
            arr.retain(|e| !entry_is_aiki_owned_any_shape(e));
            if arr.len() != before {
                changed = true;
                if arr.is_empty() {
                    emptied.push(key.clone());
                }
            }
        }
    }
    for key in emptied {
        hooks.remove(&key);
    }

    if changed {
        let pretty = serde_json::to_string_pretty(&value)
            .with_context(|| format!("Failed to serialize {label}"))?;
        fs::write(path, pretty).with_context(|| format!("Failed to write {label}"))?;
    }
    Ok(changed)
}

/// Remove aiki hook entries from `~/.claude/settings.json`.
pub fn remove_claude_code_hooks_global() -> Result<bool> {
    let home = dirs::home_dir().context("Could not find home directory")?;
    remove_aiki_hooks_from_file(&home.join(".claude/settings.json"), "~/.claude/settings.json")
}

/// Remove aiki hook entries from `~/.cursor/hooks.json`.
pub fn remove_cursor_hooks_global() -> Result<bool> {
    let home = dirs::home_dir().context("Could not find home directory")?;
    remove_aiki_hooks_from_file(&home.join(".cursor/hooks.json"), "~/.cursor/hooks.json")
}

/// Remove aiki hook entries from `~/.codex/hooks.json`.
pub fn remove_codex_hooks_global() -> Result<bool> {
    let home = dirs::home_dir().context("Could not find home directory")?;
    remove_aiki_hooks_from_file(&home.join(".codex/hooks.json"), "~/.codex/hooks.json")
}

/// Stop and remove the OTel receiver service (the inverse of
/// [`install_otel_receiver`]). Best-effort: never errors on a missing service.
pub fn uninstall_otel_receiver() -> Result<()> {
    let home = dirs::home_dir().context("Could not find home directory")?;
    match std::env::consts::OS {
        "macos" => {
            let target = format!("gui/{}/com.aiki.otel-receive", unsafe { libc::getuid() });
            let _ = Command::new("launchctl").args(["bootout", &target]).output();
            let plist = home.join("Library/LaunchAgents/com.aiki.otel-receive.plist");
            let _ = fs::remove_file(plist);
        }
        "linux" => {
            let _ = Command::new("systemctl")
                .args(["--user", "disable", "--now", "aiki-otel-receive.socket"])
                .output();
            let unit_dir = home.join(".config/systemd/user");
            let _ = fs::remove_file(unit_dir.join("aiki-otel-receive.socket"));
            let _ = fs::remove_file(unit_dir.join("aiki-otel-receive.service"));
        }
        _ => {}
    }
    Ok(())
}

/// Install the OTel receiver as a socket-activated service.
///
/// On macOS: installs a launchd plist to ~/Library/LaunchAgents/
/// On Linux: installs systemd user units to ~/.config/systemd/user/
/// On other platforms: returns Ok(()) with a warning printed.
///
/// The binary path in the template is substituted with the actual aiki binary location.
pub fn install_otel_receiver() -> Result<()> {
    let aiki_path = get_aiki_binary_path();

    match std::env::consts::OS {
        "macos" => install_otel_receiver_macos(&aiki_path),
        "linux" => install_otel_receiver_linux(&aiki_path),
        other => {
            eprintln!(
                "⚠ OTel receiver socket activation not supported on {} yet",
                other
            );
            Ok(())
        }
    }
}

/// Check if the OTel receiver is already installed (unit files exist).
pub fn is_otel_receiver_installed() -> bool {
    let home_dir = match dirs::home_dir() {
        Some(h) => h,
        None => return false,
    };

    match std::env::consts::OS {
        "macos" => home_dir
            .join("Library/LaunchAgents/com.aiki.otel-receive.plist")
            .exists(),
        "linux" => home_dir
            .join(".config/systemd/user/aiki-otel-receive.socket")
            .exists(),
        _ => false,
    }
}

/// Restart the OTel receiver. If not installed, falls back to install.
pub fn restart_otel_receiver() -> Result<()> {
    if !is_otel_receiver_installed() {
        return install_otel_receiver();
    }

    match std::env::consts::OS {
        "macos" => restart_otel_receiver_macos(),
        "linux" => restart_otel_receiver_linux(),
        other => {
            eprintln!("⚠ OTel receiver restart not supported on {} yet", other);
            Ok(())
        }
    }
}

/// Wait for the OTel receiver socket to become ready (up to ~2s).
/// Returns Ok if the socket is listening, Err if it times out.
pub fn wait_for_otel_receiver() -> Result<()> {
    let addr: SocketAddr = "127.0.0.1:19876".parse().unwrap();
    for _ in 0..10 {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
            return Ok(());
        }
    }
    anyhow::bail!("OTel receiver did not become ready within 2 seconds")
}

fn restart_otel_receiver_macos() -> Result<()> {
    let home_dir = dirs::home_dir().context("Could not find home directory")?;
    let plist_path = home_dir.join("Library/LaunchAgents/com.aiki.otel-receive.plist");
    let domain_target = format!("gui/{}", unsafe { libc::getuid() });
    let service_target = format!("{}/com.aiki.otel-receive", domain_target);

    // Bootout (stop) — ignore errors, may not be loaded
    let _ = Command::new("launchctl")
        .args(["bootout", &service_target])
        .output();

    // Clear the disabled override left by any prior `launchctl unload -w`.
    // Without this, bootstrap fails with EIO (error 5).
    let _ = Command::new("launchctl")
        .args(["enable", &service_target])
        .output();

    // Bootstrap (start)
    let output = Command::new("launchctl")
        .args(["bootstrap", &domain_target])
        .arg(&plist_path)
        .output()
        .context("Failed to run launchctl bootstrap")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("launchctl bootstrap failed: {}", stderr.trim());
    }

    Ok(())
}

fn restart_otel_receiver_linux() -> Result<()> {
    let output = Command::new("systemctl")
        .args(["--user", "restart", "aiki-otel-receive.socket"])
        .output()
        .context("Failed to run systemctl --user restart")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("systemctl restart failed: {}", stderr.trim());
    }

    Ok(())
}

fn install_otel_receiver_macos(aiki_path: &str) -> Result<()> {
    let home_dir = dirs::home_dir().context("Could not find home directory")?;
    let agents_dir = home_dir.join("Library/LaunchAgents");
    let plist_path = agents_dir.join("com.aiki.otel-receive.plist");
    let domain_target = format!("gui/{}", unsafe { libc::getuid() });
    let service_target = format!("{}/com.aiki.otel-receive", domain_target);

    fs::create_dir_all(&agents_dir).context("Failed to create ~/Library/LaunchAgents")?;

    // Bootout existing if present (ignore errors — may not be loaded)
    if plist_path.exists() {
        let _ = Command::new("launchctl")
            .args(["bootout", &service_target])
            .output();
    }

    let plist_content = generate_launchd_plist(aiki_path);
    fs::write(&plist_path, &plist_content).context("Failed to write launchd plist")?;

    // Clear the disabled override left by any prior `launchctl unload -w`.
    // Without this, bootstrap fails with EIO (error 5).
    let _ = Command::new("launchctl")
        .args(["enable", &service_target])
        .output();

    // Bootstrap the agent (registers plist + activates socket)
    let output = Command::new("launchctl")
        .args(["bootstrap", &domain_target])
        .arg(&plist_path)
        .output()
        .context("Failed to run launchctl bootstrap")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("launchctl bootstrap failed: {}", stderr.trim());
    }

    Ok(())
}

fn install_otel_receiver_linux(aiki_path: &str) -> Result<()> {
    let home_dir = dirs::home_dir().context("Could not find home directory")?;
    let user_units_dir = home_dir.join(".config/systemd/user");

    fs::create_dir_all(&user_units_dir).context("Failed to create ~/.config/systemd/user")?;

    let socket_path = user_units_dir.join("aiki-otel-receive.socket");
    let service_path = user_units_dir.join("aiki-otel-receive@.service");

    let socket_content = generate_systemd_socket();
    let service_content = generate_systemd_service(aiki_path);

    fs::write(&socket_path, &socket_content).context("Failed to write systemd socket unit")?;
    fs::write(&service_path, &service_content).context("Failed to write systemd service unit")?;

    // Reload and enable
    let _ = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output();

    let output = Command::new("systemctl")
        .args(["--user", "enable", "--now", "aiki-otel-receive.socket"])
        .output()
        .context("Failed to run systemctl --user enable")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("systemctl enable failed: {}", stderr.trim());
    }

    Ok(())
}

fn generate_launchd_plist(aiki_path: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.aiki.otel-receive</string>

    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
        <string>hooks</string>
        <string>otel</string>
        <string>--agent</string>
        <string>codex</string>
    </array>

    <!-- Socket activation: pass incoming connection as stdin -->
    <key>Sockets</key>
    <dict>
        <key>Listeners</key>
        <dict>
            <key>SockServiceName</key>
            <string>19876</string>
            <key>SockNodeName</key>
            <string>127.0.0.1</string>
            <key>SockType</key>
            <string>stream</string>
        </dict>
    </dict>

    <!-- inetd-style: stdin/stdout are the socket -->
    <key>inetdCompatibility</key>
    <dict>
        <key>Wait</key>
        <false/>
    </dict>

    <!-- Enable debug logging for diagnostics -->
    <key>EnvironmentVariables</key>
    <dict>
        <key>AIKI_DEBUG</key>
        <string>1</string>
    </dict>

    <!-- Logging -->
    <key>StandardErrorPath</key>
    <string>/tmp/aiki-otel-receive.err</string>

    <!-- Process spawning settings -->
    <key>SessionCreate</key>
    <false/>

    <!-- Don't keep running - only launch on socket activation -->
    <key>KeepAlive</key>
    <false/>

    <key>RunAtLoad</key>
    <false/>
</dict>
</plist>
"#,
        aiki_path
    )
}

fn generate_systemd_socket() -> String {
    "[Unit]\n\
     Description=Aiki OTel Receiver Socket\n\
     \n\
     [Socket]\n\
     ListenStream=127.0.0.1:19876\n\
     Accept=yes\n\
     \n\
     [Install]\n\
     WantedBy=sockets.target\n"
        .to_string()
}

fn generate_systemd_service(aiki_path: &str) -> String {
    format!(
        "[Unit]\n\
         Description=Aiki OTel Receiver (per-connection instance)\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={} hooks otel --agent codex\n\
         StandardInput=socket\n\
         StandardOutput=socket\n\
         StandardError=journal\n",
        aiki_path
    )
}

/// Check if Claude Code is installed
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_previous_hooks_path_handles_not_set() {
        let temp_dir = tempfile::tempdir().unwrap();

        // Initialize git repo
        Command::new("git")
            .args(["init"])
            .current_dir(temp_dir.path())
            .output()
            .unwrap();

        // Create .aiki directory
        fs::create_dir_all(temp_dir.path().join(".aiki")).unwrap();

        // Save hooks path (should not create file when not set)
        let result = save_previous_hooks_path(temp_dir.path());
        assert!(result.is_ok());

        // Verify file does NOT exist (no custom hooks path to preserve)
        let previous_path_file = temp_dir.path().join(".aiki/.previous_hooks_path");
        assert!(
            !previous_path_file.exists(),
            "File should not exist when there's no custom hooks path configured"
        );
    }

    #[test]
    fn save_previous_hooks_path_handles_custom_path() {
        let temp_dir = tempfile::tempdir().unwrap();

        // Initialize git repo
        Command::new("git")
            .args(["init"])
            .current_dir(temp_dir.path())
            .output()
            .unwrap();

        // Set custom hooks path
        Command::new("git")
            .args(["config", "core.hooksPath", ".custom-hooks"])
            .current_dir(temp_dir.path())
            .output()
            .unwrap();

        // Create .aiki directory (minimal - only if needed)
        fs::create_dir_all(temp_dir.path().join(".aiki")).unwrap();

        // Save hooks path
        let result = save_previous_hooks_path(temp_dir.path());
        assert!(result.is_ok());

        // Verify file contents
        let previous_path_file = temp_dir.path().join(".aiki/.previous_hooks_path");
        assert!(previous_path_file.exists());
        let content = fs::read_to_string(&previous_path_file).unwrap();
        assert_eq!(content, ".custom-hooks");
    }

    #[test]
    fn atomic_write_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        atomic_write_file(&path, b"[hooks]\ncommand = true\n").unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content, "[hooks]\ncommand = true\n");
    }

    #[test]
    fn atomic_write_replaces_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        fs::write(&path, "old content that is longer than new").unwrap();
        atomic_write_file(&path, b"new").unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content, "new", "old content must not bleed through");
    }

    #[test]
    fn atomic_write_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        atomic_write_file(&path, b"content").unwrap();

        let temps: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(temps.is_empty(), "temp file should be cleaned up by rename");
    }

    #[test]
    fn atomic_write_concurrent_writers_never_corrupt() {
        use std::sync::{Arc, Barrier};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        // Seed the file so we can detect bleed-through from old content
        fs::write(&path, "x".repeat(4096)).unwrap();

        let num_writers = 20;
        let rounds = 50;
        let barrier = Arc::new(Barrier::new(num_writers));

        // Each thread writes a distinct, self-consistent payload.
        // After all writes, the file must contain exactly one payload — not a mix.
        std::thread::scope(|s| {
            for writer_id in 0..num_writers {
                let barrier = Arc::clone(&barrier);
                let path = path.clone();
                s.spawn(move || {
                    // All threads start together to maximize contention
                    barrier.wait();
                    for round in 0..rounds {
                        let tag = format!("w{writer_id}r{round}");
                        // Vary length to reproduce the short-write-over-long-write bug
                        let payload =
                            format!("[{tag}]\nkey = \"{}\"\n", tag.repeat(1 + (writer_id % 5)));
                        atomic_write_file(&path, payload.as_bytes()).unwrap();
                    }
                });
            }
        });

        // The file must be valid: it should start with `[` and be parseable,
        // and it must NOT contain leftover bytes from a different write.
        let final_content = fs::read_to_string(&path).unwrap();
        assert!(
            final_content.starts_with('['),
            "file should start with TOML section header, got: {:?}",
            &final_content[..final_content.len().min(40)]
        );
        // Parse to confirm it's valid TOML (not a mix of two writes)
        let parsed: Result<toml::Value, _> = toml::from_str(&final_content);
        assert!(
            parsed.is_ok(),
            "file must be valid TOML after concurrent writes, got error: {:?}\ncontent: {:?}",
            parsed.err(),
            &final_content[..final_content.len().min(200)]
        );
    }

    // --- Claude Code hook install: append-not-overwrite ---

    #[test]
    fn claude_appends_without_clobbering_existing() {
        // A pre-existing, non-aiki SessionStart hook must survive install.
        let mut settings = json!({
            "hooks": {
                "SessionStart": [{
                    "matcher": "",
                    "hooks": [{ "type": "command", "command": "other-tool do-thing", "timeout": 3 }]
                }]
            }
        });

        let preserved = ensure_claude_hook_entry(
            &mut settings,
            "SessionStart",
            "",
            "aiki hooks stdin --claude SessionStart",
            10,
        )
        .unwrap();

        assert_eq!(preserved, 1, "the foreign entry should be counted as preserved");
        let arr = settings["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(arr.len(), 2, "foreign entry kept, ours appended");
        assert!(
            arr.iter()
                .any(|e| e["hooks"][0]["command"].as_str() == Some("other-tool do-thing")),
            "foreign hook must be preserved"
        );
        assert!(arr.iter().any(entry_has_aiki_command), "our hook must be present");
    }

    #[test]
    fn claude_install_is_idempotent() {
        let mut settings = json!({ "hooks": {} });
        let cmd = "aiki hooks stdin --claude SessionStart";

        ensure_claude_hook_entry(&mut settings, "SessionStart", "", cmd, 10).unwrap();
        let preserved =
            ensure_claude_hook_entry(&mut settings, "SessionStart", "", cmd, 10).unwrap();

        assert_eq!(preserved, 0, "no foreign entries to preserve");
        let arr = settings["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(arr.len(), 1, "re-running must not duplicate our entry");
        assert_eq!(
            arr.iter().filter(|e| entry_has_aiki_command(e)).count(),
            1,
            "exactly one aiki entry"
        );
    }

    #[test]
    fn claude_install_upgrades_user_prompt_submit_timeout() {
        let mut settings = json!({
            "hooks": {
                "UserPromptSubmit": [{
                    "matcher": "",
                    "hooks": [{
                        "type": "command",
                        "command": "aiki hooks stdin --claude UserPromptSubmit",
                        "timeout": 5
                    }]
                }]
            }
        });

        merge_claude_hooks(&mut settings).unwrap();

        let entries = settings["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(entries.len(), 1, "the old aiki entry must be replaced");
        assert_eq!(entries[0]["hooks"][0]["timeout"].as_u64(), Some(10));
    }

    #[test]
    fn claude_creates_entry_with_correct_shape() {
        let mut settings = json!({ "hooks": {} });
        ensure_claude_hook_entry(
            &mut settings,
            "PreToolUse",
            "Edit|Write",
            "aiki hooks stdin --claude PreToolUse",
            5,
        )
        .unwrap();

        let entry = &settings["hooks"]["PreToolUse"][0];
        assert_eq!(entry["matcher"].as_str(), Some("Edit|Write"));
        assert_eq!(entry["hooks"][0]["type"].as_str(), Some("command"));
        assert_eq!(
            entry["hooks"][0]["command"].as_str(),
            Some("aiki hooks stdin --claude PreToolUse")
        );
        assert_eq!(entry["hooks"][0]["timeout"].as_u64(), Some(5));
    }

    #[test]
    fn claude_detects_old_format_aiki_entry_as_ours() {
        // An aiki entry left over in the deprecated --agent/--event format still
        // contains "aiki hooks stdin", so we must NOT add a second entry. The
        // doctor's migration pass rewrites the format separately.
        let mut settings = json!({
            "hooks": {
                "SessionStart": [{
                    "matcher": "",
                    "hooks": [{
                        "type": "command",
                        "command": "aiki hooks stdin --agent claude-code --event SessionStart"
                    }]
                }]
            }
        });

        let preserved = ensure_claude_hook_entry(
            &mut settings,
            "SessionStart",
            "",
            "aiki hooks stdin --claude SessionStart",
            10,
        )
        .unwrap();

        assert_eq!(preserved, 0, "old-format aiki entry is ours, not foreign");
        let arr = settings["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(arr.len(), 1, "old-format aiki entry must not be duplicated");
    }

    // --- Codex hook install: read-existing + append-not-overwrite ---

    #[test]
    fn codex_appends_without_clobbering_existing() {
        let mut map = serde_json::Map::new();
        map.insert(
            "SessionStart".to_string(),
            json!([{ "hooks": [{ "type": "command", "command": "other-tool blah" }] }]),
        );

        let preserved =
            ensure_codex_hook_entry(&mut map, "SessionStart", "aiki hooks stdin --codex sessionStart")
                .unwrap();

        assert_eq!(preserved, 1);
        let arr = map.get("SessionStart").unwrap().as_array().unwrap();
        assert_eq!(arr.len(), 2, "foreign entry kept, ours appended");
        assert!(
            arr.iter()
                .any(|e| e["hooks"][0]["command"].as_str() == Some("other-tool blah")),
            "foreign hook must be preserved"
        );
        assert!(arr.iter().any(entry_has_aiki_command), "our hook must be present");
    }

    #[test]
    fn codex_install_is_idempotent() {
        let mut map = serde_json::Map::new();
        let cmd = "aiki hooks stdin --codex sessionStart";

        ensure_codex_hook_entry(&mut map, "SessionStart", cmd).unwrap();
        let preserved = ensure_codex_hook_entry(&mut map, "SessionStart", cmd).unwrap();

        assert_eq!(preserved, 0);
        assert_eq!(
            map.get("SessionStart").unwrap().as_array().unwrap().len(),
            1,
            "re-running must not duplicate our entry"
        );
    }

    #[test]
    fn codex_creates_entry_with_correct_shape() {
        let mut map = serde_json::Map::new();
        ensure_codex_hook_entry(&mut map, "PreToolUse", "aiki hooks stdin --codex preToolUse").unwrap();

        let arr = map.get("PreToolUse").unwrap().as_array().unwrap();
        let entry = &arr[0];
        assert_eq!(entry["hooks"][0]["type"].as_str(), Some("command"));
        assert_eq!(
            entry["hooks"][0]["command"].as_str(),
            Some("aiki hooks stdin --codex preToolUse")
        );
        // Codex entries carry no matcher (unlike Claude Code).
        assert!(entry.get("matcher").is_none());
    }

    // --- Malformed shapes: error rather than silently overwrite (plan line 156) ---

    #[test]
    fn claude_non_array_event_errors_and_preserves() {
        // hooks.SessionStart is an object, not the expected array. We must
        // refuse to touch it rather than overwrite the user's data.
        let mut settings = json!({
            "hooks": {
                "SessionStart": { "oops": "not an array" }
            }
        });
        let before = settings.clone();

        let result = ensure_claude_hook_entry(
            &mut settings,
            "SessionStart",
            "",
            "aiki hooks stdin --claude SessionStart",
            10,
        );

        assert!(result.is_err(), "non-array event must error");
        assert_eq!(settings, before, "original value must NOT be overwritten");
    }

    #[test]
    fn claude_non_object_hooks_errors_and_preserves() {
        // Top-level hooks is a string. merge_claude_hooks is the install
        // consumer path; it must reject the shape and leave it untouched.
        let mut settings = json!({ "hooks": "totally wrong" });
        let before = settings.clone();

        let result = merge_claude_hooks(&mut settings);

        assert!(result.is_err(), "non-object hooks must error");
        assert_eq!(settings, before, "original value must NOT be overwritten");
    }

    #[test]
    fn claude_merge_creates_hooks_when_absent() {
        // The consumer path still creates a fresh hooks object and adds our
        // entries when the file has no hooks field at all (empty-file case).
        let mut settings = json!({});
        let preserved = merge_claude_hooks(&mut settings).unwrap();

        assert_eq!(preserved, 0, "nothing pre-existing to preserve");
        assert!(settings["hooks"].is_object(), "hooks object created");
        assert!(
            settings["hooks"]["SessionStart"]
                .as_array()
                .map(|a| a.iter().any(entry_has_aiki_command))
                .unwrap_or(false),
            "our SessionStart entry was added"
        );
    }

    #[test]
    fn codex_non_array_event_errors_and_preserves() {
        // The SessionStart event is an object, not the expected array.
        let mut map = serde_json::Map::new();
        map.insert("SessionStart".to_string(), json!({ "oops": "not an array" }));
        let before = map.clone();

        let result = ensure_codex_hook_entry(
            &mut map,
            "SessionStart",
            "aiki hooks stdin --codex sessionStart",
        );

        assert!(result.is_err(), "non-array event must error");
        assert_eq!(map, before, "original value must NOT be overwritten");
    }

    #[test]
    fn codex_non_object_hooks_errors_and_preserves() {
        // Top-level hooks is a string. merge_codex_hooks is the install consumer
        // path; it must reject the shape and leave it untouched.
        let mut hooks_json = json!({ "hooks": "totally wrong" });
        let before = hooks_json.clone();

        let result = merge_codex_hooks(&mut hooks_json);

        assert!(result.is_err(), "non-object hooks must error");
        assert_eq!(hooks_json, before, "original value must NOT be overwritten");
    }

    #[test]
    fn codex_merge_creates_hooks_when_absent() {
        // The consumer path creates a fresh hooks object and adds our entries
        // when the value has no hooks field at all.
        let mut hooks_json = json!({});
        let preserved = merge_codex_hooks(&mut hooks_json).unwrap();

        assert_eq!(preserved, 0, "nothing pre-existing to preserve");
        let arr = hooks_json["hooks"]["SessionStart"].as_array().unwrap();
        assert!(
            arr.iter().any(entry_has_aiki_command),
            "our SessionStart entry was added"
        );
    }

    #[test]
    fn enable_codex_hooks_feature_migrates_legacy_flag() {
        // Consumer path: install_codex_hooks_global() calls this. A config
        // carrying the deprecated flag must come out with `hooks = true` and no
        // `codex_hooks` (which clears Codex's deprecation warning).
        let mut config: toml::Value =
            toml::from_str("[features]\ncodex_hooks = true\n").unwrap();
        enable_codex_hooks_feature(config.as_table_mut().unwrap()).unwrap();
        let features = config.get("features").and_then(|v| v.as_table()).unwrap();
        assert_eq!(features.get("hooks").and_then(|v| v.as_bool()), Some(true));
        assert!(
            features.get("codex_hooks").is_none(),
            "legacy codex_hooks flag must be removed"
        );
    }

    #[test]
    fn enable_codex_hooks_feature_on_fresh_config() {
        let mut config = toml::Value::Table(toml::map::Map::new());
        enable_codex_hooks_feature(config.as_table_mut().unwrap()).unwrap();
        assert_eq!(config["features"]["hooks"].as_bool(), Some(true));
        assert!(config["features"]
            .as_table()
            .unwrap()
            .get("codex_hooks")
            .is_none());
    }

    #[test]
    fn codex_hook_command_strings_are_frozen() {
        // Step 6 guard: Codex hook trust is hashed over the command string, so
        // any change silently re-prompts every user. Pin the raw command form.
        let mut hooks = json!({});
        merge_codex_hooks(&mut hooks).unwrap();
        for (event, arg) in [
            ("SessionStart", "sessionStart"),
            ("UserPromptSubmit", "userPromptSubmit"),
            ("PreToolUse", "preToolUse"),
            ("Stop", "stop"),
        ] {
            let cmd = hooks["hooks"][event][0]["hooks"][0]["command"]
                .as_str()
                .unwrap_or_else(|| panic!("missing command for {event}"));
            assert!(
                cmd.contains(&format!("aiki hooks stdin --codex {arg}")),
                "{event}: must contain the frozen `aiki hooks stdin --codex {arg}` form; got {cmd}"
            );
        }
    }

    // --- Malformed ROOT: non-object top-level value must error, not panic ---
    //
    // A syntactically valid file whose ROOT is an array/string/number/bool
    // (`[]`, `"bad"`, `42`, `true`) used to crash: `root.get("hooks")` returns
    // None for a non-object, so control fell into the create-fresh arm whose
    // `root["hooks"] = json!({})` Index-assign panics on any non-Null root
    // (serde_json's IndexMut only auto-vivifies Null). The root guard turns
    // that panic into an actionable "refusing to overwrite" error.

    #[test]
    fn claude_array_root_errors_and_does_not_panic() {
        let mut settings = json!([]);
        let before = settings.clone();

        let result = merge_claude_hooks(&mut settings);

        let err = result.expect_err("array root must error, not panic");
        let msg = err.to_string();
        assert!(
            msg.contains("root is not an object"),
            "message should name the root-shape problem: {msg}"
        );
        assert!(
            msg.contains("refusing to overwrite"),
            "message should refuse to overwrite: {msg}"
        );
        assert_eq!(settings, before, "array root must NOT be mutated");
    }

    #[test]
    fn claude_string_root_errors() {
        let mut settings = json!("bad");
        let result = merge_claude_hooks(&mut settings);
        assert!(result.is_err(), "string root must error");
    }

    #[test]
    fn codex_array_root_errors_and_does_not_panic() {
        let mut hooks_json = json!([]);
        let before = hooks_json.clone();

        let result = merge_codex_hooks(&mut hooks_json);

        let err = result.expect_err("array root must error, not panic");
        let msg = err.to_string();
        assert!(
            msg.contains("root is not an object"),
            "message should name the root-shape problem: {msg}"
        );
        assert!(
            msg.contains("refusing to overwrite"),
            "message should refuse to overwrite: {msg}"
        );
        assert_eq!(hooks_json, before, "array root must NOT be mutated");
    }

    #[test]
    fn codex_number_root_errors() {
        let mut hooks_json = json!(42);
        let result = merge_codex_hooks(&mut hooks_json);
        assert!(result.is_err(), "number root must error");
    }

    #[test]
    fn claude_null_root_installs_like_new_file() {
        // A null root auto-vivifies into an object on the Index-assign below,
        // matching the new-file path: it must succeed and install our entries.
        let mut settings = json!(null);
        let preserved = merge_claude_hooks(&mut settings).unwrap();

        assert_eq!(preserved, 0, "nothing pre-existing to preserve");
        assert!(settings["hooks"].is_object(), "hooks object created");
        assert!(
            settings["hooks"]["SessionStart"]
                .as_array()
                .map(|a| a.iter().any(entry_has_aiki_command))
                .unwrap_or(false),
            "our SessionStart entry was added"
        );
    }

    #[test]
    fn codex_null_root_installs_like_new_file() {
        let mut hooks_json = json!(null);
        let preserved = merge_codex_hooks(&mut hooks_json).unwrap();

        assert_eq!(preserved, 0, "nothing pre-existing to preserve");
        let arr = hooks_json["hooks"]["SessionStart"].as_array().unwrap();
        assert!(
            arr.iter().any(entry_has_aiki_command),
            "our SessionStart entry was added"
        );
    }

    // --- Shared aiki-hook detection predicate (command_invokes_aiki_hook) ---

    #[test]
    fn command_invokes_aiki_hook_matches_binary_forms() {
        // New flag format, plain binary.
        assert!(command_invokes_aiki_hook(
            "aiki hooks stdin --claude SessionStart"
        ));
        // Windows .exe binary, old --agent/--event format.
        assert!(command_invokes_aiki_hook(
            "aiki.exe hooks stdin --agent claude-code --event SessionStart"
        ));
        // Absolute path prefix.
        assert!(command_invokes_aiki_hook(
            "/usr/local/bin/aiki hooks stdin --claude Stop"
        ));
        // Windows path-prefixed .exe.
        assert!(command_invokes_aiki_hook(
            r"C:\path\aiki.exe hooks stdin --cursor stop"
        ));
        // Relative path prefix.
        assert!(command_invokes_aiki_hook(
            "./aiki hooks stdin --codex sessionStart"
        ));
    }

    #[test]
    fn command_invokes_aiki_hook_rejects_non_matches() {
        // Aiki binary, but not the hooks handler.
        assert!(!command_invokes_aiki_hook("aiki status"));
        // First word does not end with "aiki" (ends_with("aiki") is false).
        assert!(!command_invokes_aiki_hook("not-aiki-tool hooks stdin"));
        // Right binary and `hooks`, but the subcommand is not `stdin`.
        assert!(!command_invokes_aiki_hook("aiki hooks status"));
        // Bare binary with no hooks subcommand.
        assert!(!command_invokes_aiki_hook("aiki"));
    }

    // --- Idempotency for the Windows `.exe` form (review issues #2 & #3) ---
    //
    // `aiki.exe hooks stdin ...` does NOT contain the literal substring
    // `aiki hooks stdin` (it's `aiki.exe hooks stdin`), so the old substring
    // check missed it and re-init appended a SECOND aiki entry. The tokenized
    // predicate recognizes the `.exe` form as ours, restoring idempotency.

    #[test]
    fn claude_detects_exe_form_aiki_entry_as_ours() {
        let mut settings = json!({
            "hooks": {
                "SessionStart": [{
                    "matcher": "",
                    "hooks": [{
                        "type": "command",
                        "command": "aiki.exe hooks stdin --claude SessionStart"
                    }]
                }]
            }
        });

        let preserved = merge_claude_hooks(&mut settings).unwrap();

        assert_eq!(preserved, 0, "the .exe-form aiki entry is ours, not foreign");
        let arr = settings["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(arr.len(), 1, ".exe-form aiki entry must not be duplicated");
    }

    #[test]
    fn codex_detects_exe_form_aiki_entry_as_ours() {
        let mut map = serde_json::Map::new();
        map.insert(
            "SessionStart".to_string(),
            json!([{
                "hooks": [{
                    "type": "command",
                    "command": "aiki.exe hooks stdin --codex sessionStart"
                }]
            }]),
        );

        let preserved = ensure_codex_hook_entry(
            &mut map,
            "SessionStart",
            "aiki hooks stdin --codex sessionStart",
        )
        .unwrap();

        assert_eq!(preserved, 0, ".exe-form aiki entry is ours, not foreign");
        assert_eq!(
            map.get("SessionStart").unwrap().as_array().unwrap().len(),
            1,
            ".exe-form aiki entry must not be duplicated"
        );
    }

    #[test]
    fn cursor_exe_form_command_detected_as_aiki() {
        // Cursor uses a flat entry shape (command directly on the entry). Its
        // idempotency check now routes through command_invokes_aiki_hook, so a
        // pre-existing `.exe`-form beforeSubmitPrompt entry is recognized as
        // ours and not duplicated on re-install. Mirror that check shape here
        // (install_cursor_hooks_global touches disk and isn't unit-testable).
        let existing = json!([{
            "command": "aiki.exe hooks stdin --cursor beforeSubmitPrompt"
        }]);
        let already_installed = existing.as_array().unwrap().iter().any(|hook| {
            hook.get("command")
                .and_then(|c| c.as_str())
                .map(|c| command_invokes_aiki_hook(c))
                .unwrap_or(false)
        });
        assert!(
            already_installed,
            ".exe-form cursor command must be detected as ours"
        );
    }

    // --- Inline bash gate behaviour ---

    /// Render the gate with a sentinel dispatch command and run it under `shell`
    /// with the given `$PWD` and `$AIKI_HOME`. Returns trimmed stdout (the
    /// sentinel iff the gate let the command through; empty if it exited early).
    fn run_gate(shell: &str, gate: &str, pwd: &std::path::Path, aiki_home: &std::path::Path) -> String {
        let out = std::process::Command::new(shell)
            .arg("-c")
            .arg(gate)
            .env("PWD", pwd)
            .env("AIKI_HOME", aiki_home)
            .current_dir(pwd)
            .output()
            .expect("run gate");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn session_start_gate() -> String {
        gate_session_start("echo DISPATCH")
    }
    fn marker_gate() -> String {
        gate_with_marker("echo DISPATCH")
    }

    /// Create the per-user marker for `repo` under `aiki_home` using the same
    /// path the gate computes.
    fn touch_marker(aiki_home: &std::path::Path, repo: &std::path::Path) {
        let stripped = repo.strip_prefix("/").unwrap_or(repo);
        let marker = aiki_home.join(".init/repos").join(stripped).join("enabled");
        fs::create_dir_all(marker.parent().unwrap()).unwrap();
        fs::write(&marker, "").unwrap();
    }

    /// Run a gate check under every available POSIX shell so a future bashism
    /// surfaces as `Bad substitution` (an empty/garbled result), and assert the
    /// dispatch sentinel is present (or absent) as expected.
    fn assert_gate(gate: &str, pwd: &std::path::Path, home: &std::path::Path, expect_dispatch: bool) {
        for shell in ["sh", "dash"] {
            if std::process::Command::new(shell).arg("-c").arg("true").output().is_err() {
                continue; // shell not installed
            }
            let got = run_gate(shell, gate, pwd, home);
            assert_eq!(
                got == "DISPATCH",
                expect_dispatch,
                "{shell}: gate for {} expected dispatch={expect_dispatch}, got {got:?}",
                pwd.display()
            );
        }
    }

    #[test]
    fn gate_dispatches_in_active_repo_and_subdir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        let repo = root.join("repo");
        let sub = repo.join("a/b");
        fs::create_dir_all(repo.join(".aiki")).unwrap();
        fs::create_dir_all(&sub).unwrap();
        fs::create_dir_all(&home).unwrap();
        touch_marker(&home, &repo);

        // SessionStart gates on `.aiki/` only — dispatches from root and subdir.
        assert_gate(&session_start_gate(), &repo, &home, true);
        assert_gate(&session_start_gate(), &sub, &home, true);
        // Marker gate dispatches too (marker present).
        assert_gate(&marker_gate(), &repo, &home, true);
        assert_gate(&marker_gate(), &sub, &home, true);
    }

    #[test]
    fn gate_silent_in_non_aiki_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        let plain = root.join("plain");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&plain).unwrap();
        assert_gate(&session_start_gate(), &plain, &home, false);
        assert_gate(&marker_gate(), &plain, &home, false);
    }

    #[test]
    fn gate_silent_for_dormant_repo_on_marker_events() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        let repo = root.join("repo");
        fs::create_dir_all(repo.join(".aiki")).unwrap();
        fs::create_dir_all(&home).unwrap();
        // No marker → Dormant. SessionStart still dispatches (discovery signal),
        // but marker-gated events exit silently.
        assert_gate(&session_start_gate(), &repo, &home, true);
        assert_gate(&marker_gate(), &repo, &home, false);
    }

    #[test]
    fn gate_excludes_global_home_collision() {
        // A non-aiki project nested under a home dir that holds the global
        // `~/.aiki/` must NOT resolve to that home as a phantom root.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home/.aiki");
        fs::create_dir_all(home.join(".init/repos")).unwrap();
        let project = root.join("home/projects/foo");
        fs::create_dir_all(&project).unwrap();
        assert_gate(&session_start_gate(), &project, &home, false);
        assert_gate(&marker_gate(), &project, &home, false);
    }

    #[test]
    fn gate_handles_paths_with_spaces() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        let repo = root.join("My Stuff/the repo");
        fs::create_dir_all(repo.join(".aiki")).unwrap();
        fs::create_dir_all(&home).unwrap();
        touch_marker(&home, &repo);
        assert_gate(&marker_gate(), &repo, &home, true);
    }

    /// The marker path the bash gate computes (`$h/.init/repos$d/enabled`) must
    /// match the Rust `repos::marker_path` byte-for-byte (neither canonicalizes).
    #[test]
    fn gate_marker_path_matches_rust() {
        let home = std::path::Path::new("/tmp/h/.aiki");
        let repo = "/Users/me/code/repo";
        let bash = std::process::Command::new("sh")
            .arg("-c")
            .arg(r#"printf '%s' "$h/.init/repos$d/enabled""#)
            .env("h", home)
            .env("d", repo)
            .output()
            .unwrap();
        let bash_str = String::from_utf8(bash.stdout).unwrap();
        assert_eq!(bash_str, "/tmp/h/.aiki/.init/repos/Users/me/code/repo/enabled");
    }

    // --- Phase C: machine-wide hook removal ---

    #[test]
    fn remove_aiki_hooks_strips_nested_entries_keeps_foreign() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        // One aiki entry (gated form) + one foreign entry under SessionStart.
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "SessionStart": [
                        { "matcher": "", "hooks": [{ "command": "d=$PWD; exec aiki hooks stdin --claude SessionStart" }] },
                        { "matcher": "", "hooks": [{ "command": "other-tool go" }] }
                    ],
                    "Stop": [
                        { "matcher": "", "hooks": [{ "command": "aiki hooks stdin --claude Stop" }] }
                    ]
                }
            })).unwrap(),
        )
        .unwrap();

        let changed = remove_aiki_hooks_from_file(&path, "test").unwrap();
        assert!(changed);

        let after: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let ss = after["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(ss.len(), 1, "only the foreign entry remains");
        assert_eq!(ss[0]["hooks"][0]["command"].as_str(), Some("other-tool go"));
        // The Stop array was all-aiki, so it is dropped entirely.
        assert!(after["hooks"].get("Stop").is_none(), "emptied array removed");
    }

    #[test]
    fn remove_aiki_hooks_strips_flat_cursor_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hooks.json");
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "version": 1,
                "hooks": {
                    "beforeSubmitPrompt": [
                        { "command": "d=$PWD; exec aiki hooks stdin --cursor beforeSubmitPrompt" },
                        { "command": "someones-tool" }
                    ]
                }
            })).unwrap(),
        )
        .unwrap();

        assert!(remove_aiki_hooks_from_file(&path, "test").unwrap());

        let after: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let arr = after["hooks"]["beforeSubmitPrompt"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["command"].as_str(), Some("someones-tool"));
    }

    #[test]
    fn remove_aiki_hooks_no_op_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.json");
        assert!(!remove_aiki_hooks_from_file(&path, "test").unwrap());
    }
}
