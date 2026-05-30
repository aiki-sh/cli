use anyhow::{Context, Result};
use serde_json::json;
use std::fs;
use std::path::Path;
use std::process::Command;

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
    let aiki_path = get_aiki_binary_path();

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
    let tool_matcher = "Edit|Write|MultiEdit|NotebookEdit|Read|Glob|Grep|LS|Bash|WebFetch|WebSearch|mcp__.*";

    // SessionStart hook for auto-initialization and context re-injection
    // Empty matcher matches all sources: startup, resume, compact, clear
    settings["hooks"]["SessionStart"] = json!([{
        "matcher": "",
        "hooks": [{
            "type": "command",
            "command": format!("{} hooks stdin --agent claude-code --event SessionStart", aiki_path),
            "timeout": 10
        }]
    }]);

    // PreCompact hook for pre-compaction state persistence (future use)
    settings["hooks"]["PreCompact"] = json!([{
        "matcher": "",
        "hooks": [{
            "type": "command",
            "command": format!("{} hooks stdin --agent claude-code --event PreCompact", aiki_path),
            "timeout": 10
        }]
    }]);

    // UserPromptSubmit hook for turn.started
    settings["hooks"]["UserPromptSubmit"] = json!([{
        "matcher": "",
        "hooks": [{
            "type": "command",
            "command": format!("{} hooks stdin --agent claude-code --event UserPromptSubmit", aiki_path),
            "timeout": 5
        }]
    }]);

    // PreToolUse hook for permission tracking
    settings["hooks"]["PreToolUse"] = json!([{
        "matcher": tool_matcher,
        "hooks": [{
            "type": "command",
            "command": format!("{} hooks stdin --agent claude-code --event PreToolUse", aiki_path),
            "timeout": 5
        }]
    }]);

    // PostToolUse hook for change tracking
    settings["hooks"]["PostToolUse"] = json!([{
        "matcher": tool_matcher,
        "hooks": [{
            "type": "command",
            "command": format!("{} hooks stdin --agent claude-code --event PostToolUse", aiki_path),
            "timeout": 5
        }]
    }]);

    // Stop hook for turn.completed
    settings["hooks"]["Stop"] = json!([{
        "matcher": "",
        "hooks": [{
            "type": "command",
            "command": format!("{} hooks stdin --agent claude-code --event Stop", aiki_path),
            "timeout": 5
        }]
    }]);

    // SessionEnd hook for session.ended
    settings["hooks"]["SessionEnd"] = json!([{
        "matcher": "",
        "hooks": [{
            "type": "command",
            "command": format!("{} hooks stdin --agent claude-code --event SessionEnd", aiki_path),
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
    let aiki_path = get_aiki_binary_path();

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
        "command": format!("{} hooks stdin --agent cursor --event beforeSubmitPrompt", aiki_path)
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
            "command": format!("{} hooks stdin --agent cursor --event {}", aiki_path, event_name)
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
/// Adds both OTel receiver config and notify command:
/// - [otel] section with exporter.otlp-http (struct variant) and log_user_prompt
/// - notify array with aiki hooks stdin command
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
    let aiki_path = get_aiki_binary_path();

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
                    otel.insert(
                        "log_user_prompt".to_string(),
                        toml::Value::Boolean(true),
                    );
                }
            } else {
                // Same endpoint: ensure trace_exporter is disabled and log_user_prompt is set
                if let Some(otel) = config_table.get_mut("otel").and_then(|v| v.as_table_mut()) {
                    otel.insert(
                        "trace_exporter".to_string(),
                        toml::Value::String("none".to_string()),
                    );
                    otel.insert(
                        "log_user_prompt".to_string(),
                        toml::Value::Boolean(true),
                    );
                }
            }
        } else if otel.get("exporter").and_then(|v| v.as_str()).is_some() {
            // Has exporter as a unit variant (e.g., "none" or "statsig") - replace with our struct
            if let Some(otel) = config_table.get_mut("otel").and_then(|v| v.as_table_mut()) {
                otel.insert("exporter".to_string(), build_otlp_http_exporter(aiki_endpoint));
                otel.insert(
                    "trace_exporter".to_string(),
                    toml::Value::String("none".to_string()),
                );
                otel.insert(
                    "log_user_prompt".to_string(),
                    toml::Value::Boolean(true),
                );
                // Remove legacy flat fields if present from old aiki versions
                otel.remove("endpoint");
                otel.remove("protocol");
            }
        } else {
            // No exporter configured: add our struct variant
            if let Some(otel) = config_table.get_mut("otel").and_then(|v| v.as_table_mut()) {
                otel.insert("exporter".to_string(), build_otlp_http_exporter(aiki_endpoint));
                otel.insert(
                    "trace_exporter".to_string(),
                    toml::Value::String("none".to_string()),
                );
                otel.insert(
                    "log_user_prompt".to_string(),
                    toml::Value::Boolean(true),
                );
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
        otel_table.insert("exporter".to_string(), build_otlp_http_exporter(aiki_endpoint));
        // Disable trace exporter (we only want logs, not distributed tracing spans)
        otel_table.insert(
            "trace_exporter".to_string(),
            toml::Value::String("none".to_string()),
        );
        otel_table.insert(
            "log_user_prompt".to_string(),
            toml::Value::Boolean(true),
        );
        config_table.insert("otel".to_string(), toml::Value::Table(otel_table));
    }

    // Configure notify command
    let notify_cmd = vec![
        toml::Value::String(aiki_path),
        toml::Value::String("hooks".to_string()),
        toml::Value::String("stdin".to_string()),
        toml::Value::String("--agent".to_string()),
        toml::Value::String("codex".to_string()),
        toml::Value::String("--event".to_string()),
        toml::Value::String("agent-turn-complete".to_string()),
    ];
    config_table.insert("notify".to_string(), toml::Value::Array(notify_cmd));

    // Write updated config
    let content =
        toml::to_string_pretty(&config).context("Failed to serialize config.toml")?;
    fs::write(&config_path, content).context("Failed to write ~/.codex/config.toml")?;

    println!("✓ Installed Codex hooks at {}", config_path.display());
    println!("  - [otel.exporter]: Log events → {}", aiki_endpoint);
    println!("  - [otel.trace_exporter]: Disabled (no trace spans)");
    println!("  - notify: Turn completion tracking");
    println!("  - log_user_prompt: true (prompt content capture enabled)");

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
            eprintln!(
                "⚠ OTel receiver restart not supported on {} yet",
                other
            );
            Ok(())
        }
    }
}

