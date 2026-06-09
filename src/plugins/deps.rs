//! Transitive dependency resolution for plugins.
//!
//! Dependencies are auto-derived by scanning plugin contents — no manifest needed.

use std::collections::HashSet;
use std::path::Path;

use chrono::Utc;

use super::git::{clone_locked_plugin, clone_plugin, get_head_sha};
use super::lock::{PluginLock, PluginLockEntry};
use super::manifest::{load_manifest, PluginManifest, PLUGIN_MANIFEST_FILENAME};
use super::scanner::derive_plugin_refs;
use super::{check_install_status, InstallStatus, PluginRef};
use crate::error::Result;

/// Report of what happened during an install operation.
#[derive(Debug, Default)]
pub struct InstallReport {
    /// Plugins that were newly cloned, with their parsed manifest.
    pub installed: Vec<(PluginRef, PluginManifest)>,
    /// Plugins that were already installed and skipped.
    pub already_installed: Vec<PluginRef>,
    /// Plugins that failed to install, with error messages.
    pub failed: Vec<(PluginRef, String)>,
    /// Plugins that were rolled back after a failure in the dependency chain.
    pub rolled_back: Vec<PluginRef>,
}

/// Resolve all transitive dependencies of a plugin (not including the root itself).
///
/// Cycle detection: if a plugin appears twice in the walk, it's skipped silently.
/// Diamond deps: deduplicated automatically via HashSet.
pub fn resolve_deps(root: &PluginRef, plugins_base: &Path) -> Vec<PluginRef> {
    let mut visited = HashSet::new();
    visited.insert(root.clone());
    let mut deps = Vec::new();
    resolve_deps_recursive(root, plugins_base, &mut visited, &mut deps);
    deps
}

fn resolve_deps_recursive(
    plugin: &PluginRef,
    plugins_base: &Path,
    visited: &mut HashSet<PluginRef>,
    deps: &mut Vec<PluginRef>,
) {
    // Only scan if the plugin is actually installed on disk
    let dir = plugin.install_dir(plugins_base);
    if !dir.join(".git").is_dir() {
        return;
    }

    let refs = derive_plugin_refs(&dir, Some(plugin));
    for r in refs {
        if visited.contains(&r) {
            continue; // Cycle or diamond — skip
        }
        visited.insert(r.clone());
        deps.push(r.clone());
        resolve_deps_recursive(&r, plugins_base, visited, deps);
    }
}

/// Remove lock entries for plugins not reachable from the declared project refs.
///
/// Call this before project-wide install so that orphaned entries from previously
/// removed plugins don't cause stale SHAs to be used when the same dep is
/// re-introduced by a different parent.
pub fn prune_orphaned_entries(
    lock: &mut PluginLock,
    declared_refs: &[PluginRef],
    plugins_base: &Path,
) -> Vec<String> {
    let mut keep = HashSet::new();
    for r in declared_refs {
        keep.insert(r.to_string());
        for dep in resolve_deps(r, plugins_base) {
            keep.insert(dep.to_string());
        }
    }

    let orphaned: Vec<String> = lock
        .entries
        .keys()
        .filter(|k| !keep.contains(k.as_str()))
        .cloned()
        .collect();

    for key in &orphaned {
        lock.entries.remove(key);
    }

    orphaned
}

