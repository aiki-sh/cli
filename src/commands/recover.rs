//! Recover command — surface preserved-but-unabsorbed session work.
//!
//! Workspace cleanup never silently destroys unabsorbed work: it is either
//! absorbed, preserved on an `aiki/recovered/*` bookmark, or the directory
//! is quarantined under `$AIKI_HOME/recovered-workspaces/`. This command
//! makes those safety nets discoverable:
//!
//! - `aiki recover` / `aiki recover list` — list recovery bookmarks in the
//!   current repo and quarantined workspace directories.
//! - `aiki recover reclaim <bookmark>` — drop a recovery bookmark whose
//!   changes are already in main's ancestry (safe disk/namespace reclaim).

use std::path::Path;

use crate::error::{AikiError, Result};
use crate::jj::{get_repo_root, jj_cmd};

/// Arguments for the recover command
#[derive(clap::Args)]
pub struct RecoverArgs {
    #[command(subcommand)]
    pub command: Option<RecoverCommand>,
}

#[derive(clap::Subcommand)]
pub enum RecoverCommand {
    /// List recovery bookmarks and quarantined workspace directories
    List,
    /// Remove a recovery bookmark whose changes are already in main's
    /// ancestry (refuses when the changes are not yet absorbed)
    Reclaim {
        /// Bookmark name (e.g. aiki/recovered/aiki-<session-uuid>)
        bookmark: String,
        /// Remove even if the changes are NOT in main's ancestry
        #[arg(long)]
        force: bool,
    },
}

pub fn run(args: RecoverArgs) -> Result<()> {
    let cwd = std::env::current_dir()
        .map_err(|e| AikiError::Other(anyhow::anyhow!("Failed to get cwd: {}", e)))?;
    let repo_root = get_repo_root(&cwd)?;

    match args.command.unwrap_or(RecoverCommand::List) {
        RecoverCommand::List => list(&repo_root),
        RecoverCommand::Reclaim { bookmark, force } => reclaim(&repo_root, &bookmark, force),
    }
}

/// A recovery bookmark with its ancestry status.
struct RecoveryBookmark {
    name: String,
    change_id: String,
    /// True when the bookmarked change is an ancestor of main's @
    /// (already absorbed; safe to reclaim).
    absorbed: bool,
}

fn list_recovery_bookmarks(repo_root: &Path) -> Result<Vec<RecoveryBookmark>> {
    let output = jj_cmd()
        .current_dir(repo_root)
        .args([
            "bookmark",
            "list",
            "--ignore-working-copy",
            "-T",
            r#"name ++ "\t" ++ normal_target.change_id() ++ "\n""#,
        ])
        .output()
        .map_err(|e| AikiError::Other(anyhow::anyhow!("Failed to list bookmarks: {}", e)))?;

    if !output.status.success() {
        return Err(AikiError::Other(anyhow::anyhow!(
            "jj bookmark list failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    let mut bookmarks = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut parts = line.splitn(2, '\t');
        let (Some(name), Some(change_id)) = (parts.next(), parts.next()) else {
            continue;
        };
        if !name.starts_with("aiki/recovered/") {
            continue;
        }
        let absorbed = change_in_main_ancestry(repo_root, change_id.trim());
        bookmarks.push(RecoveryBookmark {
            name: name.to_string(),
            change_id: change_id.trim().to_string(),
            absorbed,
        });
    }
    Ok(bookmarks)
}

fn change_in_main_ancestry(repo_root: &Path, change_id: &str) -> bool {
    let output = jj_cmd()
        .current_dir(repo_root)
        .args([
            "log",
            "-r",
            &format!("{} & ::@", change_id),
            "--no-graph",
            "-T",
            "change_id",
            "--limit",
            "1",
            "--ignore-working-copy",
        ])
        .output();
    match output {
        Ok(o) if o.status.success() => !String::from_utf8_lossy(&o.stdout).trim().is_empty(),
        _ => false,
    }
}

fn list(repo_root: &Path) -> Result<()> {
    let bookmarks = list_recovery_bookmarks(repo_root)?;

    if bookmarks.is_empty() {
        println!("No recovery bookmarks in this repo.");
    } else {
        println!("Recovery bookmarks ({}):", bookmarks.len());
        for bm in &bookmarks {
            let status = if bm.absorbed {
                "absorbed — safe to reclaim"
            } else {
                "UNABSORBED — contains work not in main"
            };
            println!(
                "- {} @ {} [{}]",
                bm.name,
                &bm.change_id[..bm.change_id.len().min(12)],
                status
            );
        }
        println!();
        println!("Inspect one with:  jj log -r <bookmark>  /  jj diff -r <bookmark>");
        println!("Fold into @ with:  jj squash --from <bookmark> --into @");
        println!("Reclaim with:      aiki recover reclaim <bookmark>");
    }

    // Quarantined directories (cross-repo — they live under $AIKI_HOME)
    let quarantine_root = crate::global::global_aiki_dir().join("recovered-workspaces");
    let mut quarantined: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&quarantine_root) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                quarantined.push(entry.path());
            }
        }
    }
    if !quarantined.is_empty() {
        println!();
        println!("Quarantined workspace directories ({}):", quarantined.len());
        for dir in &quarantined {
            println!("- {}", dir.display());
        }
        println!("These hold files that could not be preserved in jj (no repo root");
        println!("or failed snapshot). Copy anything you need, then delete the dir.");
    }

    Ok(())
}

fn reclaim(repo_root: &Path, bookmark: &str, force: bool) -> Result<()> {
    if !bookmark.starts_with("aiki/recovered/") {
        return Err(AikiError::Other(anyhow::anyhow!(
            "refusing to touch non-recovery bookmark '{}' (must start with aiki/recovered/)",
            bookmark
        )));
    }

    let bookmarks = list_recovery_bookmarks(repo_root)?;
    let Some(bm) = bookmarks.iter().find(|b| b.name == bookmark) else {
        return Err(AikiError::Other(anyhow::anyhow!(
            "recovery bookmark '{}' not found",
            bookmark
        )));
    };

    if !bm.absorbed && !force {
        return Err(AikiError::Other(anyhow::anyhow!(
            "bookmark '{}' points at changes NOT in main's ancestry — \
             fold them in first (jj squash --from {} --into @) or pass --force to discard",
            bookmark,
            bookmark
        )));
    }

    let output = jj_cmd()
        .current_dir(repo_root)
        .args([
            "bookmark",
            "delete",
            bookmark,
            "--ignore-working-copy",
        ])
        .output()
        .map_err(|e| AikiError::Other(anyhow::anyhow!("Failed to delete bookmark: {}", e)))?;

    if !output.status.success() {
        return Err(AikiError::Other(anyhow::anyhow!(
            "jj bookmark delete failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    println!("Reclaimed {}", bookmark);
    Ok(())
}