fn restart_otel_receiver_macos() -> Result<()> {
    let home_dir = dirs::home_dir().context("Could not find home directory")?;
    let plist_path = home_dir.join("Library/LaunchAgents/com.aiki.otel-receive.plist");

    // Unload (stop)
    let _ = Command::new("launchctl")
        .args(["unload", "-w"])
        .arg(&plist_path)
        .output();

    // Reload (start)
    let output = Command::new("launchctl")
        .args(["load", "-w"])
        .arg(&plist_path)
        .output()
        .context("Failed to run launchctl load")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("launchctl load failed: {}", stderr.trim());
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

    fs::create_dir_all(&agents_dir).context("Failed to create ~/Library/LaunchAgents")?;

    // Unload existing if present (ignore errors - may not be loaded)
    if plist_path.exists() {
        let _ = Command::new("launchctl")
            .args(["unload", "-w"])
            .arg(&plist_path)
            .output();
    }

    let plist_content = generate_launchd_plist(aiki_path);
    fs::write(&plist_path, &plist_content).context("Failed to write launchd plist")?;

    // Load the agent
    let output = Command::new("launchctl")
        .args(["load", "-w"])
        .arg(&plist_path)
        .output()
        .context("Failed to run launchctl load")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("launchctl load failed: {}", stderr.trim());
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

/// Read JJ repository config from .jj/repo/config.toml
pub fn read_jj_repo_config(repo_path: &Path) -> Result<toml::Value> {
    let config_path = repo_path.join(".jj").join("repo").join("config.toml");

    if !config_path.exists() {
        // Return empty config if file doesn't exist
        return Ok(toml::Value::Table(toml::map::Map::new()));
    }

    let config_content =
        fs::read_to_string(&config_path).context("Failed to read .jj/repo/config.toml")?;

    toml::from_str(&config_content).context("Failed to parse .jj/repo/config.toml")
}

/// Write JJ repository config to .jj/repo/config.toml
pub fn write_jj_repo_config(repo_path: &Path, config: &toml::Value) -> Result<()> {
    let config_path = repo_path.join(".jj").join("repo").join("config.toml");

    // Ensure .jj/repo directory exists
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).context("Failed to create .jj/repo directory")?;
    }

    let config_content =
        toml::to_string_pretty(config).context("Failed to serialize config to TOML")?;

    fs::write(&config_path, config_content).context("Failed to write .jj/repo/config.toml")
}

/// Update JJ signing configuration in .jj/repo/config.toml
pub fn update_jj_signing_config(
    repo_path: &Path,
    backend: &str,
    key: Option<&str>,
    behavior: &str,
) -> Result<()> {
    let mut config = read_jj_repo_config(repo_path)?;

    // Ensure config is a table
    let config_table = config
        .as_table_mut()
        .context("Config root is not a table")?;

    // Create [signing] section
    let mut signing_table = toml::map::Map::new();
    signing_table.insert(
        "behavior".to_string(),
        toml::Value::String(behavior.to_string()),
    );
    signing_table.insert(
        "backend".to_string(),
        toml::Value::String(backend.to_string()),
    );

    // For SSH backend, add key and allowed-signers configuration
    if backend == "ssh" {
        if let Some(key_path) = key {
            signing_table.insert("key".to_string(), toml::Value::String(key_path.to_string()));
        }

        // Add [signing.backends.ssh] configuration
        let mut ssh_config = toml::map::Map::new();
        ssh_config.insert(
            "allowed-signers".to_string(),
            toml::Value::String(".jj/allowed-signers".to_string()),
        );

        let mut backends = toml::map::Map::new();
        backends.insert("ssh".to_string(), toml::Value::Table(ssh_config));
        signing_table.insert("backends".to_string(), toml::Value::Table(backends));
    }

    // Insert signing section into config
    config_table.insert("signing".to_string(), toml::Value::Table(signing_table));

    write_jj_repo_config(repo_path, &config)
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
}
