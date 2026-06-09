use crate::commands::agents_template::aiki_block_hash;
use crate::commands::zed_detection;
use crate::config;
use crate::editors::zed as ide_config;
use crate::error::Result;
use crate::instructions;
use crate::prerequisites::{check_command_version, PREREQUISITES};
use crate::repos::RepoDetector;
use crate::tasks::templates::builtin::default_plugin_templates;
use crate::tasks::templates::manifest::{checksum, FileEntry, RepoManifest};
use crate::tasks::templates::sync::sync_plugin_templates;
use crate::tasks::templates::TASKS_DIR_NAME;
use anyhow::Context;
use std::env;
use std::fs;

pub fn run(fix: bool) -> Result<()> {
    let mut issues_found = 0;
    let fixes_applied = 0;

    if fix {
        println!("Diagnosing and fixing issues...\n");
    } else {
        println!("Checking Aiki health...\n");
    }

    // Check prerequisites
    println!("Prerequisites:");

    for &(cmd, description) in PREREQUISITES {
        match check_command_version(cmd) {
            Some(version) => {
                println!("  ✓ {} ({})", description, version);
            }
            None => {
                println!("  ✗ {} not found", description);
                println!("    → Install {} and ensure it's on your PATH", description);
                issues_found += 1;
            }
        }
    }

    println!();

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
        let jj_ws = crate::jj::JJWorkspace::new(&project_root);
        if jj_ws.is_healthy_non_colocated() {
            println!("  ✓ JJ workspace initialized");
        } else {
            println!("  ⚠ JJ workspace exists but is not non-colocated");
            println!("    This may be your own jj repo, or was created by a newer jj");
            println!("    that defaults to --colocate");
            println!("    Warning: if you use jj for version control, removing .jj will delete your jj history");
            println!("    Run: rm -rf .jj && aiki init");
            issues_found += 1;
        }
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
    let claude_status = find_missing_claude_code_hooks(&claude_settings);
    if claude_status.missing.is_empty() && claude_status.old_format.is_empty() {
        println!("  ✓ Claude Code hooks configured");
    } else if claude_status.missing.is_empty() {
        // All hooks present but some use old format
        println!("  ✓ Claude Code hooks configured");
        println!("  ⚠ Claude Code hooks use deprecated format (--agent/--event → --claude shorthand)");
        if fix {
            match migrate_old_format_hooks_in_file(&claude_settings, "claude-code", "--claude") {
                Ok(count) if count > 0 => {
                    println!("    ✓ Migrated {} hook command(s) to new format", count);
                }
                Ok(_) => {}
                Err(e) => {
                    println!("    ✗ Failed to migrate: {}", e);
                }
            }
        } else {
            println!("    → Run: aiki doctor --fix");
        }
    } else {
        println!(
            "  ✗ Claude Code hooks: missing {}",
            claude_status.missing.join(", ")
        );
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
    let cursor_status = check_cursor_hooks(&cursor_hooks_path);
    if cursor_status.all_present && !cursor_status.has_old_format {
        println!("  ✓ Cursor hooks configured");
    } else if cursor_status.all_present && cursor_status.has_old_format {
        println!("  ✓ Cursor hooks configured");
        println!("  ⚠ Cursor hooks use deprecated format (--agent/--event → --cursor shorthand)");
        if fix {
            match migrate_old_format_hooks_in_file(&cursor_hooks_path, "cursor", "--cursor") {
                Ok(count) if count > 0 => {
                    println!("    ✓ Migrated {} hook command(s) to new format", count);
                }
                Ok(_) => {}
                Err(e) => {
                    println!("    ✗ Failed to migrate: {}", e);
                }
            }
        } else {
            println!("    → Run: aiki doctor --fix");
        }
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

    // Check Codex hooks - verify config.toml has OTel + hooks.json has hook definitions
    let codex_config_path = home_dir.join(".codex/config.toml");
    let codex_hooks_path = home_dir.join(".codex/hooks.json");
    let codex_status = check_codex_hooks(&codex_config_path, &codex_hooks_path);
    if codex_status.all_present && !codex_status.has_old_format {
        println!("  ✓ Codex hooks configured");
    } else if codex_status.all_present && codex_status.has_old_format {
        println!("  ✓ Codex hooks configured");
        println!("  ⚠ Codex hooks use deprecated format (--agent/--event → --codex shorthand)");
        if fix {
            match migrate_old_format_hooks_in_file(&codex_hooks_path, "codex", "--codex") {
                Ok(count) if count > 0 => {
                    println!("    ✓ Migrated {} hook command(s) to new format", count);
                }
                Ok(_) => {}
                Err(e) => {
                    println!("    ✗ Failed to migrate: {}", e);
                }
            }
        } else {
            println!("    → Run: aiki doctor --fix");
        }
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
        println!(
            "  {} OTel receiver",
            if otel_receiver_ok { "✓" } else { "✗" }
        );
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

    // Check instruction files (AGENTS.md / CLAUDE.md)
    println!("Agent Instructions:");

    issues_found += check_instruction_files(&project_root, fix);

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
                                        if let Some(block_includes) =
                                            block.get("include").and_then(|v| v.as_sequence())
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
                match fs::write(&hooks_yml_path, super::init::HOOKS_YML_TEMPLATE) {
                    Ok(()) => {
                        println!("    ✓ Created .aiki/hooks.yml with default workflow automation");
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

        // Plugin dependency graph health checks
        issues_found += check_plugin_graph(&project_root);

        println!();
    }

    // Check template health
    issues_found += check_templates(&project_root, fix);

    println!();

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

/// Check instruction file health using `RepoInstructionsKind`.
///
/// Reports per-variant diagnostics and, when `fix` is true, calls
/// `ensure_instruction_files` to repair the setup. Returns issue count.
fn check_instruction_files(project_root: &std::path::Path, fix: bool) -> usize {
    use instructions::RepoInstructionsKind;

    let kind = instructions::detect_instructions_kind(project_root);

    if fix {
        // Let ensure_instruction_files handle all variants
        if let Err(e) = instructions::ensure_instruction_files(project_root, false) {
            println!("  ✗ Failed to fix instruction files: {}", e);
            return 1;
        }
        return 0;
    }

    // Check-only mode: report status per variant
    let mut issues = 0;

    match &kind {
        RepoInstructionsKind::FileWithSymlink { canonical, symlink } => {
            println!("  ✓ Canonical instruction file: {}", canonical);
            println!("  ✓ {} symlinked to {}", symlink, canonical);
        }
        RepoInstructionsKind::BothFiles => {
            println!("  ⚠ Both AGENTS.md and CLAUDE.md exist as separate files (not symlinked)");
            println!("    Aiki will write the <aiki> block to both files.");
        }
        RepoInstructionsKind::FileWithoutSymlink { existing, missing } => {
            println!("  ✓ Found {}", existing);
            println!("  ⚠ {} not found (no symlink)", missing);
            println!("    → Run: aiki doctor --fix (to create symlink)");
            issues += 1;
        }
        RepoInstructionsKind::BothSymlinks => {
            println!("  ⚠ Both AGENTS.md and CLAUDE.md are symlinks to external files");
            println!("    Aiki cannot safely write through external symlinks.");
            println!("    → Replace one symlink with a regular file, then run: aiki init");
            issues += 1;
        }
        RepoInstructionsKind::Missing => {
            println!("  ✗ Neither AGENTS.md nor CLAUDE.md found");
            println!("    → Run: aiki doctor --fix (to create files)");
            issues += 1;
        }
    }

    // Check <aiki> block in real files (not for Missing or BothSymlinks)
    let files_to_check: Vec<&str> = match &kind {
        RepoInstructionsKind::FileWithSymlink { canonical, .. } => vec![canonical],
        RepoInstructionsKind::BothFiles => vec![instructions::AGENTS_MD, instructions::CLAUDE_MD],
        RepoInstructionsKind::FileWithoutSymlink { existing, .. } => vec![existing],
        _ => vec![],
    };

    for filename in files_to_check {
        let file_path = project_root.join(filename);
        match std::fs::read_to_string(&file_path) {
            Ok(content) => {
                let expected_hash = aiki_block_hash();
                let has_current_hash =
                    content.contains(&format!("hash=\"{}\"", expected_hash));
                if content.contains("<aiki version=") && has_current_hash {
                    println!("  ✓ {} has current <aiki> block", filename);
                } else if content.contains("<aiki version=") {
                    println!("  ⚠ {} has outdated <aiki> block", filename);
                    println!("    → Run: aiki doctor --fix (to update block)");
                    issues += 1;
                } else {
                    println!("  ⚠ {} missing <aiki> block", filename);
                    println!("    → Run: aiki doctor --fix (to add block)");
                    issues += 1;
                }
            }
            Err(e) => {
                println!("  ✗ Failed to read {}: {}", filename, e);
                issues += 1;
            }
        }
    }

    issues
}

/// Check template health: manifest presence, schema compatibility, missing/dirty files, legacy dirs.
///
/// Returns the number of issues found.
fn check_templates(project_root: &std::path::Path, fix: bool) -> usize {
    let aiki_dir = project_root.join(".aiki");
    if !aiki_dir.exists() {
        return 0;
    }

    println!("Templates:");

    let manifest_path = aiki_dir.join(".manifest.json");
    let templates_dir = aiki_dir.join(TASKS_DIR_NAME);

    // (a) Check manifest existence and schema
    if manifest_path.exists() {
        // Try to parse just the schema field
        let content = match fs::read_to_string(&manifest_path) {
            Ok(c) => c,
            Err(e) => {
                println!("  ✗ Failed to read manifest: {}", e);
                return 1;
            }
        };

        #[derive(serde::Deserialize)]
        struct SchemaCheck {
            schema: Option<u32>,
        }

        let schema = match serde_json::from_str::<SchemaCheck>(&content) {
            Ok(check) => check.schema.unwrap_or(1),
            Err(_) => {
                println!("  ✗ Corrupt manifest at .aiki/.manifest.json");
                return 1;
            }
        };

        // CURRENT_SCHEMA is 1 in manifest.rs
        if schema > 1 {
            println!(
                "  ✗ Manifest schema version {} is not supported by this CLI (supports version 1). Upgrade your CLI to manage templates."
            , schema);
            // Can't safely proceed — skip all other template checks
            return 1;
        }
    } else {
        println!("  ⚠ Templates not managed yet, run 'aiki init'");
        return 0;
    }

    // Load manifest for detailed checks
    let manifest = match RepoManifest::load(project_root) {
        Ok(Some(m)) => m,
        Ok(None) => {
            // Shouldn't happen since we checked exists above, but be safe
            println!("  ⚠ Templates not managed yet, run 'aiki init'");
            return 0;
        }
        Err(e) => {
            println!("  ✗ Failed to load manifest: {}", e);
            return 1;
        }
    };

    // Accumulates issues across all plugins (intentionally outside the per-plugin loop)
    let mut issues = 0;

    // Get the source templates for comparison
    let source_templates = default_plugin_templates();

    // Build source checksum lookup for detecting dirty adoptions.
    // A dirty-adopted file has manifest checksum == source checksum but disk != manifest,
    // which is expected (not a real modification).
    let source_checksums: std::collections::HashMap<&str, String> = source_templates
        .iter()
        .map(|(path, content)| (*path, checksum(content)))
        .collect();

    // Check each plugin in manifest
    for (plugin_ref, plugin_entry) in &manifest.templates {
        let install_root = &plugin_entry.install_root;
        let mut up_to_date = 0usize;
        let mut missing_files: Vec<String> = Vec::new();
        let mut dirty_files: Vec<String> = Vec::new();
        let mut adopted_files: Vec<String> = Vec::new();
        let mut stale_manifest_files: Vec<String> = Vec::new();

        for (file_name, file_entry) in &plugin_entry.files {
            let disk_path = templates_dir.join(install_root).join(file_name);

            if !disk_path.exists() {
                // (c) Missing on-disk file
                missing_files.push(file_name.clone());
            } else {
                // Check if dirty
                if let Ok(on_disk) = fs::read(&disk_path) {
                    let disk_cksum = checksum(&on_disk);
                    if disk_cksum != file_entry.checksum {
                        // Disk doesn't match manifest — but why?
                        //
                        // Case 1: Stale manifest — disk matches current source template.
                        // This happens when templates are git-tracked and updated via
                        // commits, but the gitignored manifest wasn't re-synced.
                        // Treat as up-to-date (manifest is just behind).
                        let matches_source = source_checksums
                            .get(file_name.as_str())
                            .map_or(false, |src_cksum| *src_cksum == disk_cksum);

                        // Case 2: Dirty adoption — manifest matches source but disk
                        // differs. File was pre-existing when sync adopted it.
                        let is_dirty_adoption = source_checksums
                            .get(file_name.as_str())
                            .map_or(false, |src_cksum| *src_cksum == file_entry.checksum);

                        if matches_source {
                            stale_manifest_files.push(file_name.clone());
                            up_to_date += 1;
                        } else if is_dirty_adoption {
                            adopted_files.push(file_name.clone());
                        } else {
                            dirty_files.push(file_name.clone());
                        }
                    } else {
                        up_to_date += 1;
                    }
                } else {
                    println!("    ✗ Cannot read template: {}", file_name);
                    issues += 1;
                }
            }
        }

        // Print summary header
        println!("  {}:", plugin_ref);

        if up_to_date > 0 {
            println!("    ✓ {} template(s) up to date", up_to_date);
        }

        // Stale manifest: disk matches source but manifest is outdated.
        // Auto-repair the manifest since the files are already correct.
        // Intentionally ungated on `fix`: this only updates manifest metadata
        // (checksums/timestamps) to match files that are already correct on disk,
        // so it's always safe and avoids noisy false positives on future runs.
        if !stale_manifest_files.is_empty() {
            if let Ok(Some(mut fix_manifest)) = RepoManifest::load(project_root) {
                if let Some(plugin) = fix_manifest.templates.get_mut(plugin_ref) {
                    let now = chrono::Utc::now().to_rfc3339();
                    for file_name in &stale_manifest_files {
                        if let Some(src_cksum) = source_checksums.get(file_name.as_str()) {
                            plugin.files.insert(
                                file_name.clone(),
                                FileEntry {
                                    checksum: src_cksum.clone(),
                                    version: plugin
                                        .files
                                        .get(file_name)
                                        .and_then(|e| e.version.clone()),
                                    installed_at: now.clone(),
                                },
                            );
                        }
                    }
                    plugin.source_version = env!("CARGO_PKG_VERSION").to_string();
                    let _ = fix_manifest.save(project_root);
                }
            }
        }

        // (d) Dirty/modified templates
        if !dirty_files.is_empty() {
            println!(
                "    ⚠ {} template(s) modified locally: {}",
                dirty_files.len(),
                dirty_files.join(", ")
            );
            println!("      (delete and re-init to reset)");
        }

        // Dirty-adopted templates: pre-existing user files adopted into manifest.
        // The disk/manifest mismatch is expected — not a problem.
        if !adopted_files.is_empty() {
            println!(
                "    ✓ {} template(s) customized (pre-existing): {}",
                adopted_files.len(),
                adopted_files.join(", ")
            );
        }

        // (c) Missing files
        if !missing_files.is_empty() {
            if fix {
                // Re-install missing templates via sync
                if plugin_ref == "aiki/default" {
                    let mut fix_manifest = RepoManifest::load(project_root)
                        .unwrap()
                        .unwrap_or_else(RepoManifest::new);
                    match std::fs::create_dir_all(&templates_dir) {
                        Ok(_) => {
                            match sync_plugin_templates(
                                &mut fix_manifest,
                                plugin_ref,
                                install_root,
                                env!("CARGO_PKG_VERSION"),
                                &source_templates,
                                &templates_dir,
                            ) {
                                Ok(report) => {
                                    if report.installed > 0 {
                                        println!(
                                            "    ✓ Reinstalled {} template(s): {}",
                                            report.installed,
                                            report.installed_names.join(", ")
                                        );
                                    }
                                    let _ = fix_manifest.save(project_root);
                                }
                                Err(e) => {
                                    println!("    ✗ Failed to reinstall templates: {}", e);
                                    issues += missing_files.len();
                                }
                            }
                        }
                        Err(err) => {
                            println!("    ✗ Failed to create tasks directory: {}", err);
                            issues += missing_files.len();
                        }
                    }
                } else {
                    for name in &missing_files {
                        println!(
                            "    ⚠ Template '{}' is in manifest but missing from disk. Cannot auto-fix non-default plugin.",
                            name
                        );
                    }
                    issues += missing_files.len();
                }
            } else {
                for name in &missing_files {
                    println!(
                        "    ⚠ Template '{}' is in manifest but missing from disk. Run 'aiki doctor --fix' to reinstall.",
                        name
                    );
                }
                issues += missing_files.len();
            }
        }
    }

    // If manifest has no templates at all, just show the count
    if manifest.templates.is_empty() {
        println!("  ✓ Manifest present (no templates tracked)");
    }

    // (e) Check for stale legacy directory
    let legacy_dir = templates_dir.join("aiki");
    if legacy_dir.is_dir() {
        println!(
            "  ⚠ Legacy template directory found: .aiki/{}/aiki/",
            TASKS_DIR_NAME
        );
        println!("    Run 'aiki init' to migrate, or remove manually if empty");
        issues += 1;
    }

    issues
}

/// Check plugin dependency graph health: missing deps, cycles, orphans.
///
/// Returns the number of issues found.
fn check_plugin_graph(project_root: &std::path::Path) -> usize {
    let plugins_base = match crate::plugins::plugins_base_dir() {
        Ok(p) => p,
        Err(_) => return 0,
    };

    let graph = crate::plugins::graph::PluginGraph::build(&plugins_base);

    let mut issues = 0;

    // Check for missing dependencies
    let missing = graph.missing_dependencies();
    if !missing.is_empty() {
        for dep in missing {
            println!("  ✗ Missing dependency: {} (referenced but not installed)", dep);
            println!("    → Run: aiki plugin install {}", dep);
            issues += 1;
        }
    }

    // Check for dependency cycles
    let cycles = graph.cycles();
    if !cycles.is_empty() {
        for cycle in &cycles {
            let names: Vec<String> = cycle.iter().map(|r| r.to_string()).collect();
            println!("  ✗ Dependency cycle detected: {}", names.join(" → "));
            issues += 1;
        }
    }

    issues
}

/// Result of checking whether a command string matches an expected aiki hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HookFormatMatch {
    /// Command uses the new shorthand format (e.g. `--claude SessionStart`)
    NewFormat,
    /// Command uses the deprecated old format (e.g. `--agent claude-code --event SessionStart`)
    OldFormat,
    /// Command does not match the expected hook
    NoMatch,
}

impl HookFormatMatch {
    fn is_match(self) -> bool {
        matches!(self, Self::NewFormat | Self::OldFormat)
    }
}

/// Check if a command string invokes aiki hooks stdin with specific agent/event
///
/// Accepts both old and new flag formats:
/// - Old: `aiki hooks stdin --agent claude-code --event SessionStart`
/// - New: `aiki hooks stdin --claude SessionStart`
///
/// If expected_agent or expected_event is Some, validates those flags are present.
/// Returns a `HookFormatMatch` indicating whether the command matches and which format it uses.
fn is_aiki_hooks_command_with_params(
    cmd: &str,
    expected_agent: Option<&str>,
    expected_event: Option<&str>,
) -> HookFormatMatch {
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
        return HookFormatMatch::NoMatch;
    }

    // If no specific agent/event required, we're done (treat as new format)
    if expected_agent.is_none() && expected_event.is_none() {
        return HookFormatMatch::NewFormat;
    }

    // Map canonical agent names to their shorthand flags
    let shorthand_flag = expected_agent.and_then(|a| match a {
        "claude-code" => Some("--claude"),
        "codex" => Some("--codex"),
        "cursor" => Some("--cursor"),
        "gemini" => Some("--gemini"),
        _ => None,
    });

    // New format: --<agent-shorthand> <event>  (e.g. --claude SessionStart)
    let new_format_matches = match (shorthand_flag, expected_event) {
        (Some(flag), Some(event)) => words.windows(2).any(|w| w[0] == flag && w[1] == event),
        (Some(flag), None) => words.iter().any(|w| *w == flag),
        _ => false,
    };

    if new_format_matches {
        return HookFormatMatch::NewFormat;
    }

    // Old format: --agent <name> --event <event>
    let old_format_matches = {
        let agent_ok = expected_agent
            .map(|agent| words.windows(2).any(|w| w[0] == "--agent" && w[1] == agent))
            .unwrap_or(true);
        let event_ok = expected_event
            .map(|event| words.windows(2).any(|w| w[0] == "--event" && w[1] == event))
            .unwrap_or(true);
        agent_ok && event_ok
    };

    if old_format_matches {
        return HookFormatMatch::OldFormat;
    }

    HookFormatMatch::NoMatch
}

/// Result of checking Claude Code hooks: which are missing and which use old format.
struct ClaudeHookStatus {
    /// Hook names that are completely missing (not present in any format).
    missing: Vec<&'static str>,
    /// Hook names that are present but use the deprecated `--agent/--event` format.
    old_format: Vec<&'static str>,
}

/// Required Claude Code hooks (must match what `config::install_claude_code_hooks_global` installs).
const REQUIRED_CLAUDE_HOOKS: &[&str] = &[
    "SessionStart",
    "PreCompact",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Stop",
    "SessionEnd",
];

/// Find which Claude Code hooks are missing from ~/.claude/settings.json
///
/// Returns a `ClaudeHookStatus` with both missing hooks and old-format hooks.
fn find_missing_claude_code_hooks(settings_path: &std::path::Path) -> ClaudeHookStatus {
    let all_missing = || ClaudeHookStatus {
        missing: REQUIRED_CLAUDE_HOOKS.to_vec(),
        old_format: Vec::new(),
    };

    if !settings_path.exists() {
        return all_missing();
    }

    let content = match fs::read_to_string(settings_path) {
        Ok(c) => c,
        Err(_) => return all_missing(),
    };

    let settings: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return all_missing(),
    };

    let hooks = match settings.get("hooks") {
        Some(h) => h,
        None => return all_missing(),
    };

    // Helper to check if a Claude Code hook entry contains aiki command with correct params
    let check_hook = |hook_name: &str| -> HookFormatMatch {
        hooks
            .get(hook_name)
            .and_then(|arr| arr.as_array())
            .and_then(|arr| {
                for entry in arr {
                    if let Some(hooks_arr) = entry.get("hooks").and_then(|h| h.as_array()) {
                        for hook in hooks_arr {
                            if let Some(cmd) = hook.get("command").and_then(|c| c.as_str()) {
                                let m = is_aiki_hooks_command_with_params(
                                    cmd,
                                    Some("claude-code"),
                                    Some(hook_name),
                                );
                                if m.is_match() {
                                    return Some(m);
                                }
                            }
                        }
                    }
                }
                None
            })
            .unwrap_or(HookFormatMatch::NoMatch)
    };

    let mut missing = Vec::new();
    let mut old_format = Vec::new();
    for name in REQUIRED_CLAUDE_HOOKS {
        match check_hook(name) {
            HookFormatMatch::NewFormat => {}
            HookFormatMatch::OldFormat => old_format.push(*name),
            HookFormatMatch::NoMatch => missing.push(*name),
        }
    }

    ClaudeHookStatus { missing, old_format }
}

/// Result of checking Cursor hooks.
struct CursorHookStatus {
    /// true when all required hooks are present (in either format).
    all_present: bool,
    /// true when at least one hook uses the deprecated `--agent/--event` format.
    has_old_format: bool,
}

/// Required Cursor hooks.
const REQUIRED_CURSOR_HOOKS: &[&str] = &[
    "beforeSubmitPrompt",
    "afterFileEdit",
    "beforeShellExecution",
    "afterShellExecution",
    "beforeMCPExecution",
    "afterMCPExecution",
    "stop",
];

/// Check if Cursor hooks are properly configured
///
/// Returns a `CursorHookStatus` indicating completeness and whether old format is used.
fn check_cursor_hooks(hooks_path: &std::path::Path) -> CursorHookStatus {
    let not_configured = CursorHookStatus {
        all_present: false,
        has_old_format: false,
    };

    if !hooks_path.exists() {
        return not_configured;
    }

    let content = match fs::read_to_string(hooks_path) {
        Ok(c) => c,
        Err(_) => return not_configured,
    };

    let config: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return not_configured,
    };

    let hooks = match config.get("hooks") {
        Some(h) => h,
        None => return not_configured,
    };

    // Helper to check if an array contains an aiki hooks stdin command with specific agent/event
    let check_aiki_hook = |arr: &serde_json::Value, agent: &str, event: &str| -> HookFormatMatch {
        arr.as_array()
            .and_then(|arr| {
                for hook in arr {
                    if let Some(cmd) = hook.get("command").and_then(|c| c.as_str()) {
                        let m = is_aiki_hooks_command_with_params(cmd, Some(agent), Some(event));
                        if m.is_match() {
                            return Some(m);
                        }
                    }
                }
                None
            })
            .unwrap_or(HookFormatMatch::NoMatch)
    };

    let mut all_present = true;
    let mut has_old_format = false;
    for hook_name in REQUIRED_CURSOR_HOOKS {
        match hooks
            .get(*hook_name)
            .map(|arr| check_aiki_hook(arr, "cursor", hook_name))
            .unwrap_or(HookFormatMatch::NoMatch)
        {
            HookFormatMatch::NewFormat => {}
            HookFormatMatch::OldFormat => has_old_format = true,
            HookFormatMatch::NoMatch => all_present = false,
        }
    }

    CursorHookStatus {
        all_present,
        has_old_format,
    }
}

/// Result of checking Codex hooks.
struct CodexHookStatus {
    /// true when config.toml and hooks.json are properly configured.
    all_present: bool,
    /// true when at least one hook uses the deprecated `--agent/--event` format.
    has_old_format: bool,
}

/// Check if Codex hooks are properly configured
///
/// Returns a `CodexHookStatus` indicating completeness and whether old format is used.
fn check_codex_hooks(config_path: &std::path::Path, hooks_path: &std::path::Path) -> CodexHookStatus {
    let not_configured = CodexHookStatus {
        all_present: false,
        has_old_format: false,
    };

    let check_aiki_hook = |arr: &serde_json::Value, agent: &str, event: &str| -> HookFormatMatch {
        arr.as_array()
            .and_then(|arr| {
                for hook in arr {
                    if let Some(cmd) = hook.get("command").and_then(|c| c.as_str()) {
                        let m = is_aiki_hooks_command_with_params(cmd, Some(agent), Some(event));
                        if m.is_match() {
                            return Some(m);
                        }
                    }
                }
                None
            })
            .unwrap_or(HookFormatMatch::NoMatch)
    };

    let config_ok = config_path
        .exists()
        .then(|| fs::read_to_string(config_path).ok())
        .flatten()
        .and_then(|content| toml::from_str::<toml::Value>(&content).ok())
        .map(|config| {
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

            let global_aiki = crate::global::global_aiki_dir();
            let global_aiki = global_aiki.to_string_lossy().to_string();

            let has_writable_root = config
                .get("sandbox_workspace_write")
                .and_then(|v| v.as_table())
                .and_then(|t| t.get("writable_roots"))
                .and_then(|v| v.as_array())
                .map(|roots| {
                    roots
                        .iter()
                        .any(|v| v.as_str().is_some_and(|s| s == global_aiki))
                })
                .unwrap_or(false);

            let has_hooks_enabled = config
                .get("features")
                .and_then(|v| v.as_table())
                .and_then(|t| t.get("codex_hooks"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            has_otel && has_writable_root && has_hooks_enabled
        })
        .unwrap_or(false);

    if !config_ok {
        return not_configured;
    }

    let hooks_result = hooks_path
        .exists()
        .then(|| fs::read_to_string(hooks_path).ok())
        .flatten()
        .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
        .and_then(|json| {
            let hooks = json.get("hooks").and_then(|v| v.as_object())?;
            let mut all_present = true;
            let mut has_old_format = false;
            for (name, event) in [
                ("SessionStart", "sessionStart"),
                ("UserPromptSubmit", "userPromptSubmit"),
                ("PreToolUse", "preToolUse"),
                ("Stop", "stop"),
            ] {
                let matched = hooks
                    .get(name)
                    .and_then(|v| v.as_array())
                    .and_then(|arr| {
                        for group in arr {
                            if let Some(nested) = group.get("hooks") {
                                let m = check_aiki_hook(nested, "codex", event);
                                if m.is_match() {
                                    return Some(m);
                                }
                            }
                        }
                        None
                    })
                    .unwrap_or(HookFormatMatch::NoMatch);

                match matched {
                    HookFormatMatch::NewFormat => {}
                    HookFormatMatch::OldFormat => has_old_format = true,
                    HookFormatMatch::NoMatch => all_present = false,
                }
            }
            Some(CodexHookStatus {
                all_present,
                has_old_format,
            })
        });

    hooks_result.unwrap_or(not_configured)
}

/// Migrate old-format hook commands in a JSON settings file to the new shorthand.
///
/// Walks all string values in the JSON looking for `--agent <agent_name> --event <EVENT>`
/// and replaces with `<shorthand_flag> <EVENT>`.
///
/// Returns the number of command strings that were rewritten.
fn migrate_old_format_hooks_in_file(
    path: &std::path::Path,
    agent_name: &str,
    shorthand_flag: &str,
) -> anyhow::Result<usize> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let mut json: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse {}", path.display()))?;

    let count = migrate_old_format_in_value(&mut json, agent_name, shorthand_flag);

    if count > 0 {
        let output = serde_json::to_string_pretty(&json)?;
        fs::write(path, output.as_bytes())
            .with_context(|| format!("Failed to write {}", path.display()))?;
    }

    Ok(count)
}

/// Recursively walk a JSON value, rewriting old-format hook command strings.
///
/// Returns the number of strings rewritten.
fn migrate_old_format_in_value(
    value: &mut serde_json::Value,
    agent_name: &str,
    shorthand_flag: &str,
) -> usize {
    match value {
        serde_json::Value::String(s) => {
            // Only rewrite if this looks like an aiki hooks command with old format
            if s.contains("--agent") && s.contains("--event") && s.contains("hooks") && s.contains("stdin") {
                let old_pattern = format!("--agent {} --event", agent_name);
                if s.contains(&old_pattern) {
                    *s = s.replace(&old_pattern, shorthand_flag);
                    return 1;
                }
            }
            0
        }
        serde_json::Value::Array(arr) => arr.iter_mut().map(|v| migrate_old_format_in_value(v, agent_name, shorthand_flag)).sum(),
        serde_json::Value::Object(obj) => obj.values_mut().map(|v| migrate_old_format_in_value(v, agent_name, shorthand_flag)).sum(),
        _ => 0,
    }
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
            if !KNOWN_EVENTS.contains(&key_str) && !crate::flows::sugar::is_sugar_pattern(key_str) {
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

    struct EnvGuard {
        name: String,
        original: Option<String>,
    }

    impl EnvGuard {
        fn set(name: &str, value: &str, _proof: &std::sync::MutexGuard<'_, ()>) -> Self {
            let original = std::env::var(name).ok();
            // SAFETY: Thread safety is handled by AIKI_HOME_TEST_MUTEX; the `_proof`
            // parameter guarantees the caller holds the mutex lock.
            unsafe {
                std::env::set_var(name, value);
            }
            Self {
                name: name.to_string(),
                original,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: Thread safety is handled by AIKI_HOME_TEST_MUTEX; these tests
            // run serially under the mutex lock.
            match &self.original {
                Some(v) => unsafe { std::env::set_var(&self.name, v) },
                None => unsafe { std::env::remove_var(&self.name) },
            }
        }
    }

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

        let status = find_missing_claude_code_hooks(file.path());
        assert!(status.missing.is_empty());
        // Old format commands: all hooks are old format
        assert_eq!(status.old_format.len(), 7);
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

        let status = find_missing_claude_code_hooks(file.path());
        assert_eq!(status.missing, vec!["PreCompact", "SessionEnd"]);
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

        let status = find_missing_claude_code_hooks(file.path());
        assert!(status.missing.contains(&"PostToolUse"));
        assert!(status.missing.contains(&"PreCompact"));
        assert!(status.missing.contains(&"SessionEnd"));
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

        let status = find_missing_claude_code_hooks(file.path());
        assert!(status.missing.contains(&"SessionStart")); // wrong command
    }

    #[test]
    fn test_find_missing_claude_code_hooks_no_file() {
        let path = std::path::Path::new("/nonexistent/path/settings.json");
        assert_eq!(find_missing_claude_code_hooks(path).missing.len(), 7); // all hooks missing
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

        let status = check_cursor_hooks(file.path());
        assert!(status.all_present);
        assert!(status.has_old_format); // these use --agent/--event
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

        assert!(!check_cursor_hooks(file.path()).all_present);
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

        assert!(!check_cursor_hooks(file.path()).all_present);
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

        assert!(!check_cursor_hooks(file.path()).all_present);
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

        assert!(!check_cursor_hooks(file.path()).all_present);
    }

    #[test]
    fn test_check_cursor_hooks_no_file() {
        let path = std::path::Path::new("/nonexistent/path/hooks.json");
        assert!(!check_cursor_hooks(path).all_present);
    }

    // Tests for is_aiki_hooks_command_with_params

    #[test]
    fn test_is_aiki_hooks_command_basic() {
        assert_eq!(
            is_aiki_hooks_command_with_params(
                "aiki hooks stdin --agent claude-code --event session.started",
                Some("claude-code"),
                Some("session.started")
            ),
            HookFormatMatch::OldFormat
        );
    }

    #[test]
    fn test_is_aiki_hooks_command_with_exe() {
        assert_eq!(
            is_aiki_hooks_command_with_params(
                "aiki.exe hooks stdin --agent claude-code --event session.started",
                Some("claude-code"),
                Some("session.started")
            ),
            HookFormatMatch::OldFormat
        );
    }

    #[test]
    fn test_is_aiki_hooks_command_with_path() {
        assert_eq!(
            is_aiki_hooks_command_with_params(
                "/usr/local/bin/aiki hooks stdin --agent cursor --event beforeSubmitPrompt",
                Some("cursor"),
                Some("beforeSubmitPrompt")
            ),
            HookFormatMatch::OldFormat
        );
    }

    #[test]
    fn test_is_aiki_hooks_command_with_path_and_exe() {
        assert_eq!(
            is_aiki_hooks_command_with_params(
                "C:\\Program Files\\aiki.exe hooks stdin --agent claude-code --event afterFileEdit",
                Some("claude-code"),
                Some("afterFileEdit")
            ),
            HookFormatMatch::OldFormat
        );
    }

    #[test]
    fn test_is_aiki_hooks_command_relative_path() {
        assert_eq!(
            is_aiki_hooks_command_with_params(
                "./aiki hooks stdin --agent cursor --event afterFileEdit",
                Some("cursor"),
                Some("afterFileEdit")
            ),
            HookFormatMatch::OldFormat
        );
    }

    #[test]
    fn test_is_aiki_hooks_command_wrong_agent() {
        // Should fail: command has claude-code but we expect cursor
        assert_eq!(
            is_aiki_hooks_command_with_params(
                "aiki hooks stdin --agent claude-code --event session.started",
                Some("cursor"),
                Some("session.started")
            ),
            HookFormatMatch::NoMatch
        );
    }

    #[test]
    fn test_is_aiki_hooks_command_wrong_event() {
        // Should fail: command has session.started but we expect change.completed
        assert_eq!(
            is_aiki_hooks_command_with_params(
                "aiki hooks stdin --agent claude-code --event session.started",
                Some("claude-code"),
                Some("change.completed")
            ),
            HookFormatMatch::NoMatch
        );
    }

    #[test]
    fn test_is_aiki_hooks_command_missing_agent() {
        // Should fail: no --agent flag
        assert_eq!(
            is_aiki_hooks_command_with_params(
                "aiki hooks stdin --event session.started",
                Some("claude-code"),
                Some("session.started")
            ),
            HookFormatMatch::NoMatch
        );
    }

    #[test]
    fn test_is_aiki_hooks_command_missing_event() {
        // Should fail: no --event flag
        assert_eq!(
            is_aiki_hooks_command_with_params(
                "aiki hooks stdin --agent claude-code",
                Some("claude-code"),
                Some("session.started")
            ),
            HookFormatMatch::NoMatch
        );
    }

    #[test]
    fn test_is_aiki_hooks_command_not_hooks_handle() {
        // Should fail: not "hooks stdin"
        assert_eq!(
            is_aiki_hooks_command_with_params(
                "aiki init --agent claude-code --event session.started",
                Some("claude-code"),
                Some("session.started")
            ),
            HookFormatMatch::NoMatch
        );
    }

    #[test]
    fn test_is_aiki_hooks_command_no_params_check() {
        // Should pass with no param requirements
        assert!(is_aiki_hooks_command_with_params(
            "aiki hooks stdin",
            None,
            None
        ).is_match());
    }

    #[test]
    fn test_is_aiki_hooks_command_new_claude_shorthand() {
        assert_eq!(
            is_aiki_hooks_command_with_params(
                "aiki hooks stdin --claude SessionStart",
                Some("claude-code"),
                Some("SessionStart")
            ),
            HookFormatMatch::NewFormat
        );
    }

    #[test]
    fn test_is_aiki_hooks_command_new_cursor_shorthand() {
        assert_eq!(
            is_aiki_hooks_command_with_params(
                "aiki hooks stdin --cursor beforeSubmitPrompt",
                Some("cursor"),
                Some("beforeSubmitPrompt")
            ),
            HookFormatMatch::NewFormat
        );
    }

    #[test]
    fn test_is_aiki_hooks_command_new_codex_shorthand() {
        assert_eq!(
            is_aiki_hooks_command_with_params(
                "aiki hooks stdin --codex sessionStart",
                Some("codex"),
                Some("sessionStart")
            ),
            HookFormatMatch::NewFormat
        );
    }

    #[test]
    fn test_is_aiki_hooks_command_new_shorthand_with_path() {
        assert_eq!(
            is_aiki_hooks_command_with_params(
                "/usr/local/bin/aiki hooks stdin --claude SessionStart",
                Some("claude-code"),
                Some("SessionStart")
            ),
            HookFormatMatch::NewFormat
        );
    }

    #[test]
    fn test_is_aiki_hooks_command_new_shorthand_wrong_event() {
        assert_eq!(
            is_aiki_hooks_command_with_params(
                "aiki hooks stdin --claude SessionStart",
                Some("claude-code"),
                Some("Stop")
            ),
            HookFormatMatch::NoMatch
        );
    }

    #[test]
    fn test_is_aiki_hooks_command_new_shorthand_wrong_agent() {
        assert_eq!(
            is_aiki_hooks_command_with_params(
                "aiki hooks stdin --claude SessionStart",
                Some("cursor"),
                Some("SessionStart")
            ),
            HookFormatMatch::NoMatch
        );
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

        let status = find_missing_claude_code_hooks(file.path());
        assert!(status.missing.is_empty());
        assert_eq!(status.old_format.len(), 7); // all old format
    }

    #[test]
    fn test_find_missing_claude_code_hooks_new_shorthand_format() {
        let mut file = NamedTempFile::new().unwrap();
        let settings = serde_json::json!({
            "hooks": {
                "SessionStart": [{
                    "matcher": "",
                    "hooks": [{
                        "type": "command",
                        "command": "aiki hooks stdin --claude SessionStart"
                    }]
                }],
                "PreCompact": [{
                    "matcher": "",
                    "hooks": [{
                        "type": "command",
                        "command": "aiki hooks stdin --claude PreCompact"
                    }]
                }],
                "UserPromptSubmit": [{
                    "matcher": "",
                    "hooks": [{
                        "type": "command",
                        "command": "aiki hooks stdin --claude UserPromptSubmit"
                    }]
                }],
                "PreToolUse": [{
                    "matcher": "Edit|Write|Bash",
                    "hooks": [{
                        "type": "command",
                        "command": "aiki hooks stdin --claude PreToolUse"
                    }]
                }],
                "PostToolUse": [{
                    "matcher": "Edit|Write|Bash",
                    "hooks": [{
                        "type": "command",
                        "command": "/usr/local/bin/aiki hooks stdin --claude PostToolUse"
                    }]
                }],
                "Stop": [{
                    "matcher": "",
                    "hooks": [{
                        "type": "command",
                        "command": "aiki hooks stdin --claude Stop"
                    }]
                }],
                "SessionEnd": [{
                    "matcher": "",
                    "hooks": [{
                        "type": "command",
                        "command": "aiki hooks stdin --claude SessionEnd"
                    }]
                }]
            }
        });
        write!(file, "{}", serde_json::to_string(&settings).unwrap()).unwrap();

        let status = find_missing_claude_code_hooks(file.path());
        assert!(status.missing.is_empty());
        assert!(status.old_format.is_empty()); // all new format
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

        let status = check_cursor_hooks(file.path());
        assert!(status.all_present);
        assert!(status.has_old_format); // all old format
    }

    #[test]
    fn test_check_codex_hooks_complete() {
        let _lock = crate::global::AIKI_HOME_TEST_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let fake_home = "/tmp/fake-aiki-home";
        let _guard = EnvGuard::set("AIKI_HOME", fake_home, &_lock);

        let mut config_file = NamedTempFile::new().unwrap();
        let config = format!(
            r#"
[features]
codex_hooks = true

[otel]
log_user_prompt = true

[otel.exporter.otlp-http]
endpoint = "http://127.0.0.1:19876/v1/logs"
protocol = "binary"

[sandbox_workspace_write]
writable_roots = ["{fake_home}"]
"#
        );
        write!(config_file, "{}", config).unwrap();

        let mut hooks_file = NamedTempFile::new().unwrap();
        let hooks = serde_json::json!({
            "hooks": {
                "SessionStart": [{"hooks": [{"type": "command", "command": "aiki hooks stdin --agent codex --event sessionStart"}]}],
                "UserPromptSubmit": [{"hooks": [{"type": "command", "command": "aiki hooks stdin --agent codex --event userPromptSubmit"}]}],
                "PreToolUse": [{"hooks": [{"type": "command", "command": "aiki hooks stdin --agent codex --event preToolUse"}]}],
                "Stop": [{"hooks": [{"type": "command", "command": "aiki hooks stdin --agent codex --event stop"}]}]
            }
        });
        write!(hooks_file, "{}", serde_json::to_string_pretty(&hooks).unwrap()).unwrap();

        let result = check_codex_hooks(config_file.path(), hooks_file.path());
        assert!(result.all_present);
        assert!(result.has_old_format); // old format
    }

    #[test]
    fn test_check_codex_hooks_missing_writable_root() {
        let mut config_file = NamedTempFile::new().unwrap();
        let config = r#"
[otel]
log_user_prompt = true

[otel.exporter.otlp-http]
endpoint = "http://127.0.0.1:19876/v1/logs"
protocol = "binary"
"#;
        write!(config_file, "{}", config).unwrap();

        let mut hooks_file = NamedTempFile::new().unwrap();
        let hooks = serde_json::json!({
            "hooks": {
                "SessionStart": [{"hooks": [{"type": "command", "command": "aiki hooks stdin --agent codex --event sessionStart"}]}],
                "Stop": [{"hooks": [{"type": "command", "command": "aiki hooks stdin --agent codex --event stop"}]}]
            }
        });
        write!(hooks_file, "{}", serde_json::to_string_pretty(&hooks).unwrap()).unwrap();

        assert!(!check_codex_hooks(config_file.path(), hooks_file.path()).all_present);
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
        let status = find_missing_claude_code_hooks(file.path());
        assert!(status.missing.contains(&"SessionStart"));
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
        assert!(!check_cursor_hooks(file.path()).all_present);
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

    // ---- Template health check tests ----

    #[test]
    fn test_check_templates_missing_manifest() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = dir.path();
        std::fs::create_dir_all(repo.join(".aiki")).unwrap();
        // No .manifest.json → should report warning, 0 issues (not an error)
        let issues = check_templates(repo, false);
        assert_eq!(issues, 0);
    }

    #[test]
    fn test_check_templates_unsupported_schema() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = dir.path();
        let aiki_dir = repo.join(".aiki");
        std::fs::create_dir_all(&aiki_dir).unwrap();
        std::fs::write(
            aiki_dir.join(".manifest.json"),
            r#"{"schema": 999, "templates": {}}"#,
        )
        .unwrap();

        let issues = check_templates(repo, false);
        assert_eq!(issues, 1);
    }

    #[test]
    fn test_check_templates_dirty() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = dir.path();
        let aiki_dir = repo.join(".aiki");
        let templates_dir = aiki_dir.join(TASKS_DIR_NAME);
        std::fs::create_dir_all(&templates_dir).unwrap();

        // Write a file that differs from manifest checksum
        std::fs::write(templates_dir.join("plan.md"), b"# User modified").unwrap();

        let mut manifest = RepoManifest::new();
        let plugin = manifest.get_or_create_plugin("aiki/default", "0.1.0", ".");
        plugin.files.insert(
            "plan.md".to_string(),
            crate::tasks::templates::manifest::FileEntry {
                checksum: checksum(b"# Original content"),
                version: None,
                installed_at: "2026-01-01T00:00:00Z".to_string(),
            },
        );
        manifest.save(repo).unwrap();

        let issues = check_templates(repo, false);
        // Dirty templates are informational, not issues
        assert_eq!(issues, 0);
    }

    #[test]
    fn test_check_templates_missing_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = dir.path();
        let aiki_dir = repo.join(".aiki");
        let templates_dir = aiki_dir.join(TASKS_DIR_NAME);
        std::fs::create_dir_all(&templates_dir).unwrap();

        // Manifest says file exists but it doesn't on disk
        let mut manifest = RepoManifest::new();
        let plugin = manifest.get_or_create_plugin("aiki/default", "0.1.0", ".");
        plugin.files.insert(
            "ghost.md".to_string(),
            crate::tasks::templates::manifest::FileEntry {
                checksum: checksum(b"ghost"),
                version: None,
                installed_at: "2026-01-01T00:00:00Z".to_string(),
            },
        );
        manifest.save(repo).unwrap();

        let issues = check_templates(repo, false);
        assert_eq!(issues, 1);
    }

    #[test]
    fn test_check_templates_fix_reinstalls_missing() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = dir.path();
        let aiki_dir = repo.join(".aiki");
        let templates_dir = aiki_dir.join(TASKS_DIR_NAME);
        std::fs::create_dir_all(&templates_dir).unwrap();

        // Set up manifest with a file that's in the default plugin source but missing on disk
        let source_templates = default_plugin_templates();
        let (first_name, _first_content) = source_templates[0];

        let mut manifest = RepoManifest::new();
        let plugin = manifest.get_or_create_plugin("aiki/default", "0.1.0", ".");
        plugin.files.insert(
            first_name.to_string(),
            crate::tasks::templates::manifest::FileEntry {
                checksum: checksum(b"old content"),
                version: None,
                installed_at: "2026-01-01T00:00:00Z".to_string(),
            },
        );
        manifest.save(repo).unwrap();

        // File doesn't exist on disk
        assert!(!templates_dir.join(first_name).exists());

        // Run with fix=true
        let issues = check_templates(repo, true);
        assert_eq!(issues, 0);

        // File should now exist
        assert!(templates_dir.join(first_name).exists());
    }

    #[test]
    fn test_check_templates_stale_manifest_auto_repairs() {
        // Scenario: templates are git-tracked and updated via commits, but the
        // gitignored manifest still has old checksums. Doctor should recognize
        // that on-disk content matches the current source and auto-repair.
        let dir = tempfile::TempDir::new().unwrap();
        let repo = dir.path();
        let aiki_dir = repo.join(".aiki");
        let templates_dir = aiki_dir.join(TASKS_DIR_NAME);
        std::fs::create_dir_all(&templates_dir).unwrap();

        // Get the actual current source template
        let source_templates = default_plugin_templates();
        let (name, content) = source_templates[0];

        // Write the CURRENT source content to disk (simulating git-tracked update)
        if let Some(parent) = templates_dir.join(name).parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(templates_dir.join(name), content).unwrap();

        // But put an OLD checksum in the manifest (simulating stale manifest)
        let mut manifest = RepoManifest::new();
        let plugin = manifest.get_or_create_plugin("aiki/default", "0.0.1", ".");
        plugin.files.insert(
            name.to_string(),
            crate::tasks::templates::manifest::FileEntry {
                checksum: checksum(b"stale old content that no longer exists"),
                version: Some("0.0.1".to_string()),
                installed_at: "2026-01-01T00:00:00Z".to_string(),
            },
        );
        manifest.save(repo).unwrap();

        // Doctor should report 0 issues (stale manifest is auto-repaired)
        let issues = check_templates(repo, false);
        assert_eq!(issues, 0);

        // Manifest should now have the correct checksum
        let updated = RepoManifest::load(repo).unwrap().unwrap();
        let plugin = updated.get_plugin("aiki/default").unwrap();
        let entry = plugin.files.get(name).unwrap();
        assert_eq!(entry.checksum, checksum(content));
    }

    #[test]
    fn test_check_templates_skips_sync_unsupported_schema() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = dir.path();
        let aiki_dir = repo.join(".aiki");
        std::fs::create_dir_all(&aiki_dir).unwrap();
        std::fs::write(
            aiki_dir.join(".manifest.json"),
            r#"{"schema": 999, "templates": {}}"#,
        )
        .unwrap();

        // Even with fix=true, should not attempt sync
        let issues = check_templates(repo, true);
        assert_eq!(issues, 1);
    }

    #[test]
    fn test_check_templates_legacy_directory() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = dir.path();
        let aiki_dir = repo.join(".aiki");
        let templates_dir = aiki_dir.join(TASKS_DIR_NAME);
        std::fs::create_dir_all(templates_dir.join("aiki")).unwrap();

        // Create a valid manifest so we get past the manifest check
        let manifest = RepoManifest::new();
        manifest.save(repo).unwrap();

        let issues = check_templates(repo, false);
        assert_eq!(issues, 1); // Legacy directory counts as an issue
    }

    // --- Instruction file tests ---

    #[test]
    fn test_check_instructions_file_with_symlink_current_block() {
        let dir = tempfile::tempdir().unwrap();
        let block = crate::commands::agents_template::aiki_block_template();
        fs::write(dir.path().join("AGENTS.md"), &block).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("AGENTS.md", dir.path().join("CLAUDE.md")).unwrap();
        let issues = check_instruction_files(dir.path(), false);
        assert_eq!(issues, 0);
    }

    #[test]
    fn test_check_instructions_file_with_symlink_outdated_block() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("AGENTS.md"),
            "<aiki version=\"0.0.0\">\nold content\n</aiki>\n",
        )
        .unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("AGENTS.md", dir.path().join("CLAUDE.md")).unwrap();
        let issues = check_instruction_files(dir.path(), false);
        assert_eq!(issues, 1); // outdated block
    }

    #[test]
    fn test_check_instructions_file_with_symlink_missing_block() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("AGENTS.md"), "# My instructions\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("AGENTS.md", dir.path().join("CLAUDE.md")).unwrap();
        let issues = check_instruction_files(dir.path(), false);
        assert_eq!(issues, 1); // missing block
    }

    #[test]
    fn test_check_instructions_both_files_no_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let block = crate::commands::agents_template::aiki_block_template();
        fs::write(dir.path().join("AGENTS.md"), &block).unwrap();
        fs::write(dir.path().join("CLAUDE.md"), &block).unwrap();
        let issues = check_instruction_files(dir.path(), false);
        // BothFiles is not an error — just informational + checks blocks
        assert_eq!(issues, 0);
    }

    #[test]
    fn test_check_instructions_file_without_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let block = crate::commands::agents_template::aiki_block_template();
        fs::write(dir.path().join("AGENTS.md"), &block).unwrap();
        // No CLAUDE.md at all
        let issues = check_instruction_files(dir.path(), false);
        assert_eq!(issues, 1); // missing symlink
    }

    #[cfg(unix)]
    #[test]
    fn test_check_instructions_both_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("ext_agents"), "agents").unwrap();
        fs::write(dir.path().join("ext_claude"), "claude").unwrap();
        std::os::unix::fs::symlink("ext_agents", dir.path().join("AGENTS.md")).unwrap();
        std::os::unix::fs::symlink("ext_claude", dir.path().join("CLAUDE.md")).unwrap();
        let issues = check_instruction_files(dir.path(), false);
        assert_eq!(issues, 1); // both symlinks is a problem
    }

    #[test]
    fn test_check_instructions_missing() {
        let dir = tempfile::tempdir().unwrap();
        let issues = check_instruction_files(dir.path(), false);
        assert_eq!(issues, 1); // neither file exists
    }

    #[test]
    fn test_check_instructions_fix_creates_files() {
        let dir = tempfile::tempdir().unwrap();
        let issues = check_instruction_files(dir.path(), true);
        assert_eq!(issues, 0);
        // Should have created AGENTS.md with block
        let agents = fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert!(agents.contains("<aiki version="));
        // Should have created CLAUDE.md symlink
        #[cfg(unix)]
        assert!(dir
            .path()
            .join("CLAUDE.md")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn test_check_instructions_fix_adds_missing_block() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("AGENTS.md"), "# My instructions\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("AGENTS.md", dir.path().join("CLAUDE.md")).unwrap();
        let issues = check_instruction_files(dir.path(), true);
        assert_eq!(issues, 0);
        let content = fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert!(content.contains("<aiki version="));
        assert!(content.contains("# My instructions"));
    }

    // ---- Old-format hook detection and migration tests ----

    #[test]
    fn test_old_format_hooks_detected_not_missing() {
        // All hooks present in old format: missing should be empty, old_format should be full
        let mut file = NamedTempFile::new().unwrap();
        let settings = serde_json::json!({
            "hooks": {
                "SessionStart": [{"matcher": "", "hooks": [{"type": "command", "command": "aiki hooks stdin --agent claude-code --event SessionStart"}]}],
                "PreCompact": [{"matcher": "", "hooks": [{"type": "command", "command": "aiki hooks stdin --agent claude-code --event PreCompact"}]}],
                "UserPromptSubmit": [{"matcher": "", "hooks": [{"type": "command", "command": "aiki hooks stdin --agent claude-code --event UserPromptSubmit"}]}],
                "PreToolUse": [{"matcher": "", "hooks": [{"type": "command", "command": "aiki hooks stdin --agent claude-code --event PreToolUse"}]}],
                "PostToolUse": [{"matcher": "", "hooks": [{"type": "command", "command": "aiki hooks stdin --agent claude-code --event PostToolUse"}]}],
                "Stop": [{"matcher": "", "hooks": [{"type": "command", "command": "aiki hooks stdin --agent claude-code --event Stop"}]}],
                "SessionEnd": [{"matcher": "", "hooks": [{"type": "command", "command": "aiki hooks stdin --agent claude-code --event SessionEnd"}]}]
            }
        });
        write!(file, "{}", serde_json::to_string(&settings).unwrap()).unwrap();

        let status = find_missing_claude_code_hooks(file.path());
        assert!(status.missing.is_empty(), "old-format hooks should not be counted as missing");
        assert_eq!(status.old_format.len(), 7, "all hooks should be detected as old format");
    }

    #[test]
    fn test_new_format_hooks_no_warning() {
        // All hooks in new format: no missing, no old_format
        let mut file = NamedTempFile::new().unwrap();
        let settings = serde_json::json!({
            "hooks": {
                "SessionStart": [{"matcher": "", "hooks": [{"type": "command", "command": "aiki hooks stdin --claude SessionStart"}]}],
                "PreCompact": [{"matcher": "", "hooks": [{"type": "command", "command": "aiki hooks stdin --claude PreCompact"}]}],
                "UserPromptSubmit": [{"matcher": "", "hooks": [{"type": "command", "command": "aiki hooks stdin --claude UserPromptSubmit"}]}],
                "PreToolUse": [{"matcher": "", "hooks": [{"type": "command", "command": "aiki hooks stdin --claude PreToolUse"}]}],
                "PostToolUse": [{"matcher": "", "hooks": [{"type": "command", "command": "aiki hooks stdin --claude PostToolUse"}]}],
                "Stop": [{"matcher": "", "hooks": [{"type": "command", "command": "aiki hooks stdin --claude Stop"}]}],
                "SessionEnd": [{"matcher": "", "hooks": [{"type": "command", "command": "aiki hooks stdin --claude SessionEnd"}]}]
            }
        });
        write!(file, "{}", serde_json::to_string(&settings).unwrap()).unwrap();

        let status = find_missing_claude_code_hooks(file.path());
        assert!(status.missing.is_empty());
        assert!(status.old_format.is_empty(), "new-format hooks should not trigger old_format");
    }

    #[test]
    fn test_mixed_old_new_format_hooks() {
        // Mix of old and new format: only old ones appear in old_format
        let mut file = NamedTempFile::new().unwrap();
        let settings = serde_json::json!({
            "hooks": {
                "SessionStart": [{"matcher": "", "hooks": [{"type": "command", "command": "aiki hooks stdin --claude SessionStart"}]}],
                "PreCompact": [{"matcher": "", "hooks": [{"type": "command", "command": "aiki hooks stdin --agent claude-code --event PreCompact"}]}],
                "UserPromptSubmit": [{"matcher": "", "hooks": [{"type": "command", "command": "aiki hooks stdin --claude UserPromptSubmit"}]}],
                "PreToolUse": [{"matcher": "", "hooks": [{"type": "command", "command": "aiki hooks stdin --agent claude-code --event PreToolUse"}]}],
                "PostToolUse": [{"matcher": "", "hooks": [{"type": "command", "command": "aiki hooks stdin --claude PostToolUse"}]}],
                "Stop": [{"matcher": "", "hooks": [{"type": "command", "command": "aiki hooks stdin --claude Stop"}]}],
                "SessionEnd": [{"matcher": "", "hooks": [{"type": "command", "command": "aiki hooks stdin --claude SessionEnd"}]}]
            }
        });
        write!(file, "{}", serde_json::to_string(&settings).unwrap()).unwrap();

        let status = find_missing_claude_code_hooks(file.path());
        assert!(status.missing.is_empty());
        assert_eq!(status.old_format, vec!["PreCompact", "PreToolUse"]);
    }

    #[test]
    fn test_migrate_old_format_hooks_in_json() {
        // Test that --fix rewrites old-format hook commands to new shorthand
        let mut file = NamedTempFile::new().unwrap();
        let settings = serde_json::json!({
            "hooks": {
                "SessionStart": [{"matcher": "", "hooks": [{"type": "command", "command": "aiki hooks stdin --agent claude-code --event SessionStart"}]}],
                "Stop": [{"matcher": "", "hooks": [{"type": "command", "command": "/usr/local/bin/aiki hooks stdin --agent claude-code --event Stop"}]}]
            },
            "permissions": {"allow": ["Bash"]}
        });
        write!(file, "{}", serde_json::to_string(&settings).unwrap()).unwrap();

        let count = migrate_old_format_hooks_in_file(file.path(), "claude-code", "--claude").unwrap();
        assert_eq!(count, 2);

        // Read back and verify
        let content = fs::read_to_string(file.path()).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();

        // SessionStart should now use --claude
        let cmd = json["hooks"]["SessionStart"][0]["hooks"][0]["command"].as_str().unwrap();
        assert_eq!(cmd, "aiki hooks stdin --claude SessionStart");

        // Stop should now use --claude
        let cmd = json["hooks"]["Stop"][0]["hooks"][0]["command"].as_str().unwrap();
        assert_eq!(cmd, "/usr/local/bin/aiki hooks stdin --claude Stop");

        // permissions should be preserved
        assert!(json.get("permissions").is_some());
    }

    #[test]
    fn test_migrate_mixed_old_new_only_rewrites_old() {
        // Only old-format commands get rewritten; new-format left untouched
        let mut file = NamedTempFile::new().unwrap();
        let settings = serde_json::json!({
            "hooks": {
                "SessionStart": [{"matcher": "", "hooks": [{"type": "command", "command": "aiki hooks stdin --claude SessionStart"}]}],
                "Stop": [{"matcher": "", "hooks": [{"type": "command", "command": "aiki hooks stdin --agent claude-code --event Stop"}]}]
            }
        });
        write!(file, "{}", serde_json::to_string(&settings).unwrap()).unwrap();

        let count = migrate_old_format_hooks_in_file(file.path(), "claude-code", "--claude").unwrap();
        assert_eq!(count, 1); // only Stop was old format

        let content = fs::read_to_string(file.path()).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();

        // SessionStart should still use --claude (unchanged)
        let cmd = json["hooks"]["SessionStart"][0]["hooks"][0]["command"].as_str().unwrap();
        assert_eq!(cmd, "aiki hooks stdin --claude SessionStart");

        // Stop should now use --claude
        let cmd = json["hooks"]["Stop"][0]["hooks"][0]["command"].as_str().unwrap();
        assert_eq!(cmd, "aiki hooks stdin --claude Stop");
    }

    #[test]
    fn test_migrate_already_new_format_no_changes() {
        // All hooks already in new format: no changes
        let mut file = NamedTempFile::new().unwrap();
        let settings = serde_json::json!({
            "hooks": {
                "SessionStart": [{"matcher": "", "hooks": [{"type": "command", "command": "aiki hooks stdin --claude SessionStart"}]}],
                "Stop": [{"matcher": "", "hooks": [{"type": "command", "command": "aiki hooks stdin --claude Stop"}]}]
            }
        });
        let original = serde_json::to_string(&settings).unwrap();
        write!(file, "{}", original).unwrap();

        let count = migrate_old_format_hooks_in_file(file.path(), "claude-code", "--claude").unwrap();
        assert_eq!(count, 0);

        // File should not have been rewritten (original formatting preserved)
        let content = fs::read_to_string(file.path()).unwrap();
        assert_eq!(content, original); // identical since count=0 means no write
    }

    #[test]
    fn test_migrate_cursor_hooks() {
        // Test cursor hook migration
        let mut file = NamedTempFile::new().unwrap();
        let hooks = serde_json::json!({
            "version": 1,
            "hooks": {
                "beforeSubmitPrompt": [{"command": "aiki hooks stdin --agent cursor --event beforeSubmitPrompt"}],
                "afterFileEdit": [{"command": "aiki hooks stdin --agent cursor --event afterFileEdit"}]
            }
        });
        write!(file, "{}", serde_json::to_string(&hooks).unwrap()).unwrap();

        let count = migrate_old_format_hooks_in_file(file.path(), "cursor", "--cursor").unwrap();
        assert_eq!(count, 2);

        let content = fs::read_to_string(file.path()).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();

        let cmd = json["hooks"]["beforeSubmitPrompt"][0]["command"].as_str().unwrap();
        assert_eq!(cmd, "aiki hooks stdin --cursor beforeSubmitPrompt");

        let cmd = json["hooks"]["afterFileEdit"][0]["command"].as_str().unwrap();
        assert_eq!(cmd, "aiki hooks stdin --cursor afterFileEdit");
    }

    #[test]
    fn test_migrate_codex_hooks() {
        // Test codex hook migration
        let mut file = NamedTempFile::new().unwrap();
        let hooks = serde_json::json!({
            "hooks": {
                "SessionStart": [{"hooks": [{"type": "command", "command": "aiki hooks stdin --agent codex --event sessionStart"}]}],
                "Stop": [{"hooks": [{"type": "command", "command": "aiki hooks stdin --agent codex --event stop"}]}]
            }
        });
        write!(file, "{}", serde_json::to_string(&hooks).unwrap()).unwrap();

        let count = migrate_old_format_hooks_in_file(file.path(), "codex", "--codex").unwrap();
        assert_eq!(count, 2);

        let content = fs::read_to_string(file.path()).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();

        let cmd = json["hooks"]["SessionStart"][0]["hooks"][0]["command"].as_str().unwrap();
        assert_eq!(cmd, "aiki hooks stdin --codex sessionStart");

        let cmd = json["hooks"]["Stop"][0]["hooks"][0]["command"].as_str().unwrap();
        assert_eq!(cmd, "aiki hooks stdin --codex stop");
    }

    #[test]
    fn test_cursor_hooks_old_format_detection() {
        // Cursor hooks all in old format: all_present=true, has_old_format=true
        let mut file = NamedTempFile::new().unwrap();
        let hooks = serde_json::json!({
            "version": 1,
            "hooks": {
                "beforeSubmitPrompt": [{"command": "aiki hooks stdin --agent cursor --event beforeSubmitPrompt"}],
                "afterFileEdit": [{"command": "aiki hooks stdin --agent cursor --event afterFileEdit"}],
                "beforeShellExecution": [{"command": "aiki hooks stdin --agent cursor --event beforeShellExecution"}],
                "afterShellExecution": [{"command": "aiki hooks stdin --agent cursor --event afterShellExecution"}],
                "beforeMCPExecution": [{"command": "aiki hooks stdin --agent cursor --event beforeMCPExecution"}],
                "afterMCPExecution": [{"command": "aiki hooks stdin --agent cursor --event afterMCPExecution"}],
                "stop": [{"command": "aiki hooks stdin --agent cursor --event stop"}]
            }
        });
        write!(file, "{}", serde_json::to_string(&hooks).unwrap()).unwrap();

        let status = check_cursor_hooks(file.path());
        assert!(status.all_present);
        assert!(status.has_old_format);
    }

    #[test]
    fn test_cursor_hooks_new_format_no_warning() {
        // Cursor hooks all in new format: no warning
        let mut file = NamedTempFile::new().unwrap();
        let hooks = serde_json::json!({
            "version": 1,
            "hooks": {
                "beforeSubmitPrompt": [{"command": "aiki hooks stdin --cursor beforeSubmitPrompt"}],
                "afterFileEdit": [{"command": "aiki hooks stdin --cursor afterFileEdit"}],
                "beforeShellExecution": [{"command": "aiki hooks stdin --cursor beforeShellExecution"}],
                "afterShellExecution": [{"command": "aiki hooks stdin --cursor afterShellExecution"}],
                "beforeMCPExecution": [{"command": "aiki hooks stdin --cursor beforeMCPExecution"}],
                "afterMCPExecution": [{"command": "aiki hooks stdin --cursor afterMCPExecution"}],
                "stop": [{"command": "aiki hooks stdin --cursor stop"}]
            }
        });
        write!(file, "{}", serde_json::to_string(&hooks).unwrap()).unwrap();

        let status = check_cursor_hooks(file.path());
        assert!(status.all_present);
        assert!(!status.has_old_format);
    }
}
