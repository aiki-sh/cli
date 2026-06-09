/// Shared instruction file detection logic used by init and doctor.
///
/// Handles detecting which instruction file (AGENTS.md or CLAUDE.md) is canonical,
/// ensuring the <aiki> block is present, and managing symlinks between them.
use crate::commands::agents_template::{aiki_block_hash, aiki_block_template};
use crate::error::Result;
use anyhow::Context;
use std::fs;
use std::path::Path;

pub const AGENTS_MD: &str = "AGENTS.md";
pub const CLAUDE_MD: &str = "CLAUDE.md";

/// Describes the on-disk state of instruction files and what actions are needed.
#[derive(Debug)]
pub enum RepoInstructionsKind {
    /// Both AGENTS.md and CLAUDE.md exist as real files.
    /// Action: add <aiki> block to both. No symlink.
    BothFiles,

    /// Both AGENTS.md and CLAUDE.md are symlinks (e.g., to external files).
    /// Action: warn/error — writing through symlinks could have unexpected effects.
    BothSymlinks,

    /// One real file + the other is a symlink pointing to it.
    /// Action: add <aiki> block to canonical only (symlink already covers it).
    FileWithSymlink {
        canonical: &'static str,
        symlink: &'static str,
    },

    /// Only one file exists (as a real file). The other is absent.
    /// Action: add <aiki> block to existing, create symlink for missing.
    FileWithoutSymlink {
        existing: &'static str,
        missing: &'static str,
    },

    /// Both files are missing.
    /// Action: create AGENTS.md with scaffold + block, symlink CLAUDE.md → AGENTS.md.
    Missing,
}

/// Detect the current instruction file state.
/// Pure detection — no side effects.
pub fn detect_instructions_kind(repo_root: &Path) -> RepoInstructionsKind {
    let agents_path = repo_root.join(AGENTS_MD);
    let claude_path = repo_root.join(CLAUDE_MD);
    let agents_exists = agents_path.exists();
    let claude_exists = claude_path.exists();
    let agents_is_symlink = agents_path
        .symlink_metadata()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    let claude_is_symlink = claude_path
        .symlink_metadata()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);

    match (agents_exists, claude_exists) {
        (true, true) => match (agents_is_symlink, claude_is_symlink) {
            (true, false) => RepoInstructionsKind::FileWithSymlink {
                canonical: CLAUDE_MD,
                symlink: AGENTS_MD,
            },
            (false, true) => RepoInstructionsKind::FileWithSymlink {
                canonical: AGENTS_MD,
                symlink: CLAUDE_MD,
            },
            (true, true) => RepoInstructionsKind::BothSymlinks,
            (false, false) => RepoInstructionsKind::BothFiles,
        },
        (true, false) if !agents_is_symlink => RepoInstructionsKind::FileWithoutSymlink {
            existing: AGENTS_MD,
            missing: CLAUDE_MD,
        },
        (false, true) if !claude_is_symlink => RepoInstructionsKind::FileWithoutSymlink {
            existing: CLAUDE_MD,
            missing: AGENTS_MD,
        },
        _ => RepoInstructionsKind::Missing,
    }
}

/// Ensure instruction files are correctly set up. Single entry point for
/// both init and doctor --fix.
pub fn ensure_instruction_files(repo_root: &Path, quiet: bool) -> Result<()> {
    match detect_instructions_kind(repo_root) {
        RepoInstructionsKind::BothFiles => {
            ensure_aiki_block(repo_root, AGENTS_MD, quiet)?;
            ensure_aiki_block(repo_root, CLAUDE_MD, quiet)?;
        }
        RepoInstructionsKind::BothSymlinks => {
            if !quiet {
                eprintln!(
                    "⚠ Both {} and {} are symlinks — skipping instruction file setup.\n  \
                     Aiki cannot safely write through symlinks that may point to shared files.\n  \
                     To fix: replace one symlink with a regular file, then re-run aiki init.",
                    AGENTS_MD, CLAUDE_MD
                );
            }
        }
        RepoInstructionsKind::FileWithSymlink {
            canonical, symlink, ..
        } => {
            ensure_aiki_block(repo_root, canonical, quiet)?;
            ensure_symlink(repo_root, canonical, symlink, quiet)?;
        }
        RepoInstructionsKind::FileWithoutSymlink { existing, missing } => {
            ensure_aiki_block(repo_root, existing, quiet)?;
            ensure_symlink(repo_root, existing, missing, quiet)?;
        }
        RepoInstructionsKind::Missing => {
            ensure_aiki_block_with_scaffold(repo_root, AGENTS_MD, quiet)?;
            ensure_symlink(repo_root, AGENTS_MD, CLAUDE_MD, quiet)?;
        }
    }
    Ok(())
}

