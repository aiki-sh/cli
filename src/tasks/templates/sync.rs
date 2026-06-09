use crate::error::Result;
use crate::tasks::templates::builtin::default_plugin_templates;
use crate::tasks::templates::manifest::{checksum, FileEntry, RepoManifest};
use crate::tasks::templates::TASKS_DIR_NAME;
use chrono::Utc;
use std::path::Path;

/// Summary of what a sync operation did.
#[derive(Debug)]
pub struct SyncReport {
    pub installed: usize,
    pub upgraded: usize,
    pub installed_names: Vec<String>,
    pub upgraded_names: Vec<String>,
    pub skipped_dirty: Vec<String>,
    pub pruned: Vec<String>,
}

/// Sync a plugin's embedded templates to disk, respecting user modifications.
///
/// For each template in `source_templates`:
/// - Fresh install: no manifest entry, no file on disk → write file, add to manifest
/// - Adoption: no manifest entry but file exists → compare hashes to decide clean/dirty
/// - Re-install: manifest entry exists but file deleted → write from source
/// - Up to date: source checksum == manifest checksum → skip
/// - Upgrade (clean): new version available, on-disk matches manifest → overwrite
/// - Upgrade (dirty): new version available, on-disk differs from manifest → skip (user modified)
///
/// Prune step: manifest entries for templates no longer in source are removed (files left on disk).
pub fn sync_plugin_templates(
    manifest: &mut RepoManifest,
    plugin_ref: &str,
    install_root: &str,
    source_version: &str,
    source_templates: &[(&str, &[u8])],
    templates_dir: &Path,
) -> Result<SyncReport> {
    // Path validation: ensure no template escapes templates_dir
    // Normalize the base path (resolve symlinks like /tmp -> /private/tmp on macOS)
    let canonical_base = templates_dir
        .canonicalize()
        .unwrap_or_else(|_| templates_dir.to_path_buf());

    for (rel_path, _) in source_templates {
        // Normalize by resolving .. components without requiring files to exist
        let target = canonical_base.join(install_root).join(rel_path);
        let mut normalized = std::path::PathBuf::new();
        for component in target.components() {
            match component {
                std::path::Component::ParentDir => {
                    normalized.pop();
                }
                _ => normalized.push(component),
            }
        }
        if !normalized.starts_with(&canonical_base) {
            return Err(anyhow::anyhow!(
                "Template path traversal detected in plugin '{}': '{}' resolves outside templates directory",
                plugin_ref,
                rel_path
            )
            .into());
        }
    }

    let plugin = manifest.get_or_create_plugin(plugin_ref, source_version, install_root);
    let now = Utc::now().to_rfc3339();

    let mut report = SyncReport {
        installed: 0,
        upgraded: 0,
        installed_names: Vec::new(),
        upgraded_names: Vec::new(),
        skipped_dirty: Vec::new(),
        pruned: Vec::new(),
    };

    // Build set of source template paths for prune step
    let source_paths: std::collections::HashSet<&str> =
        source_templates.iter().map(|(p, _)| *p).collect();

    for (rel_path, content) in source_templates {
        let disk_path = templates_dir.join(install_root).join(rel_path);
        let source_cksum = checksum(content);
        let manifest_entry = plugin.files.get(*rel_path);

        match manifest_entry {
            None => {
                // No manifest entry
                if disk_path.exists() {
                    // Adoption: file exists but not tracked
                    let on_disk = std::fs::read(&disk_path)?;
                    let disk_cksum = checksum(&on_disk);
                    if disk_cksum == source_cksum {
                        // Clean adoption — just track it
                        plugin.files.insert(
                            rel_path.to_string(),
                            FileEntry {
                                checksum: source_cksum,
                                version: parse_frontmatter_version(content),
                                installed_at: now.clone(),
                            },
                        );
                        report.installed += 1;
                        report.installed_names.push(rel_path.to_string());
                    } else {
                        // Dirty adoption — file exists on disk with different content than source.
                        // We intentionally store `source_cksum` (not `disk_cksum`) because:
                        //   - Future same-source syncs see `source_cksum == entry.checksum` → "up to date, skip"
                        //     → user's file is left alone (desired protective behavior)
                        //   - If we stored `disk_cksum`, future syncs would see `disk == manifest`
                        //     → file looks "clean" → sync would overwrite user content
                        // Trade-off: doctor.rs will see disk_cksum != manifest checksum and report
                        // the file as "modified". This is a known cosmetic mismatch, not a bug.
                        plugin.files.insert(
                            rel_path.to_string(),
                            FileEntry {
                                checksum: source_cksum,
                                version: parse_frontmatter_version(content),
                                installed_at: now.clone(),
                            },
                        );
                        report.skipped_dirty.push(rel_path.to_string());
                    }
                } else {
                    // Fresh install
                    if let Some(parent) = disk_path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&disk_path, content)?;
                    plugin.files.insert(
                        rel_path.to_string(),
                        FileEntry {
                            checksum: source_cksum,
                            version: parse_frontmatter_version(content),
                            installed_at: now.clone(),
                        },
                    );
                    report.installed += 1;
                    report.installed_names.push(rel_path.to_string());
                }
            }
            Some(entry) => {
                if !disk_path.exists() {
                    // Re-install: manifest entry but file deleted
                    if let Some(parent) = disk_path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&disk_path, content)?;
                    plugin.files.insert(
                        rel_path.to_string(),
                        FileEntry {
                            checksum: source_cksum,
                            version: parse_frontmatter_version(content),
                            installed_at: now.clone(),
                        },
                    );
                    report.installed += 1;
                    report.installed_names.push(rel_path.to_string());
                } else if source_cksum == entry.checksum {
                    // Up to date — skip
                } else {
                    // New version available
                    let on_disk = std::fs::read(&disk_path)?;
                    let disk_cksum = checksum(&on_disk);
                    if disk_cksum == entry.checksum {
                        // Clean — safe to overwrite
                        std::fs::write(&disk_path, content)?;
                        plugin.files.insert(
                            rel_path.to_string(),
                            FileEntry {
                                checksum: source_cksum,
                                version: parse_frontmatter_version(content),
                                installed_at: now.clone(),
                            },
                        );
                        report.upgraded += 1;
                        report.upgraded_names.push(rel_path.to_string());
                    } else {
                        // Dirty — user modified, skip
                        report.skipped_dirty.push(rel_path.to_string());
                    }
                }
            }
        }
    }

    // Prune: remove manifest entries for templates no longer in source
    let to_prune: Vec<String> = plugin
        .files
        .keys()
        .filter(|k| !source_paths.contains(k.as_str()))
        .cloned()
        .collect();
    for key in &to_prune {
        plugin.files.remove(key);
        report.pruned.push(key.clone());
    }

    // Update source version
    plugin.source_version = source_version.to_string();

    Ok(report)
}

