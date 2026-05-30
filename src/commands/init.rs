use crate::commands::agents_template::{AIKI_BLOCK_TEMPLATE, AIKI_BLOCK_VERSION};
use crate::config;
use crate::editors::zed as ide_config;
use crate::error::Result;
use crate::global;
use crate::jj;
use crate::repos::RepoDetector;
use crate::repos;
use crate::signing;
use anyhow::Context;
use std::env;
use std::fs;
use std::io::{self, Write};
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
        if !quiet {
            println!("✓ Found existing JJ repository");
        }
    } else {
        if !quiet {
            println!("Initializing JJ repository...");
        }
        // Create JJ workspace manager for the repository root
        let workspace = jj::JJWorkspace::new(&repo_root);

        // Initialize pure JJ storage (independent from Git)
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

    // Configure commit signing
    match signing::detect_signing_config()? {
        Some(signing_config) => {
            config::update_jj_signing_config(
                &repo_root,
                &signing_config.backend.to_string(),
                Some(&signing_config.key),
                "own",
            )?;

            // For SSH backend, create allowed-signers file
            if matches!(signing_config.backend, signing::SigningBackend::Ssh) {
                let email = signing::get_user_email(&repo_root)?;
                signing::create_ssh_allowed_signers(&repo_root, &email, &signing_config.key)?;
            }

            if !quiet {
                println!(
                    "✓ Configured JJ commit signing ({:?})",
                    signing_config.backend
                );
                println!("  Using key: {}", signing_config.key);
            }
        }
        None => {
            if !quiet {
                println!("⚠ No signing keys detected");
                println!();
                println!("Commit signing provides cryptographic proof of AI authorship.");
                println!();

                // Check if we're in an interactive terminal
                let is_interactive = atty::is(atty::Stream::Stdin);

                if !is_interactive {
                    println!("Run 'aiki doctor --fix' to set up signing interactively.");
                    println!();
                    println!("Continuing without signing...");
                    println!();
                } else {
                    println!("What would you like to do?");
                    println!("  1. Generate new signing key (recommended)");
                    println!("  2. I have a key, let me specify it manually");
                    println!("  3. Skip signing for now");
                    println!();

                    let choice = prompt_choice("Choice", 1, 3)?;

                    match choice {
                        1 => {
                            // Launch wizard in generate mode
                            let wizard = signing::SignSetupWizard::new(repo_root.clone());
                            wizard.run(None)?;
                        }
                        2 => {
                            // Manual key configuration
                            println!();
                            println!("Manual Key Configuration");
                            println!("========================");
                            println!();

                            println!("Which backend?");
                            println!("  1. GPG");
                            println!("  2. SSH");
                            println!();

                            let backend_choice = prompt_choice("Choice", 1, 2)?;
                            let backend = if backend_choice == 1 {
                                signing::SigningBackend::Gpg
                            } else {
                                signing::SigningBackend::Ssh
                            };

                            let key = prompt_string(
                                if backend == signing::SigningBackend::Gpg {
                                    "GPG Key ID (e.g., 4ED556E9729E000F)"
                                } else {
                                    "SSH public key path (e.g., ~/.ssh/id_ed25519.pub)"
                                },
                                None,
                            )?;

                            let wizard = signing::SignSetupWizard::new(repo_root.clone());
                            wizard.run(Some(signing::SetupMode::Manual { backend, key }))?;
                        }
                        3 => {
                            println!();
                            println!("Skipping signing setup.");
                            println!("You can set up signing later by running: aiki sign setup");
                            println!();
                        }
                        _ => unreachable!(),
                    }
                }
            }
        }
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
                if !quiet {
                    println!("✓ OTel receiver installed (socket-activated on 127.0.0.1:19876)");
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

    // Ensure AGENTS.md has task system instructions
    if !quiet {
        println!("\nConfiguring agent instructions...");
    }
    ensure_agents_md(&repo_root, quiet)?;

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

fn prompt_choice(prompt: &str, min: usize, max: usize) -> Result<usize> {
    loop {
        print!("{} [{}]: ", prompt, min);
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        if input.is_empty() {
            return Ok(min);
        }

        match input.parse::<usize>() {
            Ok(n) if n >= min && n <= max => return Ok(n),
            _ => println!("Please enter a number between {} and {}", min, max),
        }
    }
}

fn prompt_string(prompt: &str, default: Option<&str>) -> Result<String> {
    if let Some(def) = default {
        print!("{} [{}]: ", prompt, def);
    } else {
        print!("{}: ", prompt);
    }
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim().to_string();

    if input.is_empty() {
        if let Some(def) = default {
            return Ok(def.to_string());
        }
    }

    Ok(input)
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

    fs::write(&hooks_path, HOOKS_YML_TEMPLATE)
        .context("Failed to create .aiki/hooks.yml")?;

    if !quiet {
        println!("Created .aiki/hooks.yml with default workflow automation");
    }

    Ok(())
}

/// Ensure AGENTS.md exists with the <aiki> block for task system instructions
fn ensure_agents_md(repo_root: &Path, quiet: bool) -> Result<()> {
    let agents_path = repo_root.join("AGENTS.md");

    if agents_path.exists() {
        // Read existing file
        let content = fs::read_to_string(&agents_path).context("Failed to read AGENTS.md")?;

        // Check for <aiki> block
        if !content.contains("<aiki version=") {
            // Prepend block
            let updated = format!("{}\n{}", AIKI_BLOCK_TEMPLATE, content);
            fs::write(&agents_path, updated).context("Failed to update AGENTS.md")?;
            if !quiet {
                println!("✓ Added <aiki> block to AGENTS.md");
            }
        } else if !content.contains(&format!("<aiki version=\"{}\">", AIKI_BLOCK_VERSION)) {
            // Version is outdated
            if !quiet {
                println!("⚠ AGENTS.md has outdated <aiki> block");
                println!("  Run `aiki doctor --fix` to update");
            }
        } else if !quiet {
            println!("✓ AGENTS.md already has <aiki> block");
        }
    } else {
        // Create new AGENTS.md with just the block
        fs::write(&agents_path, AIKI_BLOCK_TEMPLATE).context("Failed to create AGENTS.md")?;
        if !quiet {
            println!("✓ Created AGENTS.md with task system instructions");
        }
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
                return Err(anyhow::anyhow!("Failed to initialize global JJ repo: {}", stderr).into());
            }
        }

        if !quiet {
            println!("✓ Initialized global JJ repository at {}", global_jj.display());
        }
    } else if !quiet {
        println!("✓ Global JJ repository exists");
    }

    Ok(())
}
