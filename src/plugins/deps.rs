//! Transitive dependency resolution for plugins.
//!
//! Dependencies are auto-derived by scanning plugin contents — no manifest needed.

use std::collections::HashSet;
use std::path::Path;

use super::git::clone_plugin;
use super::scanner::derive_plugin_refs;
use super::{check_install_status, InstallStatus, PluginRef};
use crate::error::Result;

/// Report of what happened during an install operation.
#[derive(Debug, Default)]
pub struct InstallReport {
    /// Plugins that were newly cloned.
    pub installed: Vec<PluginRef>,
    /// Plugins that were already installed and skipped.
    pub already_installed: Vec<PluginRef>,
    /// Plugins that failed to install, with error messages.
    pub failed: Vec<(PluginRef, String)>,
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

/// Resolve dependencies for multiple root plugins at once, deduplicating across all roots.
pub fn resolve_all_deps(roots: &[PluginRef], plugins_base: &Path) -> Vec<PluginRef> {
    let mut visited: HashSet<PluginRef> = roots.iter().cloned().collect();
    let mut deps = Vec::new();

    for root in roots {
        resolve_deps_recursive(root, plugins_base, &mut visited, &mut deps);
    }

    deps
}

/// Install a plugin and all its transitive dependencies.
///
/// No rollback on failure — partially installed plugins remain on disk.
/// Re-running retries failed plugins.
pub fn install_with_deps(plugin: &PluginRef, plugins_base: &Path) -> Result<InstallReport> {
    let mut report = InstallReport::default();

    // Install the root plugin
    install_single(plugin, plugins_base, &mut report)?;

    // Resolve and install dependencies (iteratively since new installs may reveal new deps)
    let mut to_process = vec![plugin.clone()];
    let mut visited: HashSet<PluginRef> = HashSet::new();
    visited.insert(plugin.clone());

    while let Some(current) = to_process.pop() {
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

            install_single(&dep, plugins_base, &mut report)?;
            to_process.push(dep);
        }
    }

    Ok(report)
}

/// Install a single plugin, updating the report.
fn install_single(
    plugin: &PluginRef,
    plugins_base: &Path,
    report: &mut InstallReport,
) -> Result<()> {
    match check_install_status(plugin, plugins_base) {
        InstallStatus::Installed => {
            report.already_installed.push(plugin.clone());
        }
        _ => match clone_plugin(plugin, plugins_base) {
            Ok(()) => {
                report.installed.push(plugin.clone());
            }
            Err(e) => {
                report.failed.push((plugin.clone(), e.to_string()));
            }
        },
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
        create_fake_plugin(
            tmp.path(),
            "aiki",
            "a",
            &["aiki/b/tmpl", "aiki/c/tmpl"],
        );
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
        assert_eq!(
            dep_names.iter().filter(|n| *n == "aiki/d").count(),
            1
        );
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
    fn test_resolve_all_deps() {
        let tmp = TempDir::new().unwrap();
        create_fake_plugin(tmp.path(), "aiki", "x", &["aiki/shared/tmpl"]);
        create_fake_plugin(tmp.path(), "aiki", "y", &["aiki/shared/tmpl"]);
        create_fake_plugin(tmp.path(), "aiki", "shared", &[]);

        let roots: Vec<PluginRef> = vec!["aiki/x".parse().unwrap(), "aiki/y".parse().unwrap()];
        let deps = resolve_all_deps(&roots, tmp.path());

        // shared should appear exactly once
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].to_string(), "aiki/shared");
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
}
