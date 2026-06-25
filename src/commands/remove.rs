//! `aiki remove` — the symmetric teardown for `aiki init`.
//!
//! Scope is controlled by flags (see `ops/now/init-v2.md`):
//!
//! - **bare** (`aiki remove`): Phase A only — delete *this user's* per-user
//!   enable marker for the repo. Local-only, reversible by `aiki init`, never
//!   prompts.
//! - **`--shared`**: Phase A + Phase B — also tear down the checked-in repo
//!   integration (`.aiki/`, the `<aiki>` block, the instruction symlink aiki
//!   created, and the git `core.hooksPath`). These are **working-tree changes**:
//!   committing them affects teammates, so `--shared` prompts unless `--force`
//!   and refuses to run non-interactively without `--force`.
//!
//! `--global` (Phase C — machine-wide teardown of editor hooks, the OTel
//! receiver, and `~/.aiki/`) is tracked separately and not yet wired here.

use crate::config;
use crate::error::Result;
use crate::global;
use crate::instructions;
use crate::repos::{self, InitState};
use anyhow::Context;
use std::fs;
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::process::Command;

/// Entry point for `aiki remove`.
pub fn run(shared: bool, global: bool, force: bool) -> Result<()> {
    // `--global` is path-independent (machine-wide teardown), so it does not
    // require being inside an aiki repo.
    if global {
        return run_global(shared, force);
    }

    let current_dir = std::env::current_dir().context("Failed to get current directory")?;
    let state = repos::init_state(&current_dir)?;

    if shared {
        run_shared(state, force)
    } else {
        run_bare(state)
    }
}

/// Bare `aiki remove`: delete only this user's marker. Never prompts.
fn run_bare(state: InitState) -> Result<()> {
    match state {
        InitState::Active { root } => {
            phase_a_remove_marker(&root)?;
            println!("{}", root.display());
            println!("✓ Disabled locally for this repo");
            println!("  Re-enable any time with `aiki init`.");
        }
        InitState::Dormant { .. } => {
            println!("ℹ Aiki is already disabled for you here.");
            println!(
                "  To remove aiki from this repo for everyone, run `aiki remove --shared`."
            );
        }
        InitState::OrphanedMarker { root } => {
            // A teammate ran `aiki remove --shared` and pushed the removal; only
            // our stale marker is left. Reap it.
            phase_a_remove_marker(&root)?;
            println!("✓ Removed a stale aiki marker (this repo's .aiki/ was already gone).");
        }
        InitState::NotAikiRepo => {
            return Err(anyhow::anyhow!(
                "Not in an aiki repository. Nothing to disable here.\n\n\
                 Run this from inside a repo that has been initialized with `aiki init`."
            )
            .into());
        }
    }
    Ok(())
}

/// `aiki remove --shared`: marker + checked-in repo teardown.
fn run_shared(state: InitState, force: bool) -> Result<()> {
    let root = match state {
        InitState::Active { root } | InitState::Dormant { root } => root,
        InitState::OrphanedMarker { .. } | InitState::NotAikiRepo => {
            return Err(anyhow::anyhow!(
                "Not in an aiki repository. Nothing to remove with --shared.\n\n\
                 Run this from inside a repo that has been initialized with `aiki init`."
            )
            .into());
        }
    };

    if !force {
        if !std::io::stderr().is_terminal() {
            return Err(anyhow::anyhow!(
                "Refusing to run `aiki remove --shared` non-interactively without --force.\n\
                 --shared edits the checked-in repo (.aiki/, the <aiki> block, git config);\n\
                 committing those changes affects everyone using this repo.\n\
                 Re-run with --force to proceed."
            )
            .into());
        }
        print_shared_prompt(&root);
        if !confirm()? {
            println!("Aborted. Nothing was changed.");
            return Ok(());
        }
    }

    phase_a_remove_marker(&root)?;
    phase_b_repo_teardown(&root)?;
    println!("✓ Removed aiki from this repo.");
    Ok(())
}