fn ensure_aiki_block_with_scaffold(repo_root: &Path, filename: &str, quiet: bool) -> Result<()> {
    let file_path = repo_root.join(filename);

    // Remove dangling symlink if present
    if !file_path.exists() {
        if let Ok(meta) = file_path.symlink_metadata() {
            if meta.file_type().is_symlink() {
                fs::remove_file(&file_path)?;
            }
        }
    }

    let content = format!(
        "# Repo Instructions\n\
         \n\
         <!-- Add your repo-specific instructions for AI agents below. -->\n\
         <!-- These instructions are shared across Cursor, Codex, and other AI tools that read AGENTS.md. -->\n\
         <!-- Claude Code is supported via a CLAUDE.md symlink that points to this file. -->\n\
         \n\
         {}\n",
        aiki_block_template()
    );
    fs::write(&file_path, content)
        .with_context(|| format!("Failed to create {}", filename))?;

    if !quiet {
        println!("✓ Created {} with task system instructions", filename);
        println!(
            "  Tip: Add your repo instructions to {} above the <aiki> block.",
            filename
        );
    }
    Ok(())
}

/// Ensure the <aiki> block is present in the given instruction file.
///
/// - If file exists and has current block -> no-op, print checkmark
/// - If file exists with outdated block -> replace with current block
/// - If file exists without block -> prepend block
/// - If file doesn't exist -> create it with block
pub fn ensure_aiki_block(repo_root: &Path, filename: &str, quiet: bool) -> Result<()> {
    let file_path = repo_root.join(filename);

    // Remove dangling symlink so we can create a fresh file
    if !file_path.exists() {
        if let Ok(meta) = file_path.symlink_metadata() {
            if meta.file_type().is_symlink() {
                fs::remove_file(&file_path)
                    .with_context(|| format!("Failed to remove dangling symlink {}", filename))?;
            }
        }
    }

    if file_path.exists() {
        let content = fs::read_to_string(&file_path)
            .with_context(|| format!("Failed to read {}", filename))?;

        if !content.contains("<aiki version=") {
            // Prepend block
            let updated = format!("{}\n{}", aiki_block_template(), content);
            fs::write(&file_path, updated)
                .with_context(|| format!("Failed to update {}", filename))?;
            if !quiet {
                println!("✓ Added <aiki> block to {}", filename);
            }
        } else if !content.contains(&format!("hash=\"{}\"", aiki_block_hash())) {
            // Content is outdated — replace the old block
            let start = content
                .find("<aiki version=")
                .expect("already checked content contains <aiki version=");
            let end_tag = "</aiki>";
            let end = content[start..]
                .find(end_tag)
                .map(|pos| start + pos + end_tag.len())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Malformed <aiki> block in {}: missing </aiki> closing tag",
                        filename
                    )
                })?;
            // Skip a trailing newline if present
            let end = if content[end..].starts_with("\r\n") {
                end + 2
            } else if content[end..].starts_with('\n') {
                end + 1
            } else {
                end
            };
            let updated = format!(
                "{}{}{}",
                &content[..start],
                aiki_block_template(),
                &content[end..]
            );
            fs::write(&file_path, updated)
                .with_context(|| format!("Failed to update {}", filename))?;
            if !quiet {
                println!("✓ Updated <aiki> block in {}", filename);
            }
        } else if !quiet {
            println!("✓ {} already has <aiki> block", filename);
        }
    } else {
        // Create new file with just the block
        fs::write(&file_path, aiki_block_template())
            .with_context(|| format!("Failed to create {}", filename))?;
        if !quiet {
            println!("✓ Created {} with task system instructions", filename);
        }
    }

    Ok(())
}