/// Sync built-in default templates to disk.
pub fn sync_default_templates(repo_root: &Path, quiet: bool) -> Result<SyncReport> {
    let mut manifest = RepoManifest::load(repo_root)?.unwrap_or_else(RepoManifest::new);

    let templates_dir = repo_root.join(".aiki").join(TASKS_DIR_NAME);
    std::fs::create_dir_all(&templates_dir)?;

    let source_templates = default_plugin_templates();
    let report = sync_plugin_templates(
        &mut manifest,
        "aiki/default",
        ".",
        env!("CARGO_PKG_VERSION"),
        &source_templates,
        &templates_dir,
    )?;

    manifest.save(repo_root)?;

    if !quiet {
        if report.installed > 0 {
            println!(
                "✓ Installed {} built-in template(s): {}",
                report.installed,
                report.installed_names.join(", ")
            );
        }
        if report.upgraded > 0 {
            println!(
                "✓ Updated {} built-in template(s): {}",
                report.upgraded,
                report.upgraded_names.join(", ")
            );
        }
        if !report.skipped_dirty.is_empty() {
            println!(
                "⚠ {} template(s) modified locally (skipped): {}",
                report.skipped_dirty.len(),
                report.skipped_dirty.join(", ")
            );
        }
        if !report.pruned.is_empty() {
            println!(
                "✓ Pruned {} obsolete template entry(ies)",
                report.pruned.len()
            );
        }
    }

    // Ensure .manifest.json is gitignored
    ensure_manifest_gitignored(repo_root, quiet)?;

    Ok(report)
}