/// `aiki remove --global`: machine-wide teardown (Phase C), optionally preceded
/// by per-repo teardown of every enabled repo (`--shared --global`).
fn run_global(shared: bool, force: bool) -> Result<()> {
    if !force {
        if !std::io::stderr().is_terminal() {
            return Err(anyhow::anyhow!(
                "Refusing to run `aiki remove --global` non-interactively without --force.\n\
                 --global removes editor hooks, the OTel receiver, and ~/.aiki for this machine.\n\
                 Re-run with --force to proceed."
            )
            .into());
        }
        print_global_prompt(shared);
        if !confirm()? {
            println!("Aborted. Nothing was changed.");
            return Ok(());
        }
    }

    // `--shared --global`: tear down each enabled repo first. Phase C wipes the
    // marker registry en masse afterward, so we skip Phase A in the loop.
    if shared {
        for root in repos::enabled_repo_roots() {
            if root.join(".aiki").is_dir() {
                println!("\n{}:", root.display());
                if let Err(e) = phase_b_repo_teardown(&root) {
                    eprintln!("⚠ Could not fully remove {}: {e}", root.display());
                }
            }
        }
    }

    phase_c_machine_teardown()?;
    println!("✓ Removed aiki from this machine.");
    Ok(())
}

// ---------------------------------------------------------------------------
// Phase C — machine-wide integrations
// ---------------------------------------------------------------------------