/// Create a symlink from `link_name` -> `target_name` in repo_root.
///
/// - If symlink already exists pointing to correct target -> no-op, print checkmark
/// - If symlink exists with wrong target -> remove and recreate
/// - If path exists as real file -> warn and skip
pub fn ensure_symlink(
    repo_root: &Path,
    target_name: &str,
    link_name: &str,
    quiet: bool,
) -> Result<()> {
    let link_path = repo_root.join(link_name);

    if link_path.exists() || link_path.symlink_metadata().is_ok() {
        let metadata = link_path
            .symlink_metadata()
            .with_context(|| format!("Failed to read metadata for {}", link_name))?;

        if metadata.file_type().is_symlink() {
            // Check if it points to the correct target
            let current_target = fs::read_link(&link_path)
                .with_context(|| format!("Failed to read symlink {}", link_name))?;

            if current_target.to_string_lossy() == target_name {
                if !quiet {
                    println!("✓ {} already symlinked to {}", link_name, target_name);
                }
                return Ok(());
            }

            // Wrong target -> remove and recreate
            fs::remove_file(&link_path)
                .with_context(|| format!("Failed to remove old symlink {}", link_name))?;
        } else {
            // Real file exists — can't create symlink
            if !quiet {
                println!("⚠ {} exists as a regular file, skipping symlink", link_name);
            }
            return Ok(());
        }
    }

    // Create the symlink
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        symlink(target_name, &link_path).with_context(|| {
            format!("Failed to create symlink {} -> {}", link_name, target_name)
        })?;
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::symlink_file;
        symlink_file(target_name, &link_path).with_context(|| {
            format!("Failed to create symlink {} -> {}", link_name, target_name)
        })?;
    }

    if !quiet {
        println!("✓ Created symlink {} -> {}", link_name, target_name);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn detect_both_real_files() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(AGENTS_MD), "agents").unwrap();
        fs::write(dir.path().join(CLAUDE_MD), "claude").unwrap();
        assert!(matches!(
            detect_instructions_kind(dir.path()),
            RepoInstructionsKind::BothFiles
        ));
    }

    #[test]
    fn detect_file_with_symlink_agents_canonical() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(AGENTS_MD), "agents").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(AGENTS_MD, dir.path().join(CLAUDE_MD)).unwrap();
        #[cfg(unix)]
        assert!(matches!(
            detect_instructions_kind(dir.path()),
            RepoInstructionsKind::FileWithSymlink {
                canonical: "AGENTS.md",
                symlink: "CLAUDE.md",
            }
        ));
    }

    #[test]
    fn detect_file_with_symlink_claude_canonical() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(CLAUDE_MD), "claude").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(CLAUDE_MD, dir.path().join(AGENTS_MD)).unwrap();
        #[cfg(unix)]
        assert!(matches!(
            detect_instructions_kind(dir.path()),
            RepoInstructionsKind::FileWithSymlink {
                canonical: "CLAUDE.md",
                symlink: "AGENTS.md",
            }
        ));
    }

    #[test]
    fn detect_file_without_symlink_agents_only() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(AGENTS_MD), "agents").unwrap();
        assert!(matches!(
            detect_instructions_kind(dir.path()),
            RepoInstructionsKind::FileWithoutSymlink {
                existing: "AGENTS.md",
                missing: "CLAUDE.md",
            }
        ));
    }

    #[test]
    fn detect_file_without_symlink_claude_only() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(CLAUDE_MD), "claude").unwrap();
        assert!(matches!(
            detect_instructions_kind(dir.path()),
            RepoInstructionsKind::FileWithoutSymlink {
                existing: "CLAUDE.md",
                missing: "AGENTS.md",
            }
        ));
    }

    #[test]
    fn detect_missing_both() {
        let dir = tempdir().unwrap();
        assert!(matches!(
            detect_instructions_kind(dir.path()),
            RepoInstructionsKind::Missing
        ));
    }

    #[cfg(unix)]
    #[test]
    fn detect_both_symlinks() {
        let dir = tempdir().unwrap();
        // Create external targets for the symlinks
        fs::write(dir.path().join("ext_agents"), "agents").unwrap();
        fs::write(dir.path().join("ext_claude"), "claude").unwrap();
        std::os::unix::fs::symlink("ext_agents", dir.path().join(AGENTS_MD)).unwrap();
        std::os::unix::fs::symlink("ext_claude", dir.path().join(CLAUDE_MD)).unwrap();
        assert!(matches!(
            detect_instructions_kind(dir.path()),
            RepoInstructionsKind::BothSymlinks
        ));
    }

    #[cfg(unix)]
    #[test]
    fn detect_dangling_symlink() {
        let dir = tempdir().unwrap();
        // Symlink to nonexistent target — dangling
        std::os::unix::fs::symlink("nonexistent", dir.path().join(AGENTS_MD)).unwrap();
        // .exists() returns false for dangling symlinks
        assert!(matches!(
            detect_instructions_kind(dir.path()),
            RepoInstructionsKind::Missing
        ));
    }

    #[test]
    fn ensure_both_files_get_block() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(AGENTS_MD), "# My agents instructions\n").unwrap();
        fs::write(dir.path().join(CLAUDE_MD), "# My claude instructions\n").unwrap();
        ensure_instruction_files(dir.path(), true).unwrap();
        let agents = fs::read_to_string(dir.path().join(AGENTS_MD)).unwrap();
        let claude = fs::read_to_string(dir.path().join(CLAUDE_MD)).unwrap();
        assert!(agents.contains("<aiki version="));
        assert!(claude.contains("<aiki version="));
    }

    #[test]
    fn ensure_both_files_idempotent() {
        let dir = tempdir().unwrap();
        let block = aiki_block_template();
        fs::write(dir.path().join(AGENTS_MD), &block).unwrap();
        fs::write(dir.path().join(CLAUDE_MD), &block).unwrap();
        ensure_instruction_files(dir.path(), true).unwrap();
        let agents = fs::read_to_string(dir.path().join(AGENTS_MD)).unwrap();
        // Should have exactly one block, not duplicated
        assert_eq!(agents.matches("<aiki version=").count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn ensure_symlink_preserved() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(AGENTS_MD), "# agents\n").unwrap();
        std::os::unix::fs::symlink(AGENTS_MD, dir.path().join(CLAUDE_MD)).unwrap();
        ensure_instruction_files(dir.path(), true).unwrap();
        let agents = fs::read_to_string(dir.path().join(AGENTS_MD)).unwrap();
        assert!(agents.contains("<aiki version="));
        assert!(dir.path().join(CLAUDE_MD).symlink_metadata().unwrap().file_type().is_symlink());
    }

    #[test]
    fn ensure_missing_creates_scaffold() {
        let dir = tempdir().unwrap();
        ensure_instruction_files(dir.path(), true).unwrap();
        let agents = fs::read_to_string(dir.path().join(AGENTS_MD)).unwrap();
        assert!(agents.contains("# Repo Instructions"));
        assert!(agents.contains("<aiki version="));
        #[cfg(unix)]
        assert!(dir.path().join(CLAUDE_MD).symlink_metadata().unwrap().file_type().is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn ensure_both_symlinks_skips() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("ext_agents"), "agents content").unwrap();
        fs::write(dir.path().join("ext_claude"), "claude content").unwrap();
        std::os::unix::fs::symlink("ext_agents", dir.path().join(AGENTS_MD)).unwrap();
        std::os::unix::fs::symlink("ext_claude", dir.path().join(CLAUDE_MD)).unwrap();
        // Should not error, just warn and skip
        ensure_instruction_files(dir.path(), true).unwrap();
        // Files should be unchanged — no <aiki> block injected through symlinks
        let agents = fs::read_to_string(dir.path().join("ext_agents")).unwrap();
        assert!(!agents.contains("<aiki version="));
    }
}
