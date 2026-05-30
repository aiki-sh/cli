use crate::commands::agents_template::{AIKI_BLOCK_TEMPLATE, AIKI_BLOCK_VERSION};
use crate::commands::zed_detection;
use crate::config;
use crate::editors::zed as ide_config;
use crate::error::Result;
use crate::repos::RepoDetector;
use crate::signing;
use anyhow::Context;
use std::env;
use std::fs;
use std::io::{self, Write};

pub fn run(fix: bool) -> Result<()> {
    let mut issues_found = 0;
    let fixes_applied = 0;

    if fix {
        println!("Diagnosing and fixing issues...\n");
    } else {
        println!("Checking Aiki health...\n");
    }

    // Check repository setup
    println!("Repository:");

    let current_dir = env::current_dir().context("Failed to get current directory")?;

    // Resolve the project root by walking up from cwd to find the Git repo root.
    // This ensures doctor works correctly when run from subdirectories.
    let project_root = RepoDetector::new(&current_dir)
        .find_repo_root()
        .unwrap_or_else(|_| current_dir.clone());

    // Check JJ
    if RepoDetector::has_jj(&project_root) {
        println!("  ✓ JJ workspace initialized");
    } else {
        println!("  ✗ JJ workspace not found");
        println!("    → Run: aiki init");
        issues_found += 1;
    }

    // Check Git
    if project_root.join(".git").exists() {
        println!("  ✓ Git repository detected");
    } else {
        println!("  ⚠ No Git repository (optional)");
    }

    // Check Aiki directory
    let aiki_dir = project_root.join(".aiki");
    if aiki_dir.exists() {
        println!("  ✓ Aiki directory exists");
    } else {
        println!("  ✗ Aiki directory missing");
        println!("    → Run: aiki init");
        issues_found += 1;
    }

    println!();

    // Check global hooks
    println!("Global Hooks:");

    let home_dir = dirs::home_dir().context("Failed to get home directory")?;

    // Check Git hooks
    let git_hooks_dir = home_dir.join(".aiki/githooks");
    if git_hooks_dir.exists() {
        println!("  ✓ Git hooks installed (~/.aiki/githooks/)");
    } else {
        println!("  ✗ Git hooks missing");
        println!("    → Run: aiki init or aiki doctor --fix");
        issues_found += 1;
    }

    // Check Claude Code hooks - verify file exists AND contains all required hooks
    let claude_settings = home_dir.join(".claude/settings.json");
    let missing_claude_hooks = find_missing_claude_code_hooks(&claude_settings);
    if missing_claude_hooks.is_empty() {
        println!("  ✓ Claude Code hooks configured");
    } else {
        println!("  ✗ Claude Code hooks: missing {}", missing_claude_hooks.join(", "));
        if fix {
            println!("    Installing Claude Code hooks...");
            match config::install_claude_code_hooks_global() {
                Ok(()) => {
                    println!("    ✓ Claude Code hooks installed");
                }
                Err(e) => {
                    println!("    ✗ Failed to install: {}", e);
                    issues_found += 1;
                }
            }
        } else {
            println!("    → Run: aiki doctor --fix");
            issues_found += 1;
        }
    }

    // Check Cursor hooks - verify file exists AND contains aiki hooks
    let cursor_hooks_path = home_dir.join(".cursor/hooks.json");
    let cursor_hooks_ok = check_cursor_hooks(&cursor_hooks_path);
    if cursor_hooks_ok {
        println!("  ✓ Cursor hooks configured");
    } else {
        println!("  ✗ Cursor hooks not configured");
        if fix {
            println!("    Installing Cursor hooks...");
            match config::install_cursor_hooks_global() {
                Ok(()) => {
                    println!("    ✓ Cursor hooks installed");
                }
                Err(e) => {
                    println!("    ✗ Failed to install: {}", e);
                    issues_found += 1;
                }
            }
        } else {
            println!("    → Run: aiki doctor --fix");
            issues_found += 1;
        }
    }

    // Check Codex hooks - verify config.toml contains aiki OTel + notify
    let codex_config_path = home_dir.join(".codex/config.toml");
    let codex_hooks_ok = check_codex_hooks(&codex_config_path);
    if codex_hooks_ok {
        println!("  ✓ Codex hooks configured");
    } else {
        println!("  ✗ Codex hooks not configured");
        if fix {
            println!("    Installing Codex hooks...");
            match config::install_codex_hooks_global() {
                Ok(()) => {
                    println!("    ✓ Codex hooks installed");
                }
                Err(e) => {
                    println!("    ✗ Failed to install: {}", e);
                    issues_found += 1;
                }
            }
        } else {
            println!("    → Run: aiki doctor --fix");
            issues_found += 1;
        }
    }

    // Check Codex OTel receiver socket (non-blocking connection test)
    let otel_receiver_ok = check_otel_receiver();
    if otel_receiver_ok && !fix {
        println!("  ✓ OTel receiver listening on 127.0.0.1:19876");
    } else if fix {
        println!("  {} OTel receiver", if otel_receiver_ok { "✓" } else { "✗" });
        println!("    Restarting OTel receiver...");
        match config::restart_otel_receiver() {
            Ok(()) => {
                println!("    ✓ OTel receiver restarted");
            }
            Err(e) => {
                println!("    ✗ Failed to restart OTel receiver: {}", e);
                issues_found += 1;
            }
        }
    } else {
        println!("  ✗ OTel receiver not listening");
        println!("    → Run: aiki doctor --fix");
        issues_found += 1;
    }

    println!();

    // Check ACP (Agent Client Protocol) configuration
    println!("ACP Configuration:");

    // Check Zed ACP configuration
    match ide_config::is_zed_configured() {
        Ok(true) => {
            println!("  ✓ Zed editor configured for ACP");
            if let Some(path) = ide_config::zed_settings_path() {
                println!("    Settings: {}", path.display());
            }
        }
        Ok(false) => {
            if let Some(path) = ide_config::zed_settings_path() {
                if path.parent().map(|p| p.exists()).unwrap_or(false) {
                    println!("  ✗ Zed editor not configured for ACP");
                    issues_found += 1; // Count unconfigured state as an issue
                    if fix {
                        println!("    Configuring Zed for ACP...");
                        match ide_config::configure_zed() {
                            Ok(()) => {
                                println!("    ✓ Configured Zed editor");
                                issues_found -= 1; // Clear the issue since we fixed it
                            }
                            Err(e) => {
                                println!("    ✗ Failed to configure Zed: {}", e);
                                // Issue already counted above
                            }
                        }
                    } else {
                        println!("    → Run: aiki doctor --fix (to configure Zed)");
                    }
                } else {
                    println!("  - Zed editor not installed");
                }
            }
        }
        Err(e) => {
            println!("  ✗ Error checking Zed configuration: {}", e);
            issues_found += 1;
        }
    }

    // Check ACP binary availability
    println!("\n  ACP Agent Binaries:");

    // Check common agents
    let agents_to_check = vec![
        ("claude-code", "Claude Code"),
        ("codex", "Codex"),
        ("gemini", "Gemini"),
    ];

    for (agent_type, display_name) in agents_to_check {
        match zed_detection::resolve_agent_binary(agent_type) {
            Ok(resolved) => match resolved {
                zed_detection::ResolvedBinary::ZedNodeJs(path) => {
                    println!("    ✓ {} (Zed Node.js)", display_name);
                    if std::env::var("VERBOSE").is_ok() {
                        println!("      {}", path.display());
                    }
                }
                zed_detection::ResolvedBinary::ZedNative(path) => {
                    println!("    ✓ {} (Zed native)", display_name);
                    if std::env::var("VERBOSE").is_ok() {
                        println!("      {}", path.display());
                    }
                }
                zed_detection::ResolvedBinary::InPath(exe) => {
                    println!("    ✓ {} (system PATH)", display_name);
                    if std::env::var("VERBOSE").is_ok() {
                        println!("      {}", exe);
                    }
                }
            },
            Err(_) => {
                println!("    - {} not installed", display_name);
            }
        }
    }

    // Check Node.js for Node.js-based agents
    if let Ok(_) = zed_detection::check_nodejs_installed() {
        // Node.js check already prints version to stderr
    }

    println!();

    // Check local configuration (only if in a repo)
    if project_root.join(".git").exists() {
        println!("Local Configuration:");

        // Check git core.hooksPath
        let output = std::process::Command::new("git")
            .args(["config", "core.hooksPath"])
            .current_dir(&project_root)
            .output();

        if let Ok(output) = output {
            if output.status.success() {
                let hooks_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if hooks_path.contains(".aiki/githooks") {
                    println!("  ✓ Git core.hooksPath configured");
                } else {
                    println!("  ⚠ Git core.hooksPath points elsewhere: {}", hooks_path);
                }
            } else {
                println!("  ✗ Git core.hooksPath not set");
                println!("    → Run: aiki init");
                issues_found += 1;
            }
        }

        println!();
    }

    // Check commit signing (only if in a repo)
    if project_root.join(".jj").exists() {
        println!("Commit Signing:");

        match signing::read_signing_config(&project_root) {
            Ok(Some(config)) => {
                println!("  ✓ JJ signing enabled ({:?})", config.backend);

                match signing::verify_key_accessible(&config) {
                    Ok(true) => {
                        println!("  ✓ Signing key accessible: {}", config.key);
                    }
                    Ok(false) => {
                        println!("  ✗ Signing key not found: {}", config.key);
                        println!("    → Run: aiki sign setup");
                        issues_found += 1;
                    }
                    Err(e) => {
                        println!("  ✗ Error verifying key: {}", e);
                        issues_found += 1;
                    }
                }
            }
            Ok(None) => {
                println!("  ⚠ JJ signing not configured");

                if fix {
                    println!();
                    println!("Would you like to set up signing now?");
                    let setup = prompt_yes_no("Set up signing", true)?;

                    if setup {
                        let wizard = signing::SignSetupWizard::new(project_root.clone());
                        wizard.run(None)?;
                    } else {
                        println!("Skipping signing setup.");
                    }
                } else {
                    println!("    → Run: aiki doctor --fix (to set up signing)");
                }
                // Not counted as error, just a warning
            }
            Err(e) => {
                println!("  ✗ Error reading JJ config: {}", e);
                issues_found += 1;
            }
        }

        println!();
    }

    // Check AGENTS.md for task system instructions
    println!("Agent Instructions:");

    let agents_path = project_root.join("AGENTS.md");
    if agents_path.exists() {
        match fs::read_to_string(&agents_path) {
            Ok(content) => {
                if content.contains(&format!("<aiki version=\"{}\">", AIKI_BLOCK_VERSION)) {
                    println!("  ✓ AGENTS.md has current <aiki> block");
                } else if content.contains("<aiki version=") {
                    println!("  ⚠ AGENTS.md has outdated <aiki> block");
                    if fix {
                        // Replace old block with new one
                        if let Some(start) = content.find("<aiki version=") {
                            if let Some(end) = content.find("</aiki>") {
                                let before = &content[..start];
                                let after = &content[end + "</aiki>".len()..];
                                let updated = format!(
                                    "{}{}{}",
                                    before.trim_end(),
                                    AIKI_BLOCK_TEMPLATE,
                                    after.trim_start()
                                );
                                match fs::write(&agents_path, updated) {
                                    Ok(()) => {
                                        println!(
                                            "    ✓ Updated <aiki> block to version {}",
                                            AIKI_BLOCK_VERSION
                                        );
                                    }
                                    Err(e) => {
                                        println!("    ✗ Failed to update AGENTS.md: {}", e);
                                        issues_found += 1;
                                    }
                                }
                            }
                        }
                    } else {
                        println!("    → Run: aiki doctor --fix (to update block)");
                    }
                } else {
                    println!("  ⚠ AGENTS.md missing <aiki> block");
                    if fix {
                        // Prepend block to existing content
                        let updated = format!("{}\n{}", AIKI_BLOCK_TEMPLATE, content);
                        match fs::write(&agents_path, updated) {
                            Ok(()) => {
                                println!("    ✓ Added <aiki> block to AGENTS.md");
                            }
                            Err(e) => {
                                println!("    ✗ Failed to update AGENTS.md: {}", e);
                                issues_found += 1;
                            }
                        }
                    } else {
                        println!("    → Run: aiki doctor --fix (to add block)");
                    }
                }
            }
            Err(e) => {
                println!("  ✗ Failed to read AGENTS.md: {}", e);
                issues_found += 1;
            }
        }
    } else {
        println!("  ⚠ AGENTS.md not found");
        if fix {
            match fs::write(&agents_path, AIKI_BLOCK_TEMPLATE) {
                Ok(()) => {
                    println!("    ✓ Created AGENTS.md with task system instructions");
                }
                Err(e) => {
                    println!("    ✗ Failed to create AGENTS.md: {}", e);
                    issues_found += 1;
                }
            }
        } else {
            println!("    → Run: aiki doctor --fix (to create)");
        }
    }

    println!();

    // Check hookfile
    println!("Hookfile:");

    let hooks_yml_path = project_root.join(".aiki/hooks.yml");
    if aiki_dir.exists() {
        if hooks_yml_path.exists() {
            // Validate YAML syntax, include references, and event names
            match fs::read_to_string(&hooks_yml_path) {
                Ok(content) => {
                    match serde_yaml::from_str::<serde_yaml::Value>(&content) {
                        Ok(yaml) => {
                            println!("  ✓ .aiki/hooks.yml exists and is valid YAML");

                            // Validate include references
                            if let Some(includes) = yaml
                                .as_mapping()
                                .and_then(|m| m.get("include"))
                                .and_then(|v| v.as_sequence())
                            {
                                for include in includes {
                                    if let Some(name) = include.as_str() {
                                        if name == "aiki/core" {
                                            println!("  ℹ No need to reference aiki/core — it always runs automatically");
                                        } else if !is_plugin_resolvable(name, &project_root) {
                                            println!(
                                                "  ⚠ Plugin '{}' not found (referenced in include:)",
                                                name
                                            );
                                            issues_found += 1;
                                        }
                                    }
                                }
                            }

                            // Validate event names (top-level keys and in before/after blocks)
                            if let Some(mapping) = yaml.as_mapping() {
                                issues_found += validate_event_keys(mapping, "");

                                // Check before/after composition blocks
                                for block_key in &["before", "after"] {
                                    if let Some(block) = mapping
                                        .get(serde_yaml::Value::String(block_key.to_string()))
                                        .and_then(|v| v.as_mapping())
                                    {
                                        issues_found +=
                                            validate_event_keys(block, &format!("{}:", block_key));

                                        // Check for aiki/core in composition block includes
                                        if let Some(block_includes) = block
                                            .get("include")
                                            .and_then(|v| v.as_sequence())
                                        {
                                            for include in block_includes {
                                                if include.as_str() == Some("aiki/core") {
                                                    println!("  ℹ No need to reference aiki/core in {}: — it always runs automatically", block_key);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            println!("  ✗ .aiki/hooks.yml has invalid YAML: {}", e);
                            issues_found += 1;
                        }
                    }
                }
                Err(e) => {
                    println!("  ✗ Failed to read .aiki/hooks.yml: {}", e);
                    issues_found += 1;
                }
            }
        } else {
            println!("  ⚠ No hookfile found");
            println!("    Co-author trailers and workflow automation are disabled.");
            if fix {
                match fs::write(
                    &hooks_yml_path,
                    super::init::HOOKS_YML_TEMPLATE,
                ) {
                    Ok(()) => {
                        println!(
                            "    ✓ Created .aiki/hooks.yml with default workflow automation"
                        );
                    }
                    Err(e) => {
                        println!("    ✗ Failed to create hookfile: {}", e);
                        issues_found += 1;
                    }
                }
            } else {
                println!("    → Run: aiki init or aiki doctor --fix");
                issues_found += 1;
            }
        }
    } else {
        println!("  - No hookfile (run aiki init to create one)");
    }

    println!();

    // Check plugins
    if aiki_dir.exists() {
        println!("Plugins:");

        match crate::plugins::project::check_project_plugins(&project_root) {
            Ok(statuses) => {
                if statuses.is_empty() {
                    println!("  - No plugin references found in project");
                } else {
                    for (plugin, status) in &statuses {
                        match status {
                            crate::plugins::InstallStatus::Installed => {
                                println!("  ✓ {} installed", plugin);
                            }
                            crate::plugins::InstallStatus::PartialInstall => {
                                println!("  ✗ {} partial install (interrupted clone?)", plugin);
                                issues_found += 1;
                                if fix {
                                    println!("    Reinstalling {}...", plugin);
                                    match crate::plugins::project::install_project_plugins(
                                        &project_root,
                                    ) {
                                        Ok(_) => {
                                            println!("    ✓ Reinstalled");
                                            issues_found -= 1;
                                        }
                                        Err(e) => {
                                            println!("    ✗ Failed: {}", e);
                                        }
                                    }
                                } else {
                                    println!("    → Run: aiki plugin install {}", plugin);
                                }
                            }
                            crate::plugins::InstallStatus::NotInstalled => {
                                println!("  ✗ {} not installed", plugin);
                                issues_found += 1;
                                if fix {
                                    println!("    Installing {}...", plugin);
                                    match crate::plugins::project::install_project_plugins(
                                        &project_root,
                                    ) {
                                        Ok(_) => {
                                            println!("    ✓ Installed");
                                            issues_found -= 1;
                                        }
                                        Err(e) => {
                                            println!("    ✗ Failed: {}", e);
                                        }
                                    }
                                } else {
                                    println!("    → Run: aiki plugin install {}", plugin);
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                println!("  ✗ Error checking plugins: {}", e);
                issues_found += 1;
            }
        }

        println!();
    }

    // Summary
    if issues_found == 0 {
        println!("✓ All checks passed! Aiki is healthy.");
    } else {
        println!("Found {} issue(s).", issues_found);
        if !fix {
            println!("\nRun 'aiki doctor --fix' to automatically fix issues.");
        } else if fixes_applied > 0 {
            println!("\nFixed {} issue(s).", fixes_applied);
        }
    }

    Ok(())
}

/// Prompt for yes/no
fn prompt_yes_no(prompt: &str, default: bool) -> Result<bool> {
    let default_str = if default { "Y/n" } else { "y/N" };
    print!("{} [{}]: ", prompt, default_str);
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim().to_lowercase();

    if input.is_empty() {
        return Ok(default);
    }

    Ok(input == "y" || input == "yes")
}

/// Check if a command string invokes aiki hooks stdin with specific agent/event
///
/// Matches commands like:
/// - `aiki hooks stdin --agent claude-code --event session.started`
/// - `/path/to/aiki.exe hooks stdin --agent cursor --event beforeSubmitPrompt`
///
/// If expected_agent or expected_event is Some, validates those flags are present.
fn is_aiki_hooks_command_with_params(
    cmd: &str,
    expected_agent: Option<&str>,
    expected_event: Option<&str>,
) -> bool {
    // Split command into words
    let words: Vec<&str> = cmd.split_whitespace().collect();

    // Look for pattern: <something-ending-with-aiki> hooks stdin
    let mut found_hooks_stdin = false;
    for (i, word) in words.iter().enumerate() {
        // Check if this word is the aiki binary (with or without path, with or without .exe)
        let is_aiki_binary = word.ends_with("aiki") || word.ends_with("aiki.exe");

        if is_aiki_binary {
            // Check if followed by "hooks stdin"
            if i + 2 < words.len() && words[i + 1] == "hooks" && words[i + 2] == "stdin" {
                found_hooks_stdin = true;
                break;
            }
        }
    }

    if !found_hooks_stdin {
        return false;
    }

    // If no specific agent/event required, we're done
    if expected_agent.is_none() && expected_event.is_none() {
        return true;
    }

    // Check for --agent flag
    if let Some(agent) = expected_agent {
        let has_agent = words.windows(2).any(|w| w[0] == "--agent" && w[1] == agent);
        if !has_agent {
            return false;
        }
    }

    // Check for --event flag
    if let Some(event) = expected_event {
        let has_event = words.windows(2).any(|w| w[0] == "--event" && w[1] == event);
        if !has_event {
            return false;
        }
    }

    true
}

/// Find which Claude Code hooks are missing from ~/.claude/settings.json
///
/// Returns a list of missing hook names. Empty list means all hooks are configured.
fn find_missing_claude_code_hooks(settings_path: &std::path::Path) -> Vec<&'static str> {
    // Required Claude Code hooks (must match what config::install_claude_code_hooks_global installs)
    let required_hooks: &[&str] = &[
        "SessionStart",
        "PreCompact",
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
        "Stop",
        "SessionEnd",
    ];

    if !settings_path.exists() {
        return required_hooks.to_vec();
    }

    let content = match fs::read_to_string(settings_path) {
        Ok(c) => c,
        Err(_) => return required_hooks.to_vec(),
    };

    let settings: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return required_hooks.to_vec(),
    };

    let hooks = match settings.get("hooks") {
        Some(h) => h,
        None => return required_hooks.to_vec(),
    };

    // Helper to check if a Claude Code hook entry contains aiki command with correct params
    let has_hook = |hook_name: &str| -> bool {
        hooks
            .get(hook_name)
            .and_then(|arr| arr.as_array())
            .map(|arr| {
                arr.iter().any(|entry| {
                    entry
                        .get("hooks")
                        .and_then(|h| h.as_array())
                        .map(|hooks_arr| {
                            hooks_arr.iter().any(|hook| {
                                hook.get("command")
                                    .and_then(|c| c.as_str())
                                    .map(|c| {
                                        is_aiki_hooks_command_with_params(
                                            c,
                                            Some("claude-code"),
                                            Some(hook_name),
                                        )
                                    })
                                    .unwrap_or(false)
                            })
                        })
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    };

    required_hooks
        .iter()
        .filter(|name| !has_hook(name))
        .copied()
        .collect()
}

/// Check if Cursor hooks are properly configured
///
/// Returns true if ~/.cursor/hooks.json exists AND contains both:
/// - hooks.beforeSubmitPrompt with aiki hooks stdin command
/// - hooks.afterFileEdit with aiki hooks stdin command
fn check_cursor_hooks(hooks_path: &std::path::Path) -> bool {
    if !hooks_path.exists() {
        return false;
    }

    let content = match fs::read_to_string(hooks_path) {
        Ok(c) => c,
        Err(_) => return false,
    };

    let config: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return false,
    };

    let hooks = match config.get("hooks") {
        Some(h) => h,
        None => return false,
    };

    // Helper to check if an array contains an aiki hooks stdin command with specific agent/event
    let has_aiki_hook_with_params =
        |arr: &serde_json::Value, agent: &str, event: &str| -> bool {
            arr.as_array()
                .map(|arr| {
                    arr.iter().any(|hook| {
                        hook.get("command")
                            .and_then(|c| c.as_str())
                            .map(|c| is_aiki_hooks_command_with_params(c, Some(agent), Some(event)))
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false)
        };

    // Required Cursor hooks
    let required_hooks = [
        "beforeSubmitPrompt",
        "afterFileEdit",
        "beforeShellExecution",
        "afterShellExecution",
        "beforeMCPExecution",
        "afterMCPExecution",
        "stop",
    ];

    required_hooks.iter().all(|hook_name| {
        hooks
            .get(*hook_name)
            .map(|arr| has_aiki_hook_with_params(arr, "cursor", hook_name))
            .unwrap_or(false)
    })
}

/// Check if Codex hooks are properly configured
///
/// Returns true if ~/.codex/config.toml exists AND contains both:
/// - [otel] section with aiki endpoint (127.0.0.1:19876)
/// - notify array with aiki hooks stdin command
fn check_codex_hooks(config_path: &std::path::Path) -> bool {
    if !config_path.exists() {
        return false;
    }

    let content = match fs::read_to_string(config_path) {
        Ok(c) => c,
        Err(_) => return false,
    };

    let config: toml::Value = match toml::from_str(&content) {
        Ok(v) => v,
        Err(_) => return false,
    };

    // Check [otel.exporter.otlp-http] section has aiki endpoint
    let has_otel = config
        .get("otel")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("exporter"))
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("otlp-http"))
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("endpoint"))
        .and_then(|v| v.as_str())
        .map(|endpoint| endpoint.contains("19876"))
        .unwrap_or(false);

    // Check notify array contains aiki command
    let has_notify = config
        .get("notify")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .any(|v| v.as_str().map(|s| s.contains("aiki")).unwrap_or(false))
        })
        .unwrap_or(false);

    has_otel && has_notify
}

/// Check if the OTel receiver is listening on 127.0.0.1:19876
///
/// Attempts a non-blocking TCP connection with a short timeout.
/// Returns true if the port is reachable (socket activation is working).
fn check_otel_receiver() -> bool {
    use std::net::{SocketAddr, TcpStream};
    use std::time::Duration;

    let addr: SocketAddr = "127.0.0.1:19876".parse().unwrap();
    TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok()
}

/// Known hook event names for validation.
const KNOWN_EVENTS: &[&str] = &[
    "session.started",
    "session.resumed",
    "session.ended",
    "turn.started",
    "turn.completed",
    "read.permission_asked",
    "read.completed",
    "change.permission_asked",
    "change.completed",
    "shell.permission_asked",
    "shell.completed",
    "web.permission_asked",
    "web.completed",
    "mcp.permission_asked",
    "mcp.completed",
    "commit.message_started",
    "task.started",
    "task.closed",
];

/// Non-event top-level keys that are valid in a hookfile.
const HOOKFILE_META_KEYS: &[&str] = &[
    "name",
    "description",
    "version",
    "include",
    "before",
    "after",
];

/// Check if a plugin include reference can be resolved.
///
/// Returns true if the plugin exists as:
/// 1. A file at `.aiki/hooks/{namespace}/{name}.yml` (project level)
/// 2. A file at `~/.aiki/hooks/{namespace}/{name}.yml` (user level)
/// 3. A built-in plugin embedded in the binary
fn is_plugin_resolvable(name: &str, project_root: &std::path::Path) -> bool {
    // Check built-in plugins first (cheapest check)
    if crate::flows::bundled::load_builtin_plugin(name).is_some() {
        return true;
    }

    // Check file-based resolution
    let parts: Vec<&str> = name.splitn(2, '/').collect();
    if parts.len() == 2 {
        let project_path = project_root
            .join(".aiki/hooks")
            .join(parts[0])
            .join(format!("{}.yml", parts[1]));
        if project_path.exists() {
            return true;
        }

        if let Some(home) = dirs::home_dir() {
            let user_path = home
                .join(".aiki/hooks")
                .join(parts[0])
                .join(format!("{}.yml", parts[1]));
            if user_path.exists() {
                return true;
            }
        }
    }

    false
}

/// Validate event keys in a YAML mapping, warning about unknown events.
/// Returns the number of issues found.
fn validate_event_keys(mapping: &serde_yaml::Mapping, prefix: &str) -> usize {
    let mut issues = 0;

    for key in mapping.keys() {
        if let Some(key_str) = key.as_str() {
            // Skip non-event keys (metadata, composition)
            if HOOKFILE_META_KEYS.contains(&key_str) {
                continue;
            }

            // Check if it's a known event or a valid sugar pattern
            if !KNOWN_EVENTS.contains(&key_str)
                && !crate::flows::sugar::is_sugar_pattern(key_str)
            {
                let location = if prefix.is_empty() {
                    String::new()
                } else {
                    format!(" (in {})", prefix)
                };

                if let Some(suggestion) = suggest_event(key_str) {
                    println!(
                        "  ⚠ Unknown event '{}'{} (did you mean '{}'?)",
                        key_str, location, suggestion
                    );
                } else {
                    println!("  ⚠ Unknown event '{}'{}", key_str, location);
                }
                issues += 1;
            }
        }
    }

    issues
}

/// Suggest the closest known event name for a typo.
fn suggest_event(unknown: &str) -> Option<&'static str> {
    let mut best: Option<(&str, usize)> = None;

    for known in KNOWN_EVENTS {
        let dist = edit_distance(unknown, known);
        if dist <= 3 {
            if best.is_none() || dist < best.unwrap().1 {
                best = Some((known, dist));
            }
        }
    }

    best.map(|(s, _)| s)
}

/// Simple Levenshtein edit distance.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let m = a.len();
    let n = b.len();

    let mut prev = (0..=n).collect::<Vec<_>>();
    let mut curr = vec![0; n + 1];

    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[n]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_find_missing_claude_code_hooks_complete() {
        let mut file = NamedTempFile::new().unwrap();
        let settings = serde_json::json!({
            "hooks": {
                "SessionStart": [{
                    "matcher": "startup",
                    "hooks": [{
                        "type": "command",
                        "command": "/path/to/aiki hooks stdin --agent claude-code --event SessionStart"
                    }]
                }],
                "PreCompact": [{
                    "matcher": "",
                    "hooks": [{
                        "type": "command",
                        "command": "/path/to/aiki hooks stdin --agent claude-code --event PreCompact"
                    }]
                }],
                "UserPromptSubmit": [{
                    "matcher": "",
                    "hooks": [{
                        "type": "command",
                        "command": "/path/to/aiki hooks stdin --agent claude-code --event UserPromptSubmit"
                    }]
                }],
                "PreToolUse": [{
                    "matcher": "Edit|Write|Bash",
                    "hooks": [{
                        "type": "command",
                        "command": "/path/to/aiki hooks stdin --agent claude-code --event PreToolUse"
                    }]
                }],
                "PostToolUse": [{
                    "matcher": "Edit|Write|Bash",
                    "hooks": [{
                        "type": "command",
                        "command": "/path/to/aiki hooks stdin --agent claude-code --event PostToolUse"
                    }]
                }],
                "Stop": [{
                    "matcher": "",
                    "hooks": [{
                        "type": "command",
                        "command": "/path/to/aiki hooks stdin --agent claude-code --event Stop"
                    }]
                }],
                "SessionEnd": [{
                    "matcher": "",
                    "hooks": [{
                        "type": "command",
                        "command": "/path/to/aiki hooks stdin --agent claude-code --event SessionEnd"
                    }]
                }]
            }
        });
        write!(file, "{}", serde_json::to_string(&settings).unwrap()).unwrap();

        assert!(find_missing_claude_code_hooks(file.path()).is_empty());
    }

    #[test]
    fn test_find_missing_claude_code_hooks_missing_pre_compact_and_session_end() {
        let mut file = NamedTempFile::new().unwrap();
        let settings = serde_json::json!({
            "hooks": {
                "SessionStart": [{
                    "matcher": "startup",
                    "hooks": [{
                        "type": "command",
                        "command": "/path/to/aiki hooks stdin --agent claude-code --event SessionStart"
                    }]
                }],
                "UserPromptSubmit": [{
                    "matcher": "",
                    "hooks": [{
                        "type": "command",
                        "command": "/path/to/aiki hooks stdin --agent claude-code --event UserPromptSubmit"
                    }]
                }],
                "PreToolUse": [{
                    "matcher": "Edit|Write|Bash",
                    "hooks": [{
                        "type": "command",
                        "command": "/path/to/aiki hooks stdin --agent claude-code --event PreToolUse"
                    }]
                }],
                "PostToolUse": [{
                    "matcher": "Edit|Write|Bash",
                    "hooks": [{
                        "type": "command",
                        "command": "/path/to/aiki hooks stdin --agent claude-code --event PostToolUse"
                    }]
                }],
                "Stop": [{
                    "matcher": "",
                    "hooks": [{
                        "type": "command",
                        "command": "/path/to/aiki hooks stdin --agent claude-code --event Stop"
                    }]
                }]
            }
        });
        write!(file, "{}", serde_json::to_string(&settings).unwrap()).unwrap();

        let missing = find_missing_claude_code_hooks(file.path());
        assert_eq!(missing, vec!["PreCompact", "SessionEnd"]);
    }

    #[test]
    fn test_find_missing_claude_code_hooks_missing_post_tool_use() {
        let mut file = NamedTempFile::new().unwrap();
        let settings = serde_json::json!({
            "hooks": {
                "SessionStart": [{
                    "matcher": "startup",
                    "hooks": [{
                        "type": "command",
                        "command": "/path/to/aiki hooks stdin --agent claude-code --event SessionStart"
                    }]
                }]
            }
        });
        write!(file, "{}", serde_json::to_string(&settings).unwrap()).unwrap();

        let missing = find_missing_claude_code_hooks(file.path());
        assert!(missing.contains(&"PostToolUse"));
        assert!(missing.contains(&"PreCompact"));
        assert!(missing.contains(&"SessionEnd"));
    }

    #[test]
    fn test_find_missing_claude_code_hooks_wrong_command() {
        let mut file = NamedTempFile::new().unwrap();
        let settings = serde_json::json!({
            "hooks": {
                "SessionStart": [{
                    "matcher": "startup",
                    "hooks": [{
                        "type": "command",
                        "command": "/path/to/some-other-tool"
                    }]
                }],
                "PostToolUse": [{
                    "matcher": "Edit|Write",
                    "hooks": [{
                        "type": "command",
                        "command": "/path/to/aiki hooks stdin --agent claude-code --event afterFileEdit"
                    }]
                }]
            }
        });
        write!(file, "{}", serde_json::to_string(&settings).unwrap()).unwrap();

        let missing = find_missing_claude_code_hooks(file.path());
        assert!(missing.contains(&"SessionStart")); // wrong command
    }

    #[test]
    fn test_find_missing_claude_code_hooks_no_file() {
        let path = std::path::Path::new("/nonexistent/path/settings.json");
        assert_eq!(find_missing_claude_code_hooks(path).len(), 7); // all hooks missing
    }

    #[test]
    fn test_check_cursor_hooks_complete() {
        let mut file = NamedTempFile::new().unwrap();
        let hooks = serde_json::json!({
            "version": 1,
            "hooks": {
                "beforeSubmitPrompt": [{
                    "command": "/path/to/aiki hooks stdin --agent cursor --event beforeSubmitPrompt"
                }],
                "afterFileEdit": [{
                    "command": "/path/to/aiki hooks stdin --agent cursor --event afterFileEdit"
                }],
                "beforeShellExecution": [{
                    "command": "/path/to/aiki hooks stdin --agent cursor --event beforeShellExecution"
                }],
                "afterShellExecution": [{
                    "command": "/path/to/aiki hooks stdin --agent cursor --event afterShellExecution"
                }],
                "beforeMCPExecution": [{
                    "command": "/path/to/aiki hooks stdin --agent cursor --event beforeMCPExecution"
                }],
                "afterMCPExecution": [{
                    "command": "/path/to/aiki hooks stdin --agent cursor --event afterMCPExecution"
                }],
                "stop": [{
                    "command": "/path/to/aiki hooks stdin --agent cursor --event stop"
                }]
            }
        });
        write!(file, "{}", serde_json::to_string(&hooks).unwrap()).unwrap();

        assert!(check_cursor_hooks(file.path()));
    }

    #[test]
    fn test_check_cursor_hooks_missing_after_file_edit() {
        let mut file = NamedTempFile::new().unwrap();
        let hooks = serde_json::json!({
            "version": 1,
            "hooks": {
                "beforeSubmitPrompt": [{
                    "command": "/path/to/aiki hooks stdin --agent cursor --event beforeSubmitPrompt"
                }]
            }
        });
        write!(file, "{}", serde_json::to_string(&hooks).unwrap()).unwrap();

        assert!(!check_cursor_hooks(file.path()));
    }

    #[test]
    fn test_check_cursor_hooks_missing_before_submit() {
        let mut file = NamedTempFile::new().unwrap();
        let hooks = serde_json::json!({
            "version": 1,
            "hooks": {
                "afterFileEdit": [{
                    "command": "/path/to/aiki hooks stdin --agent cursor --event afterFileEdit"
                }]
            }
        });
        write!(file, "{}", serde_json::to_string(&hooks).unwrap()).unwrap();

        assert!(!check_cursor_hooks(file.path()));
    }

    #[test]
    fn test_check_cursor_hooks_wrong_command() {
        let mut file = NamedTempFile::new().unwrap();
        let hooks = serde_json::json!({
            "version": 1,
            "hooks": {
                "beforeSubmitPrompt": [{
                    "command": "/path/to/some-other-tool"
                }],
                "afterFileEdit": [{
                    "command": "/path/to/aiki hooks stdin --agent cursor --event afterFileEdit"
                }]
            }
        });
        write!(file, "{}", serde_json::to_string(&hooks).unwrap()).unwrap();

        assert!(!check_cursor_hooks(file.path()));
    }

    #[test]
    fn test_check_cursor_hooks_generic_aiki_not_enough() {
        // Ensure just "aiki" without "hooks stdin" doesn't match
        let mut file = NamedTempFile::new().unwrap();
        let hooks = serde_json::json!({
            "version": 1,
            "hooks": {
                "beforeSubmitPrompt": [{
                    "command": "/path/to/aiki init"
                }],
                "afterFileEdit": [{
                    "command": "/path/to/aiki record"
                }]
            }
        });
        write!(file, "{}", serde_json::to_string(&hooks).unwrap()).unwrap();

        assert!(!check_cursor_hooks(file.path()));
    }

    #[test]
    fn test_check_cursor_hooks_no_file() {
        let path = std::path::Path::new("/nonexistent/path/hooks.json");
        assert!(!check_cursor_hooks(path));
    }

    // Tests for is_aiki_hooks_command_with_params

    #[test]
    fn test_is_aiki_hooks_command_basic() {
        assert!(is_aiki_hooks_command_with_params(
            "aiki hooks stdin --agent claude-code --event session.started",
            Some("claude-code"),
            Some("session.started")
        ));
    }

    #[test]
    fn test_is_aiki_hooks_command_with_exe() {
        assert!(is_aiki_hooks_command_with_params(
            "aiki.exe hooks stdin --agent claude-code --event session.started",
            Some("claude-code"),
            Some("session.started")
        ));
    }

    #[test]
    fn test_is_aiki_hooks_command_with_path() {
        assert!(is_aiki_hooks_command_with_params(
            "/usr/local/bin/aiki hooks stdin --agent cursor --event beforeSubmitPrompt",
            Some("cursor"),
            Some("beforeSubmitPrompt")
        ));
    }

    #[test]
    fn test_is_aiki_hooks_command_with_path_and_exe() {
        assert!(is_aiki_hooks_command_with_params(
            "C:\\Program Files\\aiki.exe hooks stdin --agent claude-code --event afterFileEdit",
            Some("claude-code"),
            Some("afterFileEdit")
        ));
    }

    #[test]
    fn test_is_aiki_hooks_command_relative_path() {
        assert!(is_aiki_hooks_command_with_params(
            "./aiki hooks stdin --agent cursor --event afterFileEdit",
            Some("cursor"),
            Some("afterFileEdit")
        ));
    }

    #[test]
    fn test_is_aiki_hooks_command_wrong_agent() {
        // Should fail: command has claude-code but we expect cursor
        assert!(!is_aiki_hooks_command_with_params(
            "aiki hooks stdin --agent claude-code --event session.started",
            Some("cursor"),
            Some("session.started")
        ));
    }

    #[test]
    fn test_is_aiki_hooks_command_wrong_event() {
        // Should fail: command has session.started but we expect change.completed
        assert!(!is_aiki_hooks_command_with_params(
            "aiki hooks stdin --agent claude-code --event session.started",
            Some("claude-code"),
            Some("change.completed")
        ));
    }

    #[test]
    fn test_is_aiki_hooks_command_missing_agent() {
        // Should fail: no --agent flag
        assert!(!is_aiki_hooks_command_with_params(
            "aiki hooks stdin --event session.started",
            Some("claude-code"),
            Some("session.started")
        ));
    }

    #[test]
    fn test_is_aiki_hooks_command_missing_event() {
        // Should fail: no --event flag
        assert!(!is_aiki_hooks_command_with_params(
            "aiki hooks stdin --agent claude-code",
            Some("claude-code"),
            Some("session.started")
        ));
    }

    #[test]
    fn test_is_aiki_hooks_command_not_hooks_handle() {
        // Should fail: not "hooks stdin"
        assert!(!is_aiki_hooks_command_with_params(
            "aiki init --agent claude-code --event session.started",
            Some("claude-code"),
            Some("session.started")
        ));
    }

    #[test]
    fn test_is_aiki_hooks_command_no_params_check() {
        // Should pass with no param requirements
        assert!(is_aiki_hooks_command_with_params(
            "aiki hooks stdin",
            None,
            None
        ));
    }

    #[test]
    fn test_find_missing_claude_code_hooks_with_exe() {
        let mut file = NamedTempFile::new().unwrap();
        let settings = serde_json::json!({
            "hooks": {
                "SessionStart": [{
                    "matcher": "startup",
                    "hooks": [{
                        "type": "command",
                        "command": "aiki.exe hooks stdin --agent claude-code --event SessionStart"
                    }]
                }],
                "PreCompact": [{
                    "matcher": "",
                    "hooks": [{
                        "type": "command",
                        "command": "aiki.exe hooks stdin --agent claude-code --event PreCompact"
                    }]
                }],
                "UserPromptSubmit": [{
                    "matcher": "",
                    "hooks": [{
                        "type": "command",
                        "command": "aiki.exe hooks stdin --agent claude-code --event UserPromptSubmit"
                    }]
                }],
                "PreToolUse": [{
                    "matcher": "Edit|Write|Bash",
                    "hooks": [{
                        "type": "command",
                        "command": "aiki.exe hooks stdin --agent claude-code --event PreToolUse"
                    }]
                }],
                "PostToolUse": [{
                    "matcher": "Edit|Write|Bash",
                    "hooks": [{
                        "type": "command",
                        "command": "C:\\Users\\foo\\aiki.exe hooks stdin --agent claude-code --event PostToolUse"
                    }]
                }],
                "Stop": [{
                    "matcher": "",
                    "hooks": [{
                        "type": "command",
                        "command": "C:\\Users\\foo\\aiki.exe hooks stdin --agent claude-code --event Stop"
                    }]
                }],
                "SessionEnd": [{
                    "matcher": "",
                    "hooks": [{
                        "type": "command",
                        "command": "C:\\Users\\foo\\aiki.exe hooks stdin --agent claude-code --event SessionEnd"
                    }]
                }]
            }
        });
        write!(file, "{}", serde_json::to_string(&settings).unwrap()).unwrap();

        assert!(find_missing_claude_code_hooks(file.path()).is_empty());
    }

    #[test]
    fn test_check_cursor_hooks_with_exe() {
        let mut file = NamedTempFile::new().unwrap();
        let hooks = serde_json::json!({
            "version": 1,
            "hooks": {
                "beforeSubmitPrompt": [{
                    "command": "aiki.exe hooks stdin --agent cursor --event beforeSubmitPrompt"
                }],
                "afterFileEdit": [{
                    "command": "./aiki.exe hooks stdin --agent cursor --event afterFileEdit"
                }],
                "beforeShellExecution": [{
                    "command": "aiki.exe hooks stdin --agent cursor --event beforeShellExecution"
                }],
                "afterShellExecution": [{
                    "command": "aiki.exe hooks stdin --agent cursor --event afterShellExecution"
                }],
                "beforeMCPExecution": [{
                    "command": "aiki.exe hooks stdin --agent cursor --event beforeMCPExecution"
                }],
                "afterMCPExecution": [{
                    "command": "aiki.exe hooks stdin --agent cursor --event afterMCPExecution"
                }],
                "stop": [{
                    "command": "aiki.exe hooks stdin --agent cursor --event stop"
                }]
            }
        });
        write!(file, "{}", serde_json::to_string(&hooks).unwrap()).unwrap();

        assert!(check_cursor_hooks(file.path()));
    }

    #[test]
    fn test_find_missing_claude_code_hooks_wrong_agent() {
        let mut file = NamedTempFile::new().unwrap();
        let settings = serde_json::json!({
            "hooks": {
                "SessionStart": [{
                    "matcher": "startup",
                    "hooks": [{
                        "type": "command",
                        "command": "aiki hooks stdin --agent cursor --event session.started"
                    }]
                }],
                "PostToolUse": [{
                    "matcher": "Edit|Write",
                    "hooks": [{
                        "type": "command",
                        "command": "aiki hooks stdin --agent claude-code --event afterFileEdit"
                    }]
                }]
            }
        });
        write!(file, "{}", serde_json::to_string(&settings).unwrap()).unwrap();

        // Should report SessionStart as missing (wrong agent: cursor instead of claude-code)
        let missing = find_missing_claude_code_hooks(file.path());
        assert!(missing.contains(&"SessionStart"));
    }

    #[test]
    fn test_check_cursor_hooks_wrong_event() {
        let mut file = NamedTempFile::new().unwrap();
        let hooks = serde_json::json!({
            "version": 1,
            "hooks": {
                "beforeSubmitPrompt": [{
                    "command": "aiki hooks stdin --agent cursor --event session.started"
                }],
                "afterFileEdit": [{
                    "command": "aiki hooks stdin --agent cursor --event afterFileEdit"
                }]
            }
        });
        write!(file, "{}", serde_json::to_string(&hooks).unwrap()).unwrap();

        // Should fail: beforeSubmitPrompt has wrong event (session.started instead of beforeSubmitPrompt)
        assert!(!check_cursor_hooks(file.path()));
    }

    // Tests for plugin resolution

    #[test]
    fn test_is_plugin_resolvable_builtin() {
        let temp = tempfile::tempdir().unwrap();
        assert!(is_plugin_resolvable("aiki/default", temp.path()));
        assert!(is_plugin_resolvable("aiki/git-coauthors", temp.path()));
        assert!(is_plugin_resolvable("aiki/review-loop", temp.path()));
    }

    #[test]
    fn test_is_plugin_resolvable_unknown() {
        let temp = tempfile::tempdir().unwrap();
        assert!(!is_plugin_resolvable("aiki/nonexistent", temp.path()));
        assert!(!is_plugin_resolvable("unknown/plugin", temp.path()));
    }

    #[test]
    fn test_is_plugin_resolvable_project_file() {
        let temp = tempfile::tempdir().unwrap();
        let plugin_dir = temp.path().join(".aiki/hooks/myorg");
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::write(plugin_dir.join("myplugin.yml"), "name: test\n").unwrap();
        assert!(is_plugin_resolvable("myorg/myplugin", temp.path()));
    }

    // Tests for event name validation

    #[test]
    fn test_suggest_event_typo() {
        assert_eq!(suggest_event("session.strated"), Some("session.started"));
        assert_eq!(suggest_event("turn.complted"), Some("turn.completed"));
        assert_eq!(suggest_event("sesion.started"), Some("session.started"));
    }

    #[test]
    fn test_suggest_event_no_match() {
        assert_eq!(suggest_event("completely.unknown.event"), None);
    }

    #[test]
    fn test_edit_distance() {
        assert_eq!(edit_distance("abc", "abc"), 0);
        assert_eq!(edit_distance("abc", "abd"), 1);
        assert_eq!(edit_distance("abc", "abcd"), 1);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }

    #[test]
    fn test_validate_event_keys_valid() {
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"
            name: test
            include:
              - aiki/default
            session.started:
              - context: "hello"
            turn.completed:
              - context: "done"
            "#,
        )
        .unwrap();
        let mapping = yaml.as_mapping().unwrap();
        assert_eq!(validate_event_keys(mapping, ""), 0);
    }

    #[test]
    fn test_validate_event_keys_unknown() {
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"
            session.starting:
              - context: "hello"
            "#,
        )
        .unwrap();
        let mapping = yaml.as_mapping().unwrap();
        assert_eq!(validate_event_keys(mapping, ""), 1);
    }
}
