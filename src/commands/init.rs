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
# --- Workflow Lifecycle ---
#
# Emitted by aiki's own orchestration commands (build, fix, loop, review),
# not the editor. Neutral signals: plugins map them to external surfaces (the
# aiki-sh/aiki-plugin-herdr plugin shows a running workflow as an agent row).
#
# workflow.started:
#   # Fires when an aiki workflow command begins (brackets the whole run)
#   # Vars: {{event.workflow.name}}
#   # Use for: surfacing the workflow as an external "agent" or progress signal
#
# workflow.completed:
#   # Fires when an aiki workflow command ends (success, error, or unwind)
#   # Vars: {{event.workflow.name}}, {{event.workflow.success}}
#
# step.started:
#   # Fires when a single step within a workflow begins (decompose, loop, ...)
#   # Vars: {{event.step.name}}
#
# step.completed:
#   # Fires when a workflow step ends (success or error)
#   # Vars: {{event.step.name}}, {{event.step.success}}
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

/// Returns a human-readable reason when `repo_root` is a directory aiki must
/// never turn into a repository (the user's home directory or a filesystem
/// root), or `None` when it is safe to initialize. Initializing at one of those
/// makes jj treat the entire tree (including synced `~/Library` dirs) as one
/// working copy, which breaks snapshots and can swallow unrelated files.
/// Canonicalizes both paths so a symlinked or relative form still matches.
fn unsafe_init_root_reason(repo_root: &Path, home_dir: &Path) -> Option<&'static str> {
    let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let root = canonical(repo_root);
    if root == canonical(home_dir) {
        Some("your home directory")
    } else if root.parent().is_none() {
        Some("a filesystem root")
    } else {
        None
    }
}

pub fn run(quiet: bool) -> Result<()> {
    prerequisites::check_prerequisites(quiet)?;

    // Lazy reaper: clean per-user markers whose repos are gone (a teammate ran
    // `aiki remove --shared`, or the repo was deleted). Best-effort, cheap.
    let reaped = repos::reap_stale_markers();
    if reaped > 0 && !quiet {
        println!("✓ Reaped {reaped} stale aiki marker(s)");
    }

    // Get current directory
    let current_dir = env::current_dir().context("Failed to get current directory")?;

    // Detect repository
    let detector = RepoDetector::new(&current_dir);

    // Find the Git repository root
    let repo_root = detector.find_repo_root()?;

    // Never initialize aiki at the home directory or a filesystem root: doing so
    // makes jj treat the entire tree (including synced ~/Library dirs) as one
    // working copy, which breaks snapshots and can swallow unrelated files. If we
    // resolved to one of those, do nothing (return Ok) with a helpful message.
    let home_dir = dirs::home_dir().context("Could not find home directory")?;
    if let Some(reason) = unsafe_init_root_reason(&repo_root, &home_dir) {
        if !quiet {
            println!(
                "Skipping aiki init: {} is {reason}.\n\
                 Initializing here would turn the whole tree into a single jj \
                 repository, which breaks snapshots and can interfere with synced \
                 folders (iCloud, Dropbox).\n\
                 To set up aiki for a project, cd into that project's directory \
                 and run 'aiki init' there.",
                repo_root.display()
            );
        }
        return Ok(());
    }

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

    // Check if pointing to global hooks (home_dir resolved above)
    let global_hooks = home_dir.join(".aiki/githooks");

    if let Some(ref hooks_path) = git_hooks_path {
        if hooks_path.contains(".aiki/githooks") {
            // Even on re-init, ensure hookfile exists
            ensure_hooks_yml(&repo_root, quiet)?;

            // Sync built-in templates on re-init (picks up new/updated templates)
            crate::tasks::templates::sync::sync_default_templates(&repo_root, quiet)?;

            // Ensure instruction files exist even on re-init
            instructions::ensure_instruction_files(&repo_root, quiet)?;

            // Opt this user into aiki for this repo (idempotent). On a legacy
            // repo being upgraded to init-v2, this is where a previously
            // auto-init'd user gets their explicit per-user marker.
            ensure_enable_marker(&repo_root, quiet)?;

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

    // Record this user's explicit opt-in for this repo. The per-user marker is
    // what distinguishes an Active repo from a cloned-but-not-enabled (Dormant)
    // one — see ops/now/init-v2.md.
    ensure_enable_marker(&repo_root, quiet)?;

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

    // Attribute the scaffolding we just created to Aiki (first-run provenance).
    // Last, so every artifact above is part of the attributed change.
    record_init_provenance(&repo_root, quiet);

    if !quiet {
        println!("\n✓ Repository initialized successfully!");
        println!("\nYour AI changes will now be tracked automatically.");
        println!("Git commits will include AI co-authors.");
    }

    Ok(())
}

/// Create the per-user enable marker recording that this user has opted into
/// aiki for `repo_root`. Existence is the signal; the file contents are unused.
///
/// **Marker creation is reserved for `aiki init`.** Not `aiki doctor --fix`,
/// not the lazy reaper, not migrations — otherwise legacy auto-init repos would
/// silently re-enroll every user who clones them. The marker is what makes a
/// repo `Active` rather than `Dormant` for a given user. See
/// `ops/now/init-v2.md`.
fn ensure_enable_marker(repo_root: &Path, quiet: bool) -> Result<()> {
    let marker = repos::marker_path(repo_root);
    if marker.exists() {
        return Ok(());
    }
    if let Some(parent) = marker.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!("Failed to create marker directory {}", parent.display())
        })?;
    }
    fs::write(&marker, "").context("Failed to write per-user enable marker")?;
    if !quiet {
        println!("✓ Enabled aiki for you in this repo");
    }
    Ok(())
}

