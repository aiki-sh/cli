use crate::config;
use crate::editors::zed as ide_config;
use crate::error::Result;
use crate::global;
use crate::instructions;
use crate::jj;
use crate::prerequisites;
use crate::repos;
use crate::repos::RepoDetector;
use anyhow::Context;
use std::env;
use std::fs;
use std::path::Path;

/// Default content for .aiki/hooks.yml created by `aiki init`.
/// This is also used by `aiki doctor --fix` to recreate a missing hookfile.
pub const HOOKS_YML_TEMPLATE: &str = r#"# Aiki Hooks
#
# This file configures agent hooks for your project.
#
# Learn more:
#   aiki hooks --help
#   https://aiki.sh/help/hooks

include:
  - aiki/default  # The opinionated Aiki Way (auto-updates with new releases)

# ============================================================================
# Custom Hooks
# ============================================================================
# Add your own event handlers below. Each event fires at a specific point
# in the agent lifecycle. Uncomment and modify to customize.
#
# --- Session Lifecycle ---
#
# session.started:
#   # Fires when a new agent session begins (after aiki/core initializes)
#   # Use for: injecting project context, setting up session state
#   - context: "Remember to run tests before committing"
#
# session.resumed:
#   # Fires when an existing session is resumed (not a fresh start)
#   # Use for: re-injecting context that may have been lost to compaction
#
# session.ended:
#   # Fires when an agent session ends
#   # Use for: cleanup, notifications, session summaries
#
# --- Turn Lifecycle ---
#
# turn.started:
#   # Fires before each agent turn (user prompt or autoreply)
#   # Use for: injecting per-turn context, rate limiting
#   # Note: survives context compaction (re-injected every turn)
#
# turn.completed:
#   # Fires after the agent finishes responding
#   # Use for: post-turn validation, autoreplies, review triggers
#   # Supports: autoreply: (send a follow-up message to the agent)
#
# --- File Operations ---
#
# change.permission_asked:
#   # Fires before a file write, delete, or move (gateable)
#   # Use for: blocking writes to protected files, requiring approval
#   # - if: $event.file_paths | contains(".env")
#   #   then:
#   #     - block: "Cannot modify .env files"
#
# change.completed:
#   # Fires after a file mutation completes
#   # Use for: post-change validation, lint checks
#
# read.permission_asked:
#   # Fires before a file read (gateable)
#   # Use for: blocking reads of sensitive files (secrets, credentials)
#
# --- Shell Commands ---
#
# shell.permission_asked:
#   # Fires before a shell command executes (gateable)
#   # Use for: blocking dangerous commands, requiring review before push
#   # - if: $event.command | contains("git push")
#   #   then:
#   #     - block: "Run tests before pushing"
#
# shell.completed:
#   # Fires after a shell command completes
#   # Use for: logging, post-command validation
#
# --- Task Lifecycle ---
#
# task.started:
#   # Fires when a task transitions to in_progress
#   # Use for: notifications, task setup
#
# task.closed:
#   # Fires when a task is closed
#   # Use for: notifications, triggering follow-up work
#
# --- Other Events ---
#
# commit.message_started:
#   # Fires during Git's prepare-commit-msg hook
#   # Use for: adding trailers, enforcing commit message format
#
# mcp.permission_asked:
#   # Fires before an MCP tool call (gateable)
#   # Use for: rate limiting, blocking expensive operations
#
# web.permission_asked:
#   # Fires before a web fetch (gateable)
#   # Use for: blocking external requests, domain allowlisting
"#;