fn phase_c_machine_teardown() -> Result<()> {
    // Editor hook configs (the tokenized match catches gated, bare, and .exe
    // command forms).
    let _ = config::remove_claude_code_hooks_global();
    let _ = config::remove_cursor_hooks_global();
    let _ = config::remove_codex_hooks_global();
    let _ = crate::editors::zed::remove_zed_config();
    println!("✓ Removed editor hook integrations (Claude/Cursor/Codex/Zed)");

    // OTel receiver service.
    let _ = config::uninstall_otel_receiver();
    println!("✓ Removed OTel receiver");

    // The global aiki home — this also wipes `~/.aiki/githooks/` and the entire
    // per-user marker registry as a side effect. Checked-in `.aiki/` directories
    // in repos are left untouched.
    let home = global::global_aiki_dir();
    if home.exists() {
        fs::remove_dir_all(&home)
            .with_context(|| format!("Failed to remove {}", home.display()))?;
        println!("✓ Removed {}", home.display());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Phase A — per-user enable marker
// ---------------------------------------------------------------------------

/// Delete the per-user enable marker for `root` and prune now-empty parent
/// directories. Idempotent: a missing marker is not an error.
fn phase_a_remove_marker(root: &Path) -> Result<()> {
    let marker = repos::marker_path(root);
    if marker.exists() {
        fs::remove_file(&marker)
            .with_context(|| format!("Failed to remove marker {}", marker.display()))?;
    }
    prune_empty_marker_dirs(&marker);
    Ok(())
}

/// `rmdir` empty marker parent directories up to — but not including —
/// `<global>/.init/repos/`, so one repo's removal doesn't disturb another's.
fn prune_empty_marker_dirs(marker: &Path) {
    let stop = global::global_aiki_dir().join(".init/repos");
    let mut dir = marker.parent().map(Path::to_path_buf);
    while let Some(d) = dir {
        if d == stop || !d.starts_with(&stop) {
            break;
        }
        // `remove_dir` only succeeds on an empty directory; a populated parent
        // (another enabled repo shares it) stops the walk.
        if fs::remove_dir(&d).is_err() {
            break;
        }
        dir = d.parent().map(Path::to_path_buf);
    }
}

// ---------------------------------------------------------------------------
// Phase B — checked-in repo teardown
// ---------------------------------------------------------------------------

fn phase_b_repo_teardown(root: &Path) -> Result<()> {
    // 1. Restore git core.hooksPath (reads .aiki/.previous_hooks_path, so this
    //    must run before we delete .aiki/).
    restore_hooks_path(root)?;

    // 2. Strip the <aiki> block from AGENTS.md / CLAUDE.md.
    instructions::remove_aiki_block(root, false)?;

    // 3. Take down the instruction symlink — but only the one aiki created
    //    (recorded in .aiki/.created_symlink). Reads inside .aiki/, so this
    //    must also run before .aiki/ is deleted.
    remove_aiki_symlink(root)?;

    // 4. Remove the .aiki/ directory.
    let aiki_dir = root.join(".aiki");
    if aiki_dir.exists() {
        fs::remove_dir_all(&aiki_dir)
            .with_context(|| format!("Failed to remove {}", aiki_dir.display()))?;
        println!("✓ Removed .aiki/");
    }

    // 5. .jj/ is deliberately left in place. Until support-existing-jj-repos.md
    //    lands, aiki cannot reliably tell a .jj/ it created from one the user
    //    owns, and losing version-control history is unrecoverable. Skip when in
    //    doubt.
    if root.join(".jj").exists() {
        println!(
            "ℹ Left .jj/ in place (aiki can't yet distinguish its own JJ repo from yours)."
        );
    }

    // 6. Drop the .aiki/.manifest.json entry aiki added to .gitignore.
    clean_gitignore(root)?;

    Ok(())
}

/// Restore `core.hooksPath` to whatever it was before `aiki init` set it.
///
/// `.aiki/.previous_hooks_path` records the prior value: a real path, the
/// literal `EMPTY` (it was set to an empty string), or the file is absent (it
/// was never set — so we unset).
fn restore_hooks_path(root: &Path) -> Result<()> {
    let prev_file = root.join(".aiki").join(".previous_hooks_path");
    let saved = fs::read_to_string(&prev_file)
        .ok()
        .map(|s| s.trim().to_string());

    match saved.as_deref() {
        Some("EMPTY") => git_config_set(root, "")?,
        Some(path) if !path.is_empty() => git_config_set(root, path)?,
        _ => git_config_unset(root)?,
    }
    println!("✓ Restored git core.hooksPath");
    Ok(())
}

fn git_config_set(root: &Path, value: &str) -> Result<()> {
    Command::new("git")
        .args(["config", "core.hooksPath", value])
        .current_dir(root)
        .output()
        .context("Failed to set git config core.hooksPath")?;
    Ok(())
}

fn git_config_unset(root: &Path) -> Result<()> {
    // Ignore failure: `--unset` exits non-zero when the key is already absent.
    let _ = Command::new("git")
        .args(["config", "--unset", "core.hooksPath"])
        .current_dir(root)
        .output();
    Ok(())
}

/// Remove the instruction symlink aiki created, if it still is one. Leaves
/// symlinks aiki didn't create (recorded ownership in `.aiki/.created_symlink`).
fn remove_aiki_symlink(root: &Path) -> Result<()> {
    let Some(name) = instructions::aiki_created_symlink(root) else {
        return Ok(());
    };
    let link = root.join(&name);
    let is_symlink = link
        .symlink_metadata()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    if is_symlink {
        fs::remove_file(&link)
            .with_context(|| format!("Failed to remove symlink {}", link.display()))?;
        println!("✓ Removed instruction symlink {}", name);
    }
    Ok(())
}

/// Remove the exact `.aiki/.manifest.json` line `aiki init` adds to
/// `.gitignore`. Broader user-authored patterns (e.g. `.aiki/`) are left alone.
fn clean_gitignore(root: &Path) -> Result<()> {
    let gitignore = root.join(".gitignore");
    let Ok(content) = fs::read_to_string(&gitignore) else {
        return Ok(());
    };
    let kept: Vec<&str> = content
        .lines()
        .filter(|line| {
            let t = line.trim();
            t != ".aiki/.manifest.json" && t != "/.aiki/.manifest.json"
        })
        .collect();
    if kept.len() == content.lines().count() {
        return Ok(()); // Nothing to remove.
    }
    if kept.iter().all(|l| l.trim().is_empty()) {
        // The file held only our entry — remove it entirely.
        let _ = fs::remove_file(&gitignore);
    } else {
        let mut out = kept.join("\n");
        out.push('\n');
        fs::write(&gitignore, out).context("Failed to rewrite .gitignore")?;
    }
    println!("✓ Cleaned .gitignore");
    Ok(())
}

// ---------------------------------------------------------------------------
// Prompting
// ---------------------------------------------------------------------------

fn print_shared_prompt(root: &Path) {
    println!("{}", root.display());
    println!();
    println!("Will modify this repo for everyone using it:");
    println!("  Repo (working tree changes):");
    println!("    - .aiki/                 (removed)");
    println!("    - <aiki> block in AGENTS.md / CLAUDE.md  (stripped)");
    println!("    - instruction symlink aiki created       (removed)");
    println!("    - git core.hooksPath     (restored)");
    println!("  Your per-user marker for this repo is also removed.");
    println!();
    println!("These are checked-in changes; committing them affects teammates.");
    println!("(To disable aiki for just yourself, run plain `aiki remove`.)");
    print!("Proceed? [y/N] ");
    let _ = std::io::stdout().flush();
}

fn print_global_prompt(shared: bool) {
    println!("Will remove aiki from this machine:");
    println!("  - Editor hook integrations (Claude, Cursor, Codex, Zed)");
    println!("  - OTel receiver service");
    println!("  - ~/.aiki/ (githooks + every per-user enable marker)");
    if shared {
        println!();
        println!("  --shared also: each enabled repo's checked-in integration");
        println!("  (.aiki/, the <aiki> block, instruction symlink, git config)");
        println!("  will be torn down too. Those are checked-in changes.");
    } else {
        println!();
        println!("Your enabled repos' checked-in .aiki/ directories are NOT removed.");
        println!("(Use `aiki remove --shared --global` for that.)");
    }
    print!("Proceed? [y/N] ");
    let _ = std::io::stdout().flush();
}

fn confirm() -> Result<bool> {
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .context("Failed to read confirmation")?;
    let answer = input.trim().to_lowercase();
    Ok(answer == "y" || answer == "yes")
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn phase_a_removes_marker_and_prunes_dirs() {
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        with_aiki_home(home.path(), || {
            let marker = repos::marker_path(repo.path());
            fs::create_dir_all(marker.parent().unwrap()).unwrap();
            fs::write(&marker, "").unwrap();

            phase_a_remove_marker(repo.path()).unwrap();

            assert!(!marker.exists(), "marker should be gone");
            // The per-repo marker directory should be pruned...
            assert!(!marker.parent().unwrap().exists());
            // ...but the shared registry root must survive.
            assert!(home.path().join(".init/repos").exists());
        });
    }

    #[test]
    fn phase_a_is_idempotent_when_marker_absent() {
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        with_aiki_home(home.path(), || {
            // No marker created — should not error.
            phase_a_remove_marker(repo.path()).unwrap();
        });
    }

    #[test]
    fn prune_stops_at_shared_parent() {
        let home = tempfile::tempdir().unwrap();
        with_aiki_home(home.path(), || {
            // Two repos share a parent directory in the registry.
            let repo_a = Path::new("/Users/me/code/alpha");
            let repo_b = Path::new("/Users/me/code/beta");
            for r in [repo_a, repo_b] {
                let m = repos::marker_path(r);
                fs::create_dir_all(m.parent().unwrap()).unwrap();
                fs::write(&m, "").unwrap();
            }

            phase_a_remove_marker(repo_a).unwrap();

            // alpha's dir is pruned; the shared `.../code/` parent survives
            // because beta still lives there.
            assert!(!repos::marker_path(repo_a).parent().unwrap().exists());
            assert!(repos::marker_path(repo_b).exists());
            let shared = home.path().join(".init/repos/Users/me/code");
            assert!(shared.exists(), "shared parent must survive");
        });
    }

    #[test]
    fn clean_gitignore_removes_only_manifest_line() {
        let repo = tempfile::tempdir().unwrap();
        let gitignore = repo.path().join(".gitignore");
        fs::write(&gitignore, "target/\n.aiki/.manifest.json\nnode_modules/\n").unwrap();

        clean_gitignore(repo.path()).unwrap();

        let after = fs::read_to_string(&gitignore).unwrap();
        assert!(!after.contains(".aiki/.manifest.json"));
        assert!(after.contains("target/"));
        assert!(after.contains("node_modules/"));
    }

    #[test]
    fn clean_gitignore_removes_file_when_only_entry() {
        let repo = tempfile::tempdir().unwrap();
        let gitignore = repo.path().join(".gitignore");
        fs::write(&gitignore, ".aiki/.manifest.json\n").unwrap();

        clean_gitignore(repo.path()).unwrap();

        assert!(!gitignore.exists(), "gitignore with only our entry is removed");
    }

    #[test]
    fn clean_gitignore_no_op_without_entry() {
        let repo = tempfile::tempdir().unwrap();
        let gitignore = repo.path().join(".gitignore");
        fs::write(&gitignore, "target/\n").unwrap();

        clean_gitignore(repo.path()).unwrap();

        assert_eq!(fs::read_to_string(&gitignore).unwrap(), "target/\n");
    }
}
