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

/// Install global Claude Code hooks in ~/.claude/settings.json
pub fn install_claude_code_hooks_global() -> Result<()> {
    let home_dir = dirs::home_dir().context("Could not find home directory")?;
    let settings_path = home_dir.join(".claude/settings.json");
    // Use bare "aiki" so hooks resolve via PATH, not a stale absolute path
    // (e.g. a workspace temp binary that no longer exists).
    let aiki_path = "aiki";

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

    // Ensure hooks object exists
    if settings.get("hooks").is_none() {
        settings["hooks"] = json!({});
    }

    // Tool matcher for Pre/PostToolUse hooks (covers all file, shell, web, and MCP tools)
    let tool_matcher =
        "Edit|Write|MultiEdit|NotebookEdit|Read|Glob|Grep|LS|Bash|WebFetch|WebSearch|mcp__.*";

    // SessionStart hook for auto-initialization and context re-injection
    // Empty matcher matches all sources: startup, resume, compact, clear
    settings["hooks"]["SessionStart"] = json!([{
        "matcher": "",
        "hooks": [{
            "type": "command",
            "command": format!("{} hooks stdin --claude SessionStart", aiki_path),
            "timeout": 10
        }]
    }]);

    // PreCompact hook for pre-compaction state persistence (future use)
    settings["hooks"]["PreCompact"] = json!([{
        "matcher": "",
        "hooks": [{
            "type": "command",
            "command": format!("{} hooks stdin --claude PreCompact", aiki_path),
            "timeout": 10
        }]
    }]);

    // UserPromptSubmit hook for turn.started
    settings["hooks"]["UserPromptSubmit"] = json!([{
        "matcher": "",
        "hooks": [{
            "type": "command",
            "command": format!("{} hooks stdin --claude UserPromptSubmit", aiki_path),
            "timeout": 5
        }]
    }]);

    // PreToolUse hook for permission tracking
    settings["hooks"]["PreToolUse"] = json!([{
        "matcher": tool_matcher,
        "hooks": [{
            "type": "command",
            "command": format!("{} hooks stdin --claude PreToolUse", aiki_path),
            "timeout": 5
        }]
    }]);

    // PostToolUse hook for change tracking
    settings["hooks"]["PostToolUse"] = json!([{
        "matcher": tool_matcher,
        "hooks": [{
            "type": "command",
            "command": format!("{} hooks stdin --claude PostToolUse", aiki_path),
            "timeout": 5
        }]
    }]);

    // Stop hook for turn.completed
    settings["hooks"]["Stop"] = json!([{
        "matcher": "",
        "hooks": [{
            "type": "command",
            "command": format!("{} hooks stdin --claude Stop", aiki_path),
            "timeout": 5
        }]
    }]);

    // SessionEnd hook for session.ended
    settings["hooks"]["SessionEnd"] = json!([{
        "matcher": "",
        "hooks": [{
            "type": "command",
            "command": format!("{} hooks stdin --claude SessionEnd", aiki_path),
            "timeout": 5
        }]
    }]);

    // Write updated settings
    let content =
        serde_json::to_string_pretty(&settings).context("Failed to serialize settings.json")?;
    fs::write(&settings_path, content).context("Failed to write ~/.claude/settings.json")?;

    println!(
        "✓ Installed Claude Code hooks at {}",
        settings_path.display()
    );
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

    // beforeSubmitPrompt hook for auto-initialization
    let before_submit = hooks["hooks"]["beforeSubmitPrompt"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    let aiki_init_hook = json!({
        "command": format!("{} hooks stdin --cursor beforeSubmitPrompt", aiki_path)
    });

    // Check if already installed
    let init_already_installed = before_submit.iter().any(|hook| {
        hook.get("command")
            .and_then(|c| c.as_str())
            .map(|c| c.contains("aiki hooks stdin"))
            .unwrap_or(false)
    });

    if !init_already_installed {
        let mut new_hooks = before_submit;
        new_hooks.push(aiki_init_hook);
        hooks["hooks"]["beforeSubmitPrompt"] = json!(new_hooks);
    }

    // Install remaining Cursor hooks (afterFileEdit, stop, shell, MCP, sessionEnd)
    let additional_hooks = [
        ("afterFileEdit", "afterFileEdit"),
        ("beforeShellExecution", "beforeShellExecution"),
        ("afterShellExecution", "afterShellExecution"),
        ("beforeMCPExecution", "beforeMCPExecution"),
        ("afterMCPExecution", "afterMCPExecution"),
        ("stop", "stop"),
        ("sessionEnd", "sessionEnd"),
    ];

    for (hook_name, event_name) in &additional_hooks {
        let existing = hooks["hooks"][*hook_name]
            .as_array()
            .cloned()
            .unwrap_or_default();

        let aiki_hook = json!({
            "command": format!("{} hooks stdin --cursor {}", aiki_path, event_name)
        });

        let already_installed = existing.iter().any(|hook| {
            hook.get("command")
                .and_then(|c| c.as_str())
                .map(|c| c.contains("aiki hooks stdin"))
                .unwrap_or(false)
        });

        if !already_installed {
            let mut new_hooks = existing;
            new_hooks.push(aiki_hook);
            hooks["hooks"][*hook_name] = json!(new_hooks);
        }
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

    // Enable hooks feature (disabled by default in Codex)
    let features_table = config_table
        .entry("features")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .context("features section is not a table")?;
    features_table.insert(
        "codex_hooks".to_string(),
        toml::Value::Boolean(true),
    );

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
    let hook_events = [
        ("SessionStart", "sessionStart"),
        ("UserPromptSubmit", "userPromptSubmit"),
        ("PreToolUse", "preToolUse"),
        ("Stop", "stop"),
    ];

    let mut hooks_map = serde_json::Map::new();
    for (event_key, event_arg) in &hook_events {
        let hook_entry = serde_json::json!([{
            "hooks": [{
                "type": "command",
                "command": format!("aiki hooks stdin --codex {}", event_arg),
            }]
        }]);
        hooks_map.insert(event_key.to_string(), hook_entry);
    }

    let hooks_json = serde_json::json!({ "hooks": hooks_map });
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
}