/// Attribute the scaffolding `aiki init` just created (`.aiki/`, the `<aiki>`
/// block, the instruction symlink, `.gitignore`) to Aiki by describing the
/// working-copy change with an `[aiki]` provenance block, then start a fresh
/// change. This is the first-run provenance that used to live in `hooks.yaml`'s
/// `session.started` (moved here so it survives once auto-init is stripped).
///
/// Best-effort and non-fatal: a missing/unhealthy jj never blocks init. Guarded
/// to only describe an **undescribed** change, so it can never clobber a
/// user's own working-copy description. The `user-init` session placeholder is
/// replaced by real CLI session UUIDs once init-first-class-cli-sessions ships.
fn record_init_provenance(repo_root: &Path, quiet: bool) {
    use crate::jj::jj_cmd;

    // Never clobber a described change (the user's working copy).
    let described = jj_cmd()
        .args([
            "log", "-r", "@", "--no-graph", "--no-pager", "-T", "description",
            "--ignore-working-copy",
        ])
        .current_dir(repo_root)
        .output()
        .map(|o| o.status.success() && !String::from_utf8_lossy(&o.stdout).trim().is_empty())
        .unwrap_or(true); // On error, assume described and skip (safe).
    if described {
        return;
    }

    // Anything to attribute?
    let has_changes = jj_cmd()
        .args(["diff", "-r", "@", "--name-only", "--ignore-working-copy"])
        .current_dir(repo_root)
        .output()
        .map(|o| o.status.success() && !String::from_utf8_lossy(&o.stdout).trim().is_empty())
        .unwrap_or(false);
    if !has_changes {
        return;
    }

    let message = "[aiki]\nauthor=aiki\nauthor_type=agent\nsession=user-init\n[/aiki]";
    let ok = jj_cmd()
        .args(["describe", "--message", message, "--ignore-working-copy"])
        .env("JJ_USER", "Aiki")
        .env("JJ_EMAIL", "noreply@aiki.sh")
        .current_dir(repo_root)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if ok {
        // Leave a fresh empty working copy on top of the attributed change.
        let _ = jj_cmd()
            .args(["new", "--ignore-working-copy"])
            .current_dir(repo_root)
            .output();
    } else if !quiet {
        eprintln!("⚠ Could not record init provenance (non-fatal)");
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Run `f` with `AIKI_HOME` pointed at `home`, serialized against other
    /// tests that touch the env var.
    fn with_aiki_home<F: FnOnce() -> R, R>(home: &Path, f: F) -> R {
        let _lock = global::AIKI_HOME_TEST_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let original = std::env::var(global::AIKI_HOME_ENV).ok();
        std::env::set_var(global::AIKI_HOME_ENV, home);
        let result = f();
        match original {
            Some(v) => std::env::set_var(global::AIKI_HOME_ENV, v),
            None => std::env::remove_var(global::AIKI_HOME_ENV),
        }
        result
    }

    #[test]
    fn ensure_enable_marker_creates_marker_and_is_idempotent() {
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        with_aiki_home(home.path(), || {
            let marker = repos::marker_path(repo.path());
            assert!(!marker.exists());

            ensure_enable_marker(repo.path(), true).unwrap();
            assert!(marker.is_file(), "marker should be created");

            // Second call is a no-op (no error, marker still present).
            ensure_enable_marker(repo.path(), true).unwrap();
            assert!(marker.is_file());
        });
    }

    #[test]
    fn ensure_enable_marker_makes_repo_active() {
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        fs::create_dir(repo.path().join(".aiki")).unwrap();
        with_aiki_home(home.path(), || {
            // Before opt-in: Dormant (has .aiki/, no marker).
            assert_eq!(
                repos::init_state(repo.path()).unwrap(),
                repos::InitState::Dormant {
                    root: repo.path().to_path_buf()
                },
            );

            ensure_enable_marker(repo.path(), true).unwrap();

            // After opt-in: Active.
            assert_eq!(
                repos::init_state(repo.path()).unwrap(),
                repos::InitState::Active {
                    root: repo.path().to_path_buf()
                },
            );
        });
    }

    #[test]
    fn unsafe_init_root_reason_flags_home_directory() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            unsafe_init_root_reason(home.path(), home.path()),
            Some("your home directory"),
        );
    }

    #[test]
    fn unsafe_init_root_reason_flags_filesystem_root() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            unsafe_init_root_reason(Path::new("/"), home.path()),
            Some("a filesystem root"),
        );
    }

    #[test]
    fn unsafe_init_root_reason_allows_normal_project_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("code").join("myproj");
        fs::create_dir_all(&project).unwrap();
        // home is a sibling of the resolved project root, not equal to it.
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        assert_eq!(unsafe_init_root_reason(&project, &home), None);
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_init_root_reason_flags_home_reached_via_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let link = tmp.path().join("link-to-home");
        std::os::unix::fs::symlink(&home, &link).unwrap();
        // A symlinked path to home still canonicalizes to home.
        assert_eq!(
            unsafe_init_root_reason(&link, &home),
            Some("your home directory"),
        );
    }
}