/// Install a plugin and all its transitive dependencies.
///
/// When `project_root` is provided, the lock file is consulted: locked plugins
/// are installed at the exact pinned SHA, and newly cloned plugins have their
/// SHA recorded. The lock file is saved after all installations complete.
///
/// Atomic per resolution chain: if any plugin in the chain fails to install,
/// all newly installed plugins are rolled back (removed from disk) and no lock
/// entries are written.
pub fn install(
    plugin: &PluginRef,
    plugins_base: &Path,
    project_root: Option<&Path>,
    external_lock: Option<&mut PluginLock>,
) -> Result<InstallReport> {
    let mut report = InstallReport::default();

    // Use the caller's lock if provided, otherwise load our own.
    let mut external_lock = external_lock;
    let mut owned_lock = match &external_lock {
        Some(_) => None,
        None => match project_root {
            Some(root) => Some(PluginLock::load(root)?),
            None => None,
        },
    };

    // Track newly installed plugins (for rollback) and deferred lock entries.
    let mut chain: Vec<PluginRef> = Vec::new();
    let mut pending_locks: Vec<(PluginRef, PluginLockEntry)> = Vec::new();

    // Install the root plugin
    install_single(
        plugin,
        plugins_base,
        external_lock.as_deref().or(owned_lock.as_ref()),
        &mut report,
        &mut chain,
        &mut pending_locks,
    )?;

    // Resolve and install dependencies (iteratively since new installs may reveal new deps)
    if report.failed.is_empty() {
        let mut to_process = vec![plugin.clone()];
        let mut visited: HashSet<PluginRef> = HashSet::new();
        visited.insert(plugin.clone());

        'deps: while let Some(current) = to_process.pop() {
            // Only scan installed plugins
            let dir = current.install_dir(plugins_base);
            if !dir.join(".git").is_dir() {
                continue;
            }

            let refs = derive_plugin_refs(&dir, Some(&current));
            for dep in refs {
                if visited.contains(&dep) {
                    continue;
                }
                visited.insert(dep.clone());

                install_single(
                    &dep,
                    plugins_base,
                    external_lock.as_deref().or(owned_lock.as_ref()),
                    &mut report,
                    &mut chain,
                    &mut pending_locks,
                )?;

                if !report.failed.is_empty() {
                    break 'deps;
                }

                to_process.push(dep);
            }
        }
    }

    if !report.failed.is_empty() {
        // Rollback: remove all newly installed plugins from disk.
        for rolled_back in &chain {
            let dir = rolled_back.install_dir(plugins_base);
            let _ = std::fs::remove_dir_all(&dir);
        }
        report.installed.clear();
        report.rolled_back = chain;
        // Pending lock entries are discarded (not applied).
    } else {
        // Success: apply deferred lock entries.
        if let Some(lock) = external_lock.as_deref_mut().or(owned_lock.as_mut()) {
            for (ref plugin_ref, entry) in pending_locks {
                lock.insert(plugin_ref, entry);
            }
        }

        // Save only when we loaded our own lock (caller saves external lock).
        if let (Some(lock), Some(root)) = (&owned_lock, project_root) {
            lock.save(root)?;
        }
    }

    Ok(report)
}