/// Parse `version:` from YAML frontmatter if present.
fn parse_frontmatter_version(content: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(content).ok()?;
    // Frontmatter is delimited by --- lines
    if !text.starts_with("---") {
        return None;
    }
    let after_first = &text[3..];
    let end = after_first.find("\n---")?;
    let frontmatter = &after_first[..end];
    for line in frontmatter.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("version:") {
            let version = rest.trim().trim_matches('"').trim_matches('\'');
            if !version.is_empty() {
                return Some(version.to_string());
            }
        }
    }
    None
}

/// Ensure `.aiki/.manifest.json` is covered by `.gitignore`.
fn ensure_manifest_gitignored(repo_root: &Path, quiet: bool) -> Result<()> {
    let gitignore_path = repo_root.join(".gitignore");
    let manifest_pattern = ".aiki/.manifest.json";

    if gitignore_path.exists() {
        let content = std::fs::read_to_string(&gitignore_path)?;
        // Check if already covered (exact match or broader covering pattern)
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed == manifest_pattern
                || trimmed == "/.aiki/.manifest.json"
                || trimmed == ".aiki/"
                || trimmed == ".aiki/*"
                || trimmed == "/.aiki/"
                || trimmed == "/.aiki/*"
            {
                return Ok(());
            }
        }
        // Not found — append
        let mut new_content = content;
        if !new_content.ends_with('\n') {
            new_content.push('\n');
        }
        new_content.push_str(manifest_pattern);
        new_content.push('\n');
        std::fs::write(&gitignore_path, new_content)?;
        if !quiet {
            println!("✓ Added {} to .gitignore", manifest_pattern);
        }
    } else {
        std::fs::write(&gitignore_path, format!("{}\n", manifest_pattern))?;
        if !quiet {
            println!("✓ Created .gitignore with {}", manifest_pattern);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::templates::manifest::RepoManifest;
    use tempfile::TempDir;

    fn setup_dir() -> TempDir {
        TempDir::new().unwrap()
    }

    #[test]
    fn test_fresh_install() {
        let dir = setup_dir();
        let templates_dir = dir.path().join("templates");
        std::fs::create_dir_all(&templates_dir).unwrap();

        let mut manifest = RepoManifest::new();
        let source = vec![
            ("review.md", b"# Review\nContent" as &[u8]),
            ("plan.md", b"---\nversion: 1.0.0\n---\n# Plan" as &[u8]),
        ];

        let report = sync_plugin_templates(
            &mut manifest,
            "test/plugin",
            ".",
            "0.1.0",
            &source,
            &templates_dir,
        )
        .unwrap();

        assert_eq!(report.installed, 2);
        assert_eq!(report.upgraded, 0);
        assert!(report.skipped_dirty.is_empty());
        assert!(report.pruned.is_empty());

        // Files written
        assert!(templates_dir.join("review.md").exists());
        assert!(templates_dir.join("plan.md").exists());

        // Manifest updated
        let plugin = manifest.get_plugin("test/plugin").unwrap();
        assert_eq!(plugin.files.len(), 2);
        assert_eq!(
            plugin.files.get("plan.md").unwrap().version,
            Some("1.0.0".to_string())
        );
        assert_eq!(plugin.files.get("review.md").unwrap().version, None);
    }

    #[test]
    fn test_upgrade_clean() {
        let dir = setup_dir();
        let templates_dir = dir.path().join("templates");
        std::fs::create_dir_all(&templates_dir).unwrap();

        let old_content = b"# Old content";
        let new_content = b"# New content";

        // Write old file
        std::fs::write(templates_dir.join("review.md"), old_content).unwrap();

        // Set up manifest with old checksum
        let mut manifest = RepoManifest::new();
        let plugin = manifest.get_or_create_plugin("test/plugin", "0.1.0", ".");
        plugin.files.insert(
            "review.md".to_string(),
            FileEntry {
                checksum: checksum(old_content),
                version: None,
                installed_at: "2026-01-01T00:00:00Z".to_string(),
            },
        );

        let source = vec![("review.md", new_content as &[u8])];
        let report = sync_plugin_templates(
            &mut manifest,
            "test/plugin",
            ".",
            "0.2.0",
            &source,
            &templates_dir,
        )
        .unwrap();

        assert_eq!(report.upgraded, 1);
        assert_eq!(report.installed, 0);
        assert!(report.skipped_dirty.is_empty());

        // File updated
        let on_disk = std::fs::read(templates_dir.join("review.md")).unwrap();
        assert_eq!(on_disk, new_content);
    }

    #[test]
    fn test_dirty_skip() {
        let dir = setup_dir();
        let templates_dir = dir.path().join("templates");
        std::fs::create_dir_all(&templates_dir).unwrap();

        let old_source = b"# Old source";
        let user_modified = b"# User modified this file";
        let new_source = b"# New source";

        // Write user-modified file
        std::fs::write(templates_dir.join("review.md"), user_modified).unwrap();

        // Manifest has old source checksum
        let mut manifest = RepoManifest::new();
        let plugin = manifest.get_or_create_plugin("test/plugin", "0.1.0", ".");
        plugin.files.insert(
            "review.md".to_string(),
            FileEntry {
                checksum: checksum(old_source),
                version: None,
                installed_at: "2026-01-01T00:00:00Z".to_string(),
            },
        );

        let source = vec![("review.md", new_source as &[u8])];
        let report = sync_plugin_templates(
            &mut manifest,
            "test/plugin",
            ".",
            "0.2.0",
            &source,
            &templates_dir,
        )
        .unwrap();

        assert_eq!(report.upgraded, 0);
        assert_eq!(report.installed, 0);
        assert_eq!(report.skipped_dirty, vec!["review.md"]);

        // File preserved
        let on_disk = std::fs::read(templates_dir.join("review.md")).unwrap();
        assert_eq!(on_disk, user_modified);
    }

    #[test]
    fn test_reinstall_deleted() {
        let dir = setup_dir();
        let templates_dir = dir.path().join("templates");
        std::fs::create_dir_all(&templates_dir).unwrap();

        let content = b"# Template content";

        // Manifest entry exists but file is gone
        let mut manifest = RepoManifest::new();
        let plugin = manifest.get_or_create_plugin("test/plugin", "0.1.0", ".");
        plugin.files.insert(
            "review.md".to_string(),
            FileEntry {
                checksum: checksum(b"old content"),
                version: None,
                installed_at: "2026-01-01T00:00:00Z".to_string(),
            },
        );

        let source = vec![("review.md", content as &[u8])];
        let report = sync_plugin_templates(
            &mut manifest,
            "test/plugin",
            ".",
            "0.2.0",
            &source,
            &templates_dir,
        )
        .unwrap();

        assert_eq!(report.installed, 1);
        assert!(templates_dir.join("review.md").exists());
    }

    #[test]
    fn test_prune() {
        let dir = setup_dir();
        let templates_dir = dir.path().join("templates");
        std::fs::create_dir_all(&templates_dir).unwrap();

        // Write a file that will be pruned
        std::fs::write(templates_dir.join("obsolete.md"), b"old").unwrap();

        let mut manifest = RepoManifest::new();
        let plugin = manifest.get_or_create_plugin("test/plugin", "0.1.0", ".");
        plugin.files.insert(
            "obsolete.md".to_string(),
            FileEntry {
                checksum: checksum(b"old"),
                version: None,
                installed_at: "2026-01-01T00:00:00Z".to_string(),
            },
        );
        plugin.files.insert(
            "keep.md".to_string(),
            FileEntry {
                checksum: checksum(b"keep content"),
                version: None,
                installed_at: "2026-01-01T00:00:00Z".to_string(),
            },
        );

        // Source only has keep.md
        let source = vec![("keep.md", b"keep content" as &[u8])];
        let report = sync_plugin_templates(
            &mut manifest,
            "test/plugin",
            ".",
            "0.2.0",
            &source,
            &templates_dir,
        )
        .unwrap();

        assert_eq!(report.pruned, vec!["obsolete.md"]);
        // File still on disk
        assert!(templates_dir.join("obsolete.md").exists());
        // Manifest entry removed
        let plugin = manifest.get_plugin("test/plugin").unwrap();
        assert!(!plugin.files.contains_key("obsolete.md"));
        assert!(plugin.files.contains_key("keep.md"));
    }

    #[test]
    fn test_path_traversal_rejected() {
        let dir = setup_dir();
        let templates_dir = dir.path().join("templates");
        std::fs::create_dir_all(&templates_dir).unwrap();

        let mut manifest = RepoManifest::new();
        let source = vec![("../../../etc/passwd", b"malicious" as &[u8])];

        let result = sync_plugin_templates(
            &mut manifest,
            "evil/plugin",
            ".",
            "0.1.0",
            &source,
            &templates_dir,
        );

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("path traversal"));
    }

    #[test]
    fn test_adoption_clean() {
        let dir = setup_dir();
        let templates_dir = dir.path().join("templates");
        std::fs::create_dir_all(&templates_dir).unwrap();

        let content = b"# Template";

        // File exists on disk but not in manifest
        std::fs::write(templates_dir.join("review.md"), content).unwrap();

        let mut manifest = RepoManifest::new();
        let source = vec![("review.md", content as &[u8])];

        let report = sync_plugin_templates(
            &mut manifest,
            "test/plugin",
            ".",
            "0.1.0",
            &source,
            &templates_dir,
        )
        .unwrap();

        // Clean adoption counts as installed
        assert_eq!(report.installed, 1);
        assert!(report.skipped_dirty.is_empty());
    }

    #[test]
    fn test_adoption_dirty() {
        let dir = setup_dir();
        let templates_dir = dir.path().join("templates");
        std::fs::create_dir_all(&templates_dir).unwrap();

        // File on disk differs from source
        std::fs::write(templates_dir.join("review.md"), b"# My custom version").unwrap();

        let mut manifest = RepoManifest::new();
        let source = vec![("review.md", b"# Source version" as &[u8])];

        let report = sync_plugin_templates(
            &mut manifest,
            "test/plugin",
            ".",
            "0.1.0",
            &source,
            &templates_dir,
        )
        .unwrap();

        assert_eq!(report.installed, 0);
        assert_eq!(report.skipped_dirty, vec!["review.md"]);

        // File preserved
        let on_disk = std::fs::read_to_string(templates_dir.join("review.md")).unwrap();
        assert_eq!(on_disk, "# My custom version");
    }

    #[test]
    fn test_parse_frontmatter_version() {
        assert_eq!(
            parse_frontmatter_version(b"---\nversion: 1.0.0\ntype: review\n---\n# Hi"),
            Some("1.0.0".to_string())
        );
        assert_eq!(
            parse_frontmatter_version(b"---\nversion: \"2.5.0\"\n---\n# Hi"),
            Some("2.5.0".to_string())
        );
        assert_eq!(parse_frontmatter_version(b"# No frontmatter"), None);
        assert_eq!(
            parse_frontmatter_version(b"---\ntype: review\n---\n# No version"),
            None
        );
    }

    #[test]
    fn test_ensure_manifest_gitignored_creates_file() {
        let dir = setup_dir();
        ensure_manifest_gitignored(dir.path(), true).unwrap();
        let content = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(content.contains(".aiki/.manifest.json"));
    }

    #[test]
    fn test_ensure_manifest_gitignored_appends() {
        let dir = setup_dir();
        std::fs::write(dir.path().join(".gitignore"), "node_modules/\n").unwrap();
        ensure_manifest_gitignored(dir.path(), true).unwrap();
        let content = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(content.contains("node_modules/"));
        assert!(content.contains(".aiki/.manifest.json"));
    }

    #[test]
    fn test_ensure_manifest_gitignored_already_present() {
        let dir = setup_dir();
        std::fs::write(
            dir.path().join(".gitignore"),
            "node_modules/\n.aiki/.manifest.json\n",
        )
        .unwrap();
        ensure_manifest_gitignored(dir.path(), true).unwrap();
        let content = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        // Should not duplicate
        assert_eq!(content.matches(".aiki/.manifest.json").count(), 1);
    }

    #[test]
    fn test_ensure_manifest_gitignored_covered_by_directory_pattern() {
        for pattern in &[".aiki/", ".aiki/*", "/.aiki/", "/.aiki/*"] {
            let dir = setup_dir();
            std::fs::write(
                dir.path().join(".gitignore"),
                format!("node_modules/\n{}\n", pattern),
            )
            .unwrap();
            ensure_manifest_gitignored(dir.path(), true).unwrap();
            let content = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
            assert!(
                !content.contains(".aiki/.manifest.json"),
                "Should not add .aiki/.manifest.json when {} already covers it",
                pattern
            );
        }
    }

    #[test]
    fn test_install_with_subdirectories() {
        let dir = setup_dir();
        let templates_dir = dir.path().join("templates");
        std::fs::create_dir_all(&templates_dir).unwrap();

        let mut manifest = RepoManifest::new();
        let source = vec![
            ("review/code.md", b"# Code Review" as &[u8]),
            ("review/plan.md", b"# Plan Review" as &[u8]),
        ];

        let report = sync_plugin_templates(
            &mut manifest,
            "test/plugin",
            "aiki",
            "0.1.0",
            &source,
            &templates_dir,
        )
        .unwrap();

        assert_eq!(report.installed, 2);
        assert!(templates_dir.join("aiki/review/code.md").exists());
        assert!(templates_dir.join("aiki/review/plan.md").exists());
    }

    #[test]
    fn test_sync_default_templates() {
        let dir = setup_dir();
        let repo_root = dir.path();
        std::fs::create_dir_all(repo_root.join(".aiki")).unwrap();

        let report = sync_default_templates(repo_root, true).unwrap();

        // Should install all built-in templates
        assert!(report.installed > 0);
        assert_eq!(report.upgraded, 0);
        assert!(report.skipped_dirty.is_empty());

        // Manifest should exist
        assert!(repo_root.join(".aiki/.manifest.json").exists());

        // Templates should exist (at root, not in aiki/ subdir)
        assert!(repo_root.join(".aiki/tasks/plan.md").exists());

        // .gitignore should be updated
        let gitignore = std::fs::read_to_string(repo_root.join(".gitignore")).unwrap();
        assert!(gitignore.contains(".aiki/.manifest.json"));
    }

    #[test]
    fn test_up_to_date_no_changes() {
        let dir = setup_dir();
        let templates_dir = dir.path().join("templates");
        std::fs::create_dir_all(&templates_dir).unwrap();

        let content = b"# Template";

        // Write file and create matching manifest
        std::fs::write(templates_dir.join("review.md"), content).unwrap();

        let mut manifest = RepoManifest::new();
        let plugin = manifest.get_or_create_plugin("test/plugin", "0.1.0", ".");
        plugin.files.insert(
            "review.md".to_string(),
            FileEntry {
                checksum: checksum(content),
                version: None,
                installed_at: "2026-01-01T00:00:00Z".to_string(),
            },
        );

        let source = vec![("review.md", content as &[u8])];
        let report = sync_plugin_templates(
            &mut manifest,
            "test/plugin",
            ".",
            "0.1.0",
            &source,
            &templates_dir,
        )
        .unwrap();

        assert_eq!(report.installed, 0);
        assert_eq!(report.upgraded, 0);
        assert!(report.skipped_dirty.is_empty());
        assert!(report.pruned.is_empty());
    }
}