pub fn run(quiet: bool) -> Result<()> {
    prerequisites::check_prerequisites(quiet)?;

    // Get current directory
    let current_dir = env::current_dir().context("Failed to get current directory")?;

    // Detect repository
    let detector = RepoDetector::new(&current_dir);

    // Find the Git repository root
    let repo_root = detector.find_repo_root()?;

    // Check if already initialized by looking at git config
    let git_hooks_path = std::process::Command::new("git")
        .args(["config", "core.hooksPath"])
        .current_dir(&repo_root)
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
            } else {
                None
            }
        });

    // Check if pointing to global hooks
    let home_dir = dirs::home_dir().context("Could not find home directory")?;
    let global_hooks = home_dir.join(".aiki/githooks");

    if let Some(ref hooks_path) = git_hooks_path {
        if hooks_path.contains(".aiki/githooks") {
            // Even on re-init, ensure hookfile exists
            ensure_hooks_yml(&repo_root, quiet)?;

            // Sync built-in templates on re-init (picks up new/updated templates)
            crate::tasks::templates::sync::sync_default_templates(&repo_root, quiet)?;

            // Ensure instruction files exist even on re-init
            instructions::ensure_instruction_files(&repo_root, quiet)?;

            if quiet {
                // Silent success for auto mode
                return Ok(());
            }
            println!("Repository already initialized at {}", repo_root.display());
            return Ok(());
        }
    }

    if !quiet {
        println!("Initializing Aiki in: {}", repo_root.display());
    }

    // Initialize global aiki directories (~/.aiki/sessions/ and ~/.aiki/.jj/)
    init_global_directories(quiet)?;

    // Check if JJ is already initialized
    if RepoDetector::has_jj(&repo_root) {
        let workspace = jj::JJWorkspace::new(&repo_root);
        if workspace.is_healthy_non_colocated() {
            if !quiet {
                println!("✓ Found existing JJ repository");
            }
        } else {
            if !quiet {
                println!("⚠ JJ workspace exists but is not non-colocated");
                println!("  Warning: if you use jj for version control, removing .jj will delete your jj history");
                println!("  Run: rm -rf .jj && aiki init");
            } else {
                eprintln!(
                    "Warning: JJ workspace is not non-colocated. Run: rm -rf .jj && aiki init"
                );
            }
        }
    } else {
        if !quiet {
            println!("Initializing JJ repository...");
        }
        let workspace = jj::JJWorkspace::new(&repo_root);
        workspace
            .init()
            .context("Failed to initialize JJ repository")?;
        if !quiet {
            println!("✓ Initialized JJ repository");
        }
    }

    // Create .aiki directory to store repository-specific configuration
    let aiki_dir = repo_root.join(".aiki");
    fs::create_dir_all(&aiki_dir).context("Failed to create .aiki directory")?;

    // Generate repository ID for global state tracking
    let repo_id = repos::ensure_repo_id(&repo_root)?;
    if !quiet {
        if repo_id.starts_with("local-") {
            println!("✓ Generated repository ID (local): {}", repo_id);
            println!("  Note: This will upgrade to a stable ID after your first git commit");
        } else {
            println!("✓ Generated repository ID: {}", repo_id);
        }
    }

    // Save previous git hooks path before configuring global hooks
    // This allows Git hooks to chain to pre-existing hooks
    config::save_previous_hooks_path(&repo_root)?;

    // Configure git to use global hooks directory
    let global_hooks_str = global_hooks.to_str().context("Invalid global hooks path")?;
    std::process::Command::new("git")
        .args(["config", "core.hooksPath", global_hooks_str])
        .current_dir(&repo_root)
        .output()
        .context("Failed to set git config core.hooksPath")?;

    if !quiet {
        println!("✓ Configured Git hooks (→ {})", global_hooks.display());
    }

    // Configure IDE settings (Zed)
    if !quiet {
        println!("\nConfiguring IDE settings...");
    }

    match ide_config::configure_zed() {
        Ok(()) => {
            if !quiet {
                println!("✓ Configured Zed editor for ACP support");
                if let Some(path) = ide_config::zed_settings_path() {
                    println!("  Settings: {}", path.display());
                }
            }
        }
        Err(e) => {
            if !quiet {
                println!("⚠ Failed to configure Zed: {}", e);
                println!("  You can configure manually later");
            }
        }
    }

    // Install OTel receiver for Codex session tracking
    if !config::is_otel_receiver_installed() {
        if !quiet {
            println!("\nInstalling OTel receiver...");
        }
        match config::install_otel_receiver() {
            Ok(()) => {
                // Wait for launchd/systemd to actually bind the socket
                match config::wait_for_otel_receiver() {
                    Ok(()) => {
                        if !quiet {
                            println!("✓ OTel receiver installed (listening on 127.0.0.1:19876)");
                        }
                    }
                    Err(_) => {
                        if !quiet {
                            println!("⚠ OTel receiver installed but not yet listening");
                            println!("  Run: aiki doctor --fix");
                        }
                    }
                }
            }
            Err(e) => {
                if !quiet {
                    println!("⚠ Failed to install OTel receiver: {}", e);
                    println!("  Codex session tracking will not work until this is resolved.");
                    println!("  Run: aiki doctor --fix");
                }
            }
        }
    }

    // Install agent integrations (Claude Code, Cursor, Codex hooks)
    if !quiet {
        println!("\nInstalling agent integrations...");
    }

    match config::install_global_git_hooks() {
        Ok(()) => {
            if !quiet {
                println!("✓ Global Git hooks installed");
            }
        }
        Err(e) => {
            if !quiet {
                println!("⚠ Failed to install global Git hooks: {}", e);
            }
        }
    }

    match config::install_claude_code_hooks_global() {
        Ok(()) => {
            if !quiet {
                println!("✓ Claude Code hooks installed");
            }
        }
        Err(e) => {
            if !quiet {
                println!("⚠ Failed to install Claude Code hooks: {}", e);
            }
        }
    }

    match config::install_cursor_hooks_global() {
        Ok(()) => {
            if !quiet {
                println!("✓ Cursor hooks installed");
            }
        }
        Err(e) => {
            if !quiet {
                println!("⚠ Failed to install Cursor hooks: {}", e);
            }
        }
    }

    match config::install_codex_hooks_global() {
        Ok(()) => {
            if !quiet {
                println!("✓ Codex hooks installed");
            }
        }
        Err(e) => {
            if !quiet {
                println!("⚠ Failed to install Codex hooks: {}", e);
            }
        }
    }

    // Ensure hookfile exists for workflow automation
    ensure_hooks_yml(&repo_root, quiet)?;

    // Ensure instruction file has <aiki> block and symlink exists
    if !quiet {
        println!("\nConfiguring agent instructions...");
    }
    instructions::ensure_instruction_files(&repo_root, quiet)?;

    // Sync built-in templates
    if !quiet {
        println!("\nSyncing built-in templates...");
    }
    crate::tasks::templates::sync::sync_default_templates(&repo_root, quiet)?;

    // Install plugins referenced by project templates
    let plugin_refs = crate::plugins::project::derive_project_plugin_refs(&repo_root);
    if !plugin_refs.is_empty() {
        if !quiet {
            println!("\nInstalling plugins...");
        }
        match crate::plugins::project::install_project_plugins(&repo_root) {
            Ok(count) => {
                if !quiet && count > 0 {
                    println!("✓ Installed {} plugin(s)", count);
                } else if !quiet {
                    println!("✓ All plugins already installed");
                }
            }
            Err(e) => {
                if !quiet {
                    eprintln!("⚠ Failed to install some plugins: {}", e);
                    eprintln!("  Run: aiki plugin install");
                }
            }
        }
    }

    if !quiet {
        println!("\n✓ Repository initialized successfully!");
        println!("\nYour AI changes will now be tracked automatically.");
        println!("Git commits will include AI co-authors.");
    }

    Ok(())
}