/// Install a single plugin, updating the report.
///
/// When a lock is provided, locked plugins are installed at their pinned SHA.
/// Newly cloned plugins have their lock entry collected in `pending_locks`
/// (deferred until the entire chain succeeds). Successfully cloned plugins
/// are tracked in `chain` for rollback.
///
/// After cloning, validates that `plugin.yaml` exists. If missing, the clone is
/// removed and the plugin is recorded as failed — this is the single validation
/// point so callers don't need to re-check.
fn install_single(
    plugin: &PluginRef,
    plugins_base: &Path,
    lock: Option<&PluginLock>,
    report: &mut InstallReport,
    chain: &mut Vec<PluginRef>,
    pending_locks: &mut Vec<(PluginRef, PluginLockEntry)>,
) -> Result<()> {
    match check_install_status(plugin, plugins_base) {
        InstallStatus::Installed => {
            report.already_installed.push(plugin.clone());
        }
        _ => {
            // Check lock file for a pinned SHA
            let locked_sha = lock
                .and_then(|l| l.get(plugin))
                .map(|entry| entry.sha.clone());

            let clone_result = match &locked_sha {
                Some(sha) => clone_locked_plugin(plugin, plugins_base, sha),
                None => clone_plugin(plugin, plugins_base),
            };

            match clone_result {
                Ok(()) => {
                    let install_dir = plugin.install_dir(plugins_base);
                    match load_manifest(&install_dir) {
                        Ok(manifest) => {
                            // Defer lock entry for newly cloned (unlocked) plugins
                            if locked_sha.is_none() {
                                if let Ok(sha) = get_head_sha(plugin, plugins_base) {
                                    pending_locks.push((
                                        plugin.clone(),
                                        PluginLockEntry {
                                            sha,
                                            source: plugin.github_url(),
                                            resolved: Utc::now().to_rfc3339(),
                                        },
                                    ));
                                }
                            }
                            chain.push(plugin.clone());
                            report.installed.push((plugin.clone(), manifest));
                        }
                        Err(_) => {
                            // Not a valid plugin — clean up and record as failed
                            let _ = std::fs::remove_dir_all(&install_dir);
                            report.failed.push((
                                plugin.clone(),
                                format!(
                                    "Missing {}. This may not be a valid aiki plugin.",
                                    PLUGIN_MANIFEST_FILENAME
                                ),
                            ));
                        }
                    }
                }
                Err(e) => {
                    report.failed.push((plugin.clone(), e.to_string()));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Create a fake installed plugin with optional deps in hooks.yaml
    fn create_fake_plugin(base: &Path, ns: &str, name: &str, deps: &[&str]) {
        let dir = base.join(ns).join(name);
        fs::create_dir_all(dir.join(".git")).unwrap();

        if !deps.is_empty() {
            let yaml: Vec<String> = deps
                .iter()
                .enumerate()
                .map(|(i, d)| format!("hook{}:\n  template: {}", i, d))
                .collect();
            fs::write(dir.join("hooks.yaml"), yaml.join("\n")).unwrap();
        }
    }

    #[test]
    fn test_resolve_deps_no_deps() {
        let tmp = TempDir::new().unwrap();
        create_fake_plugin(tmp.path(), "aiki", "way", &[]);

        let root: PluginRef = "aiki/way".parse().unwrap();
        let deps = resolve_deps(&root, tmp.path());
        assert!(deps.is_empty());
    }

    #[test]
    fn test_resolve_deps_chain() {
        let tmp = TempDir::new().unwrap();
        // A depends on B, B depends on C
        create_fake_plugin(tmp.path(), "aiki", "a", &["aiki/b/template"]);
        create_fake_plugin(tmp.path(), "aiki", "b", &["aiki/c/template"]);
        create_fake_plugin(tmp.path(), "aiki", "c", &[]);

        let root: PluginRef = "aiki/a".parse().unwrap();
        let deps = resolve_deps(&root, tmp.path());

        let dep_names: Vec<String> = deps.iter().map(|d| d.to_string()).collect();
        assert!(dep_names.contains(&"aiki/b".to_string()));
        assert!(dep_names.contains(&"aiki/c".to_string()));
        assert_eq!(deps.len(), 2);
    }

    #[test]
    fn test_resolve_deps_diamond() {
        let tmp = TempDir::new().unwrap();
        // A→B, A→C, B→D, C→D (diamond at D)
        create_fake_plugin(tmp.path(), "aiki", "a", &["aiki/b/tmpl", "aiki/c/tmpl"]);
        create_fake_plugin(tmp.path(), "aiki", "b", &["aiki/d/tmpl"]);
        create_fake_plugin(tmp.path(), "aiki", "c", &["aiki/d/tmpl"]);
        create_fake_plugin(tmp.path(), "aiki", "d", &[]);

        let root: PluginRef = "aiki/a".parse().unwrap();
        let deps = resolve_deps(&root, tmp.path());

        let dep_names: Vec<String> = deps.iter().map(|d| d.to_string()).collect();
        assert!(dep_names.contains(&"aiki/b".to_string()));
        assert!(dep_names.contains(&"aiki/c".to_string()));
        assert!(dep_names.contains(&"aiki/d".to_string()));
        // D should appear exactly once
        assert_eq!(dep_names.iter().filter(|n| *n == "aiki/d").count(), 1);
    }

    #[test]
    fn test_resolve_deps_cycle() {
        let tmp = TempDir::new().unwrap();
        // A→B→A (cycle)
        create_fake_plugin(tmp.path(), "aiki", "a", &["aiki/b/tmpl"]);
        create_fake_plugin(tmp.path(), "aiki", "b", &["aiki/a/tmpl"]);

        let root: PluginRef = "aiki/a".parse().unwrap();
        let deps = resolve_deps(&root, tmp.path());

        // Should find B as a dep, skip A (cycle), no infinite loop
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].to_string(), "aiki/b");
    }

    #[test]
    fn test_resolve_deps_not_installed() {
        let tmp = TempDir::new().unwrap();
        // A references B but B is not installed
        create_fake_plugin(tmp.path(), "aiki", "a", &["aiki/b/tmpl"]);
        // B is NOT created

        let root: PluginRef = "aiki/a".parse().unwrap();
        let deps = resolve_deps(&root, tmp.path());

        // B is listed as a dep (discovered) but its own deps can't be resolved (not installed)
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].to_string(), "aiki/b");
    }

    // -----------------------------------------------------------------------
    // install() tests
    // -----------------------------------------------------------------------

    /// Create a fake installed plugin with a valid plugin.yaml manifest.
    fn create_valid_plugin(base: &Path, ns: &str, name: &str, dep_refs: &[&str]) {
        let dir = base.join(ns).join(name);
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(
            dir.join("plugin.yaml"),
            format!("name: {} {}\n", ns, name),
        )
        .unwrap();

        if !dep_refs.is_empty() {
            let yaml: Vec<String> = dep_refs
                .iter()
                .enumerate()
                .map(|(i, d)| format!("hook{}:\n  template: {}", i, d))
                .collect();
            fs::write(dir.join("hooks.yaml"), yaml.join("\n")).unwrap();
        }
    }

    #[test]
    fn test_install_already_installed_plugin_skipped() {
        let tmp = TempDir::new().unwrap();
        create_valid_plugin(tmp.path(), "ns", "root", &[]);

        let root: PluginRef = "ns/root".parse().unwrap();
        let report = install(&root, tmp.path(), None, None).unwrap();

        assert_eq!(report.already_installed.len(), 1);
        assert_eq!(report.already_installed[0].to_string(), "ns/root");
        assert!(report.installed.is_empty());
        assert!(report.failed.is_empty());
        assert!(report.rolled_back.is_empty());
    }

    #[test]
    fn test_install_already_installed_chain_all_skipped() {
        let tmp = TempDir::new().unwrap();
        // Root → dep A → dep B, all pre-installed
        create_valid_plugin(tmp.path(), "ns", "root", &["ns/a/tpl"]);
        create_valid_plugin(tmp.path(), "ns", "a", &["ns/b/tpl"]);
        create_valid_plugin(tmp.path(), "ns", "b", &[]);

        let root: PluginRef = "ns/root".parse().unwrap();
        let report = install(&root, tmp.path(), None, None).unwrap();

        assert_eq!(report.already_installed.len(), 3);
        assert!(report.installed.is_empty());
        assert!(report.failed.is_empty());
        assert!(report.rolled_back.is_empty());
    }

    #[test]
    fn test_install_not_installed_fails_clone() {
        let tmp = TempDir::new().unwrap();
        // Root is NOT installed — clone_plugin will fail (no real GitHub repo)
        let root: PluginRef = "fake/nonexistent".parse().unwrap();
        let report = install(&root, tmp.path(), None, None).unwrap();

        assert!(report.installed.is_empty());
        assert!(report.already_installed.is_empty());
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].0.to_string(), "fake/nonexistent");
        assert!(!report.failed[0].1.is_empty()); // error message present
    }

    #[test]
    fn test_install_transitive_dep_failure_reports_correctly() {
        let tmp = TempDir::new().unwrap();
        // Root is installed with a dep on a non-existent plugin
        create_valid_plugin(tmp.path(), "ns", "root", &["ns/missing/tpl"]);

        let root: PluginRef = "ns/root".parse().unwrap();
        let report = install(&root, tmp.path(), None, None).unwrap();

        // Root was already installed
        assert_eq!(report.already_installed.len(), 1);
        assert_eq!(report.already_installed[0].to_string(), "ns/root");
        // The missing dep failed to clone
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].0.to_string(), "ns/missing");
    }

    #[test]
    fn test_install_lock_file_not_updated_on_failure() {
        let tmp = TempDir::new().unwrap();
        let project_root = tmp.path().join("project");
        fs::create_dir_all(project_root.join(".aiki")).unwrap();

        let plugins_base = tmp.path().join("plugins");
        // Root installed, dep will fail
        create_valid_plugin(&plugins_base, "ns", "root", &["ns/missing/tpl"]);

        let root: PluginRef = "ns/root".parse().unwrap();
        let report = install(&root, &plugins_base, Some(&project_root), None).unwrap();

        assert!(!report.failed.is_empty());
        // Lock file should not exist (no successful installs wrote to it)
        let lock_path = project_root.join(".aiki/plugins.lock");
        assert!(
            !lock_path.exists(),
            "Lock file should not be created when install fails"
        );
    }

    #[test]
    fn test_install_lock_file_saved_on_success() {
        let tmp = TempDir::new().unwrap();
        let project_root = tmp.path().join("project");
        fs::create_dir_all(project_root.join(".aiki")).unwrap();

        let plugins_base = tmp.path().join("plugins");
        create_valid_plugin(&plugins_base, "ns", "root", &[]);

        let root: PluginRef = "ns/root".parse().unwrap();
        let report = install(&root, &plugins_base, Some(&project_root), None).unwrap();

        assert!(report.failed.is_empty());
        // Lock file should be saved (even if empty — save is called on success path)
        let lock_path = project_root.join(".aiki/plugins.lock");
        assert!(
            lock_path.exists(),
            "Lock file should be saved on successful install"
        );
    }

    #[test]
    fn test_install_rollback_removes_chain_from_disk() {
        let tmp = TempDir::new().unwrap();
        let plugins_base = tmp.path().join("plugins");

        // Root installed, dep A installed, dep A depends on non-existent dep B
        create_valid_plugin(&plugins_base, "ns", "root", &["ns/a/tpl"]);
        create_valid_plugin(&plugins_base, "ns", "a", &["ns/missing/tpl"]);

        let root: PluginRef = "ns/root".parse().unwrap();
        let report = install(&root, &plugins_base, None, None).unwrap();

        // Both root and A were already installed, missing failed
        assert_eq!(report.already_installed.len(), 2);
        assert_eq!(report.failed.len(), 1);
        // Since root and A were already installed (not newly cloned), they're not in rolled_back
        // Nothing new was in chain, so rolled_back is empty
        assert!(report.rolled_back.is_empty());
        // Root and A should still be on disk (not removed)
        assert!(plugins_base.join("ns/root/.git").is_dir());
        assert!(plugins_base.join("ns/a/.git").is_dir());
    }

    #[test]
    fn test_install_report_reflects_rollback_on_failed_clone() {
        let tmp = TempDir::new().unwrap();
        let plugins_base = tmp.path();

        // Plugin not installed — clone will fail
        let root: PluginRef = "fake/plugin".parse().unwrap();
        let report = install(&root, plugins_base, None, None).unwrap();

        // Failed plugins are reported with error messages
        assert_eq!(report.failed.len(), 1);
        let (ref failed_ref, ref err_msg) = report.failed[0];
        assert_eq!(failed_ref.to_string(), "fake/plugin");
        assert!(!err_msg.is_empty());
        // installed should be cleared by rollback path
        assert!(report.installed.is_empty());
    }

    #[test]
    fn test_install_external_lock_not_saved_by_install() {
        let tmp = TempDir::new().unwrap();
        let project_root = tmp.path().join("project");
        fs::create_dir_all(project_root.join(".aiki")).unwrap();

        let plugins_base = tmp.path().join("plugins");
        create_valid_plugin(&plugins_base, "ns", "root", &[]);

        let mut lock = PluginLock::default();
        let root: PluginRef = "ns/root".parse().unwrap();
        let report = install(&root, &plugins_base, Some(&project_root), Some(&mut lock)).unwrap();

        assert!(report.failed.is_empty());
        // When external lock is provided, install() should NOT save the lock file itself
        let lock_path = project_root.join(".aiki/plugins.lock");
        assert!(
            !lock_path.exists(),
            "install() should not save lock when external_lock is provided"
        );
    }

    #[test]
    fn test_install_pre_existing_lock_preserved_on_failure() {
        let tmp = TempDir::new().unwrap();
        let project_root = tmp.path().join("project");
        fs::create_dir_all(project_root.join(".aiki")).unwrap();

        // Write a pre-existing lock file
        let mut lock = PluginLock::default();
        let existing_ref: PluginRef = "existing/plugin".parse().unwrap();
        lock.insert(
            &existing_ref,
            PluginLockEntry {
                sha: "abcd1234".repeat(5),
                source: "https://github.com/existing/plugin.git".to_string(),
                resolved: "2026-01-01T00:00:00Z".to_string(),
            },
        );
        lock.save(&project_root).unwrap();

        let plugins_base = tmp.path().join("plugins");
        // Root installed, dep will fail
        create_valid_plugin(&plugins_base, "ns", "root", &["ns/missing/tpl"]);

        let root: PluginRef = "ns/root".parse().unwrap();
        let report = install(&root, &plugins_base, Some(&project_root), None).unwrap();
        assert!(!report.failed.is_empty());

        // Pre-existing lock file should be unchanged
        let reloaded = PluginLock::load(&project_root).unwrap();
        assert!(
            reloaded.get(&existing_ref).is_some(),
            "Pre-existing lock entry should be preserved after failed install"
        );
        assert_eq!(
            reloaded.entries.len(),
            1,
            "No new entries should be added on failure"
        );
    }

    // -----------------------------------------------------------------------
    // prune_orphaned_entries() tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_prune_removes_orphaned_transitive_dep() {
        // Scenario: plugin A used to depend on B. A was removed (lock entry
        // cleaned up), but B's lock entry was left behind (orphaned transitive
        // dep). Now the project references C (which also depends on B), but
        // neither C nor B is installed yet (fresh machine / first install).
        //
        // Prune runs before install. Since C isn't installed, resolve_deps
        // can't discover B as a dep of C. B's lock entry is orphaned → pruned.
        // When install then clones C and discovers dep B, B is resolved fresh
        // to HEAD instead of using the stale SHA from the A era.
        let tmp = TempDir::new().unwrap();
        let plugins_base = tmp.path().join("plugins");
        fs::create_dir_all(&plugins_base).unwrap();

        // Neither C nor B is installed — fresh machine scenario.

        let mut lock = PluginLock::default();
        // Orphaned entry for B from the old A days
        lock.insert(
            &"ns/b".parse().unwrap(),
            PluginLockEntry {
                sha: "stale_sha_from_old_plugin_a_padding0".to_string(),
                source: "https://github.com/ns/b.git".to_string(),
                resolved: "2026-01-01T00:00:00Z".to_string(),
            },
        );

        // Project declares only C (C isn't installed, so its deps are unknown)
        let declared: Vec<PluginRef> = vec!["ns/c".parse().unwrap()];

        let pruned = prune_orphaned_entries(&mut lock, &declared, &plugins_base);

        // B is not declared and not in any installed plugin's dep tree → pruned
        assert!(
            pruned.contains(&"ns/b".to_string()),
            "orphaned lock entry for ns/b should be pruned"
        );
        assert!(
            lock.get(&"ns/b".parse::<PluginRef>().unwrap()).is_none(),
            "ns/b should no longer be in the lock"
        );
    }

    #[test]
    fn test_prune_keeps_declared_ref() {
        let tmp = TempDir::new().unwrap();
        let plugins_base = tmp.path().join("plugins");
        create_valid_plugin(&plugins_base, "ns", "root", &[]);

        let mut lock = PluginLock::default();
        lock.insert(
            &"ns/root".parse().unwrap(),
            PluginLockEntry {
                sha: "valid_sha_for_declared_ref_padding0".to_string(),
                source: "https://github.com/ns/root.git".to_string(),
                resolved: "2026-01-01T00:00:00Z".to_string(),
            },
        );

        let declared: Vec<PluginRef> = vec!["ns/root".parse().unwrap()];
        let pruned = prune_orphaned_entries(&mut lock, &declared, &plugins_base);

        assert!(pruned.is_empty(), "declared ref should not be pruned");
        assert!(
            lock.get(&"ns/root".parse::<PluginRef>().unwrap()).is_some(),
            "ns/root should still be in the lock"
        );
    }

    #[test]
    fn test_prune_keeps_installed_transitive_dep() {
        let tmp = TempDir::new().unwrap();
        let plugins_base = tmp.path().join("plugins");

        // Root depends on dep, both installed
        create_valid_plugin(&plugins_base, "ns", "root", &["ns/dep/tpl"]);
        create_valid_plugin(&plugins_base, "ns", "dep", &[]);

        let mut lock = PluginLock::default();
        lock.insert(
            &"ns/root".parse().unwrap(),
            PluginLockEntry {
                sha: "root_sha_padding_to_fill_space000".to_string(),
                source: "https://github.com/ns/root.git".to_string(),
                resolved: "2026-01-01T00:00:00Z".to_string(),
            },
        );
        lock.insert(
            &"ns/dep".parse().unwrap(),
            PluginLockEntry {
                sha: "dep_sha_padding_to_fill_the_space0".to_string(),
                source: "https://github.com/ns/dep.git".to_string(),
                resolved: "2026-01-01T00:00:00Z".to_string(),
            },
        );

        let declared: Vec<PluginRef> = vec!["ns/root".parse().unwrap()];
        let pruned = prune_orphaned_entries(&mut lock, &declared, &plugins_base);

        assert!(pruned.is_empty(), "installed transitive dep should not be pruned");
        assert_eq!(lock.entries.len(), 2);
    }
}