/// Ensure .aiki/hooks.yml exists with default workflow automation.
/// Never overwrites an existing hookfile — user customizations are sacred.
fn ensure_hooks_yml(repo_root: &Path, quiet: bool) -> Result<()> {
    let hooks_path = repo_root.join(".aiki/hooks.yml");

    if hooks_path.exists() {
        if !quiet {
            println!(".aiki/hooks.yml already exists (skipping)");
        }
        return Ok(());
    }

    // Ensure .aiki directory exists (may not exist on re-init path)
    let aiki_dir = repo_root.join(".aiki");
    if !aiki_dir.exists() {
        return Ok(()); // No .aiki dir yet — will be created later in init flow
    }

    fs::write(&hooks_path, HOOKS_YML_TEMPLATE).context("Failed to create .aiki/hooks.yml")?;

    if !quiet {
        println!("Created .aiki/hooks.yml with default workflow automation");
    }

    Ok(())
}

/// Initialize global aiki directories
///
/// Creates:
/// - `~/.aiki/sessions/` for global session files
/// - `~/.aiki/.jj/` for global conversation history
fn init_global_directories(quiet: bool) -> Result<()> {
    use crate::jj::jj_cmd;

    let global_aiki = global::global_aiki_dir();
    let global_sessions = global::global_sessions_dir();
    let global_jj = global::global_jj_dir();

    // Create sessions directory
    fs::create_dir_all(&global_sessions).context("Failed to create global sessions directory")?;

    // Initialize global JJ repo if not exists
    // The JJ repo is non-colocated (no git), stores conversation history
    if !global_jj.exists() {
        if !quiet {
            println!("Initializing global JJ repository...");
        }

        // Create parent directory first
        fs::create_dir_all(&global_aiki).context("Failed to create global aiki directory")?;

        // Initialize JJ repo (non-colocated, git backend)
        let result = jj_cmd()
            .args(["git", "init", "--no-colocate"])
            .current_dir(&global_aiki)
            .output()
            .context("Failed to run jj init for global repo")?;

        if !result.status.success() {
            let stderr = String::from_utf8_lossy(&result.stderr);
            // Ignore "already exists" errors (idempotent)
            if !stderr.contains("already exists") {
                return Err(
                    anyhow::anyhow!("Failed to initialize global JJ repo: {}", stderr).into(),
                );
            }
        }

        if !quiet {
            println!(
                "✓ Initialized global JJ repository at {}",
                global_jj.display()
            );
        }
    } else if !quiet {
        println!("✓ Global JJ repository exists");
    }

    Ok(())
}
