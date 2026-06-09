//! `aiki plugin` CLI subcommand — install, update, list, and remove plugins.

use clap::Subcommand;
use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::Path;

use chrono::Utc;

use crate::error::{AikiError, Result};
use crate::plugins::deps::{prune_orphaned_entries, resolve_deps, InstallReport};
use crate::plugins::install;
use crate::plugins::graph::PluginGraph;
use crate::plugins::git::{clone_locked_plugin, get_head_sha, remove_plugin, resolve_remote_head};
use crate::plugins::lock::{PluginLock, PluginLockEntry};
use crate::plugins::scanner::derive_plugin_refs;
use crate::plugins::{
    check_install_status, list_installed_plugins, plugins_base_dir, InstallStatus, PluginRef,
};
use crate::tasks::templates::TASKS_DIR_NAME;

#[derive(Subcommand)]
#[command(disable_help_subcommand = true)]
pub enum PluginCommands {
    /// Install a plugin from GitHub (and its dependencies)
    Install {
        /// Plugin reference (e.g., aiki/way, somecorp/security). Omit to install from project.
        reference: Option<String>,
    },
    /// Update an installed plugin (and reconcile dependencies)
    Update {
        /// Plugin reference to update. Omit to update all installed.
        reference: Option<String>,
    },
    /// List installed plugins
    List {
        /// Limit the number of results shown
        #[arg(long, short = 'n')]
        number: Option<usize>,
    },
    /// Remove an installed plugin
    Remove {
        /// Plugin reference to remove (e.g., aiki/way)
        reference: String,
        /// Remove even if other plugins depend on this one
        #[arg(long)]
        force: bool,
    },
}

pub fn run(command: PluginCommands) -> Result<()> {
    match command {
        PluginCommands::Install { reference } => run_install(reference),
        PluginCommands::Update { reference } => run_update(reference),
        PluginCommands::List { number } => run_list(number),
        PluginCommands::Remove { reference, force } => run_remove(reference, force),
    }
}

fn run_install(reference: Option<String>) -> Result<()> {
    let plugins_base = plugins_base_dir()?;
    let cwd = env::current_dir()?;
    let project_root = find_project_root(&cwd);

    match reference {
        Some(ref_str) => {
            let plugin: PluginRef = ref_str.parse()?;
            // Clear any fetch-failure marker so the install is attempted fresh.
            crate::plugins::clear_fetch_failed(&plugin, &plugins_base);
            let report = install(&plugin, &plugins_base, project_root.as_deref(), None)?;
            print_install_report(&report, &plugin);

            if !report.failed.is_empty() {
                Err(anyhow::anyhow!("Some plugins failed to install"))?;
            }
        }
        None => {
            // Scan current project for plugin references
            let aiki_dir = find_aiki_dir(&cwd)?;

            let refs = derive_plugin_refs(&aiki_dir, None);
            if refs.is_empty() {
                println!("No plugin references found in project.");
                return Ok(());
            }

            // Prune orphaned lock entries before resolution so stale SHAs
            // from previously removed plugins don't get reused.
            if let Some(ref root) = project_root {
                if let Ok(mut lock) = PluginLock::load(root) {
                    let pruned = prune_orphaned_entries(&mut lock, &refs, &plugins_base);
                    if !pruned.is_empty() {
                        lock.save(root)?;
                    }
                }
            }

            // Clear all markers — explicit install-all should retry everything.
            crate::plugins::clear_all_fetch_failed(&plugins_base);

            let mut any_failed = false;
            for plugin in &refs {
                let report = install(plugin, &plugins_base, project_root.as_deref(), None)?;
                print_install_report(&report, plugin);
                if !report.failed.is_empty() {
                    any_failed = true;
                }
            }

            if any_failed {
                Err(anyhow::anyhow!("Some plugins failed to install"))?;
            }
        }
    }

    Ok(())
}

fn run_update(reference: Option<String>) -> Result<()> {
    let plugins_base = plugins_base_dir()?;
    let cwd = env::current_dir()?;
    let project_root = find_project_root(&cwd);

    match reference {
        Some(ref_str) => {
            let plugin: PluginRef = ref_str.parse()?;
            crate::plugins::clear_fetch_failed(&plugin, &plugins_base);
            update_single(&plugin, &plugins_base, project_root.as_deref())?;
        }
        None => {
            // Update all installed plugins
            crate::plugins::clear_all_fetch_failed(&plugins_base);
            let installed = list_installed_plugins(&plugins_base);
            if installed.is_empty() {
                println!("No plugins installed.");
                return Ok(());
            }

            let mut any_failed = false;
            for plugin in &installed {
                if let Err(e) = update_single(plugin, &plugins_base, project_root.as_deref()) {
                    eprintln!("Error: failed to update {} — {}", plugin, e);
                    any_failed = true;
                }
            }

            if any_failed {
                Err(anyhow::anyhow!("Some plugins failed to update"))?;
            }
        }
    }

    Ok(())
}

fn update_single(plugin: &PluginRef, plugins_base: &Path, project_root: Option<&Path>) -> Result<()> {
    let mut lock = match project_root {
        Some(root) => PluginLock::load(root)?,
        None => PluginLock::default(),
    };

    let mut summary: Vec<String> = Vec::new();
    let mut failures: Vec<String> = Vec::new();

    // 1. Resolve remote HEAD and update the root plugin
    update_plugin_via_lock(plugin, plugins_base, &mut lock, &mut summary, &mut failures)?;

    // 2. Resolve deps after updating root, then update existing deps
    let deps = resolve_deps(plugin, plugins_base);
    let mut updated: HashSet<PluginRef> = HashSet::new();
    updated.insert(plugin.clone());

    for dep in &deps {
        if updated.contains(dep) {
            continue;
        }
        match check_install_status(dep, plugins_base) {
            InstallStatus::Installed => {
                updated.insert(dep.clone());
                if let Err(e) = update_plugin_via_lock(dep, plugins_base, &mut lock, &mut summary, &mut failures) {
                    eprintln!("Error: failed to update dependency {}: {}", dep, e);
                }
            }
            _ => {
                // New dep — install with its own deps, sharing the parent lock
                let report = install(dep, plugins_base, project_root, Some(&mut lock))?;
                for (installed, _manifest) in &report.installed {
                    summary.push(format!("{}  (new dependency)", installed));
                    println!("Installed (dependency): {}", installed.display_name(plugins_base));
                }
                for (failed, err) in &report.failed {
                    eprintln!("Error: failed to install {}: {}", failed, err);
                    failures.push(format!("{}: {}", failed, err));
                }
            }
        }
    }

    // 3. Re-resolve deps after updates — updated deps may now reference new plugins
    let deps_after = resolve_deps(plugin, plugins_base);
    for dep in &deps_after {
        if updated.contains(dep) {
            continue;
        }
        match check_install_status(dep, plugins_base) {
            InstallStatus::Installed => {
                updated.insert(dep.clone());
                if let Err(e) = update_plugin_via_lock(dep, plugins_base, &mut lock, &mut summary, &mut failures) {
                    eprintln!("Error: failed to update dependency {}: {}", dep, e);
                }
            }
            _ => {
                let report = install(dep, plugins_base, project_root, Some(&mut lock))?;
                for (installed, _manifest) in &report.installed {
                    summary.push(format!("{}  (new dependency)", installed));
                    println!("Installed (dependency): {}", installed.display_name(plugins_base));
                }
                for (failed, err) in &report.failed {
                    eprintln!("Error: failed to install {}: {}", failed, err);
                    failures.push(format!("{}: {}", failed, err));
                }
            }
        }
    }

    // 4. Save lock file
    if let Some(root) = project_root {
        lock.save(root)?;
    }

    // 5. Print summary
    for line in &summary {
        println!("{}", line);
    }

    if !failures.is_empty() {
        return Err(AikiError::PluginOperationFailed {
            plugin: plugin.to_string(),
            details: format!(
                "{} dependency failure(s): {}",
                failures.len(),
                failures.join("; ")
            ),
        });
    }

    Ok(())
}

/// Resolve remote HEAD for a plugin, compare with locked SHA, and reinstall if changed.
fn update_plugin_via_lock(
    plugin: &PluginRef,
    plugins_base: &Path,
    lock: &mut PluginLock,
    summary: &mut Vec<String>,
    failures: &mut Vec<String>,
) -> Result<()> {
    let remote_sha = resolve_remote_head(plugin)?;

    let old_sha = lock
        .get(plugin)
        .map(|e| e.sha.clone())
        .or_else(|| get_head_sha(plugin, plugins_base).ok());

    if old_sha.as_deref() == Some(&remote_sha) {
        summary.push(format!("{}  (unchanged)", plugin));
        return Ok(());
    }

    // Clone-then-swap: clone new version to a temp dir first, so a failed clone
    // doesn't leave the existing install deleted.
    let install_dir = plugin.install_dir(plugins_base);
    let temp_base = plugins_base.join(format!(".updating-{}", plugin.name));

    // Clean up any stale temp dir from a previous interrupted update
    if temp_base.exists() {
        let _ = std::fs::remove_dir_all(&temp_base);
    }

    match clone_locked_plugin(plugin, &temp_base, &remote_sha) {
        Ok(()) => {
            let temp_install = plugin.install_dir(&temp_base);

            // Remove the old install dir, then rename the temp one into place
            if install_dir.exists() {
                std::fs::remove_dir_all(&install_dir).map_err(|e| {
                    let _ = std::fs::remove_dir_all(&temp_base);
                    AikiError::PluginOperationFailed {
                        plugin: plugin.to_string(),
                        details: format!("Failed to remove old install: {}", e),
                    }
                })?;
            }
            // Ensure parent dir exists (in case install_dir was never created)
            if let Some(parent) = install_dir.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::rename(&temp_install, &install_dir).map_err(|e| {
                AikiError::PluginOperationFailed {
                    plugin: plugin.to_string(),
                    details: format!("Failed to move new install into place: {}", e),
                }
            })?;
            // Clean up the temp base directory structure
            let _ = std::fs::remove_dir_all(&temp_base);

            let old_display = old_sha
                .as_deref()
                .map(|s| &s[..s.len().min(12)])
                .unwrap_or("(new)");
            let new_display = &remote_sha[..remote_sha.len().min(12)];
            summary.push(format!("{}  {} → {}", plugin, old_display, new_display));

            lock.insert(
                plugin,
                PluginLockEntry {
                    sha: remote_sha,
                    source: plugin.github_url(),
                    resolved: Utc::now().to_rfc3339(),
                },
            );
        }
        Err(e) => {
            // Clone failed — clean up temp dir, leave old install untouched
            let _ = std::fs::remove_dir_all(&temp_base);
            failures.push(format!("{}: {}", plugin, e));
        }
    }

    Ok(())
}

fn run_list(number: Option<usize>) -> Result<()> {
    let plugins_base = plugins_base_dir()?;
    let cwd = env::current_dir()?;

    // Check if we're in a project
    let aiki_dir = find_aiki_dir_optional(&cwd);

    match aiki_dir {
        Some(aiki_dir) => {
            // In repo: show project context
            let mut project_refs = derive_plugin_refs(&aiki_dir, None);
            if let Some(n) = number {
                project_refs.truncate(n);
            }

            if project_refs.is_empty() {
                println!("No plugin references found in project.");
            } else {
                let graph = PluginGraph::build(&plugins_base);
                println!("Plugins:");
                for plugin in &project_refs {
                    let status = check_install_status(plugin, &plugins_base);
                    match status {
                        InstallStatus::Installed => {
                            println!("  {}    installed", graph.display_name(plugin));
                            // Show deps
                            for dep in &graph.dependencies(plugin) {
                                let dep_status = check_install_status(dep, &plugins_base);
                                match dep_status {
                                    InstallStatus::Installed => {
                                        println!("    └ {}    installed (dependency)", dep);
                                    }
                                    _ => {
                                        println!("    └ {}    not installed", dep);
                                    }
                                }
                            }
                        }
                        InstallStatus::PartialInstall => {
                            println!(
                                "  {}    partial install    ← aiki plugin install {}",
                                plugin, plugin
                            );
                        }
                        InstallStatus::NotInstalled => {
                            println!(
                                "  {}    not installed    ← aiki plugin install {}",
                                plugin, plugin
                            );
                        }
                    }
                }
            }

            // Check for overrides
            let templates_dir = aiki_dir.join(TASKS_DIR_NAME);
            if templates_dir.is_dir() {
                let overrides = find_overrides(&templates_dir, &project_refs);
                if !overrides.is_empty() {
                    println!();
                    println!("Overrides:");
                    for (ref_path, local_path) in &overrides {
                        println!("  {} → {}", ref_path, local_path);
                    }
                }
            }
        }
        None => {
            // Outside repo: list all installed
            let installed = list_installed_plugins(&plugins_base);
            if installed.is_empty() {
                println!("No plugins installed.");
                return Ok(());
            }

            let graph = PluginGraph::build(&plugins_base);

            // Build (plugin, deps) pairs from the graph
            let mut plugin_deps: Vec<(PluginRef, Vec<PluginRef>)> = installed
                .iter()
                .map(|p| (p.clone(), graph.dependencies(p)))
                .collect();

            // A plugin is a root if nothing depends on it.
            // If no roots exist (all in cycles), show everything as top-level.
            let roots: HashSet<&PluginRef> = installed
                .iter()
                .filter(|p| graph.dependents(p).is_empty())
                .collect();

            let hidden: HashSet<PluginRef> = if roots.is_empty() {
                HashSet::new()
            } else {
                plugin_deps
                    .iter()
                    .filter(|(p, _)| roots.contains(p))
                    .flat_map(|(_, deps)| deps.iter().cloned())
                    .collect()
            };

            if let Some(n) = number {
                plugin_deps.truncate(n);
            }
            println!("Installed ({}):", plugins_base.display());
            for (plugin, deps) in &plugin_deps {
                if hidden.contains(plugin) {
                    continue; // Only show as dependency under its parent
                }
                println!("  {}", graph.display_name(plugin));
                for dep in deps {
                    println!("    └ {}    (dependency)", dep);
                }
            }
        }
    }

    Ok(())
}

fn run_remove(reference: String, force: bool) -> Result<()> {
    let plugins_base = plugins_base_dir()?;
    remove_plugin_from(&reference, force, &plugins_base)?;

    // Remove from hooks.yml include references and lock file in the current project (if in one)
    let plugin: PluginRef = reference.parse()?;
    let cwd = env::current_dir()?;
    if let Some(aiki_dir) = find_aiki_dir_optional(&cwd) {
        remove_from_hooks_yml(&aiki_dir, &plugin);
    }

    if let Some(project_root) = find_project_root(&cwd) {
        if let Ok(mut lock) = PluginLock::load(&project_root) {
            if lock.remove(&plugin).is_some() {
                if let Err(e) = lock.save(&project_root) {
                    eprintln!("Warning: failed to update lock file: {}", e);
                }
            }
        }
    }

    Ok(())
}

/// Core remove logic: build graph, check dependents, delete plugin directory.
/// Extracted so regression tests can exercise the same code path as `run_remove`
/// without needing a global plugins_base_dir or a real project directory.
pub(crate) fn remove_plugin_from(reference: &str, force: bool, plugins_base: &Path) -> Result<()> {
    let plugin: PluginRef = reference.parse()?;

    let graph = PluginGraph::build(plugins_base);

    // Capture display name before removing (removal deletes the directory)
    let display_name = plugin.display_name(plugins_base);

    // Error if other installed plugins depend on this one (unless --force)
    let dependents = graph.dependents(&plugin);
    if !dependents.is_empty() {
        let names: Vec<String> = dependents.iter().map(|d| d.to_string()).collect();
        if force {
            eprintln!(
                "Warning: {} is a dependency of: {}",
                plugin,
                names.join(", ")
            );
        } else {
            return Err(AikiError::PluginHasDependents {
                plugin: plugin.to_string(),
                dependents: names.join(", "),
            });
        }
    }

    remove_plugin(&plugin, plugins_base)?;

    println!("Removed: {}", display_name);
    Ok(())
}

// --- Helpers ---

/// Find the project root (parent of `.aiki/` directory) if we're in a project.
fn find_project_root(start: &Path) -> Option<std::path::PathBuf> {
    find_aiki_dir_optional(start).and_then(|aiki_dir| aiki_dir.parent().map(|p| p.to_path_buf()))
}

fn find_aiki_dir(start: &Path) -> Result<std::path::PathBuf> {
    find_aiki_dir_optional(start).ok_or_else(|| AikiError::InvalidArgument(
        "Not in an aiki project. Use 'aiki plugin install <reference>' to install a specific plugin.".to_string(),
    ))
}

fn find_aiki_dir_optional(start: &Path) -> Option<std::path::PathBuf> {
    let mut current = start;
    loop {
        let aiki_dir = current.join(".aiki");
        if aiki_dir.is_dir() {
            return Some(aiki_dir);
        }
        current = current.parent()?;
    }
}

/// Print install report.
///
/// Validation (plugin.yaml check + cleanup) is handled by `install_single` in
/// `deps.rs`, so this function only formats the output.
fn print_install_report(report: &InstallReport, root: &PluginRef) {
    for (plugin, manifest) in &report.installed {
        let label = if plugin == root {
            "Installed"
        } else {
            "Installed (dependency)"
        };
        let plugin_str = plugin.to_string();
        match manifest.name.as_deref() {
            Some(name) if !name.is_empty() && name != plugin_str => {
                println!("{}: {} ({})", label, plugin, name);
            }
            _ => println!("{}: {}", label, plugin),
        }
    }
    for plugin in &report.rolled_back {
        eprintln!("Rolled back: {}", plugin);
    }
    for (failed, err) in &report.failed {
        eprintln!("Error: failed to install {} — {}", failed, err);
    }
}

/// Remove a plugin reference from `include:` lists in the project's `hooks.yml`.
///
/// Scans both top-level `include:` and `before:/after:` composition block includes.
/// Writes the file back only if changes were made.
fn remove_from_hooks_yml(aiki_dir: &Path, plugin: &PluginRef) {
    let hooks_path = aiki_dir.join("hooks.yml");
    if !hooks_path.is_file() {
        return;
    }

    let content = match fs::read_to_string(&hooks_path) {
        Ok(c) => c,
        Err(_) => return,
    };

    let plugin_str = plugin.to_string();

    // Filter out lines that are include references to this plugin.
    // We operate on raw lines to preserve formatting/comments.
    // TODO: This removes ALL list items matching the plugin name regardless of
    // which YAML key they appear under (not just `include:` blocks). Should be
    // replaced with YAML-aware removal (see ops/now/plugins/yaml-aware-hooks-removal.md).
    let lines: Vec<&str> = content.lines().collect();
    let mut new_lines: Vec<&str> = Vec::with_capacity(lines.len());
    let mut changed = false;

    for line in &lines {
        let trimmed = line.trim();
        // Match list items like "- aiki/way" or "- aiki/way  # comment"
        if trimmed.starts_with("- ") {
            let value = trimmed[2..].trim();
            // Strip trailing comment
            let value = value.split('#').next().unwrap_or(value).trim();
            if value == plugin_str {
                changed = true;
                continue; // Skip this line
            }
        }
        new_lines.push(line);
    }

    if changed {
        let new_content = new_lines.join("\n");
        // Preserve trailing newline if original had one
        let new_content = if content.ends_with('\n') && !new_content.ends_with('\n') {
            format!("{}\n", new_content)
        } else {
            new_content
        };
        if let Err(e) = fs::write(&hooks_path, new_content) {
            eprintln!("Warning: failed to update hooks.yml: {}", e);
        }
    }
}


/// Find project-level template overrides of plugin templates.
fn find_overrides(templates_dir: &Path, project_refs: &[PluginRef]) -> Vec<(String, String)> {
    let mut overrides = Vec::new();

    // For each plugin ref, check if there's a local template override
    let ref_set: HashSet<String> = project_refs.iter().map(|r| r.to_string()).collect();

    // Walk the templates directory looking for three-part paths matching known plugins
    collect_overrides(templates_dir, templates_dir, &ref_set, &mut overrides);

    overrides
}

fn collect_overrides(
    base: &Path,
    current: &Path,
    known_plugins: &HashSet<String>,
    overrides: &mut Vec<(String, String)>,
) {
    let entries = match fs::read_dir(current) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_overrides(base, &path, known_plugins, overrides);
        } else if path.is_file() && path.extension().map_or(false, |e| e == "md") {
            let relative = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .with_extension("")
                .display()
                .to_string()
                .replace('\\', "/");

            // Check if this is a three-part path matching a known plugin
            let parts: Vec<&str> = relative.split('/').collect();
            if parts.len() >= 3 {
                let plugin_ref = format!("{}/{}", parts[0], parts[1]);
                if known_plugins.contains(&plugin_ref) {
                    overrides.push((
                        relative,
                        path.strip_prefix(base.parent().unwrap_or(base))
                            .unwrap_or(&path)
                            .display()
                            .to_string(),
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_remove_from_hooks_yml_removes_matching_line() {
        let tmp = TempDir::new().unwrap();
        let aiki_dir = tmp.path().join(".aiki");
        fs::create_dir_all(&aiki_dir).unwrap();

        let hooks_content = "include:\n  - eslint/standard\n  - aiki/way\n";
        fs::write(aiki_dir.join("hooks.yml"), hooks_content).unwrap();

        let plugin: PluginRef = "eslint/standard".parse().unwrap();
        remove_from_hooks_yml(&aiki_dir, &plugin);

        let result = fs::read_to_string(aiki_dir.join("hooks.yml")).unwrap();
        assert!(!result.contains("eslint/standard"));
        assert!(result.contains("aiki/way"));
    }

    #[test]
    fn test_remove_from_hooks_yml_preserves_trailing_newline() {
        let tmp = TempDir::new().unwrap();
        let aiki_dir = tmp.path().join(".aiki");
        fs::create_dir_all(&aiki_dir).unwrap();

        let hooks_content = "include:\n  - eslint/standard\n  - aiki/way\n";
        fs::write(aiki_dir.join("hooks.yml"), hooks_content).unwrap();

        let plugin: PluginRef = "eslint/standard".parse().unwrap();
        remove_from_hooks_yml(&aiki_dir, &plugin);

        let result = fs::read_to_string(aiki_dir.join("hooks.yml")).unwrap();
        assert!(result.ends_with('\n'));
    }

    #[test]
    fn test_remove_from_hooks_yml_no_file() {
        let tmp = TempDir::new().unwrap();
        let aiki_dir = tmp.path().join(".aiki");
        fs::create_dir_all(&aiki_dir).unwrap();

        let plugin: PluginRef = "eslint/standard".parse().unwrap();
        // Should not panic when hooks.yml doesn't exist
        remove_from_hooks_yml(&aiki_dir, &plugin);
    }

    #[test]
    fn test_remove_from_hooks_yml_no_match() {
        let tmp = TempDir::new().unwrap();
        let aiki_dir = tmp.path().join(".aiki");
        fs::create_dir_all(&aiki_dir).unwrap();

        let original = "include:\n  - aiki/way\n";
        fs::write(aiki_dir.join("hooks.yml"), original).unwrap();

        let plugin: PluginRef = "eslint/standard".parse().unwrap();
        remove_from_hooks_yml(&aiki_dir, &plugin);

        let result = fs::read_to_string(aiki_dir.join("hooks.yml")).unwrap();
        assert_eq!(result, original);
    }

    #[test]
    fn test_remove_from_hooks_yml_with_comment() {
        let tmp = TempDir::new().unwrap();
        let aiki_dir = tmp.path().join(".aiki");
        fs::create_dir_all(&aiki_dir).unwrap();

        let hooks_content = "include:\n  - eslint/standard  # linting\n  - aiki/way\n";
        fs::write(aiki_dir.join("hooks.yml"), hooks_content).unwrap();

        let plugin: PluginRef = "eslint/standard".parse().unwrap();
        remove_from_hooks_yml(&aiki_dir, &plugin);

        let result = fs::read_to_string(aiki_dir.join("hooks.yml")).unwrap();
        assert!(!result.contains("eslint/standard"));
        assert!(result.contains("aiki/way"));
    }

    /// Helper: create a fake installed plugin with optional deps declared via hooks.yaml.
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

    // --- Regression tests for the plugin remove consumer path ---
    // These exercise remove_plugin_from() — the same code path run_remove() uses —
    // to catch miswiring between graph construction, dependent checking, and removal.

    #[test]
    fn test_remove_no_dependents_succeeds() {
        let tmp = TempDir::new().unwrap();
        create_fake_plugin(tmp.path(), "aiki", "alpha", &[]);

        // Should succeed: no dependents, plugin gets removed
        remove_plugin_from("aiki/alpha", false, tmp.path()).unwrap();
        assert!(!tmp.path().join("aiki").join("alpha").exists());
    }

    #[test]
    fn test_remove_blocked_by_direct_dependent() {
        let tmp = TempDir::new().unwrap();
        create_fake_plugin(tmp.path(), "aiki", "alpha", &[]);
        create_fake_plugin(tmp.path(), "aiki", "beta", &["aiki/alpha/tmpl"]);

        // Should fail: beta depends on alpha
        let err = remove_plugin_from("aiki/alpha", false, tmp.path()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("aiki/beta"), "error should name the dependent: {msg}");
        // Plugin should NOT have been removed
        assert!(tmp.path().join("aiki").join("alpha").exists());
    }

    #[test]
    fn test_remove_unrelated_plugin_not_blocked() {
        let tmp = TempDir::new().unwrap();
        create_fake_plugin(tmp.path(), "aiki", "alpha", &[]);
        create_fake_plugin(tmp.path(), "aiki", "beta", &["aiki/gamma/tmpl"]);
        create_fake_plugin(tmp.path(), "aiki", "gamma", &[]);

        // alpha has no dependents — removal should succeed
        remove_plugin_from("aiki/alpha", false, tmp.path()).unwrap();
        assert!(!tmp.path().join("aiki").join("alpha").exists());
    }

    #[test]
    fn test_remove_blocked_by_multiple_dependents() {
        let tmp = TempDir::new().unwrap();
        create_fake_plugin(tmp.path(), "aiki", "alpha", &[]);
        create_fake_plugin(tmp.path(), "aiki", "beta", &["aiki/alpha/tmpl"]);
        create_fake_plugin(tmp.path(), "aiki", "gamma", &["aiki/alpha/tmpl"]);

        let err = remove_plugin_from("aiki/alpha", false, tmp.path()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("aiki/beta"), "error should name beta: {msg}");
        assert!(msg.contains("aiki/gamma"), "error should name gamma: {msg}");
    }

    #[test]
    fn test_remove_only_blocked_by_direct_dependents() {
        let tmp = TempDir::new().unwrap();
        // A depends on B, B depends on C
        create_fake_plugin(tmp.path(), "aiki", "a", &["aiki/b/tmpl"]);
        create_fake_plugin(tmp.path(), "aiki", "b", &["aiki/c/tmpl"]);
        create_fake_plugin(tmp.path(), "aiki", "c", &[]);

        // Removing C should be blocked by B (direct dependent), not A (transitive)
        let err = remove_plugin_from("aiki/c", false, tmp.path()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("aiki/b"), "error should name direct dependent b: {msg}");
        assert!(!msg.contains("aiki/a"), "error should NOT name transitive dependent a: {msg}");
    }

    #[test]
    fn test_remove_nonexistent_plugin_fails() {
        let tmp = TempDir::new().unwrap();
        // No plugins installed — remove should fail at the removal step (not installed)
        let err = remove_plugin_from("aiki/nonexistent", false, tmp.path()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not installed") || msg.contains("nonexistent"), "expected not-installed error: {msg}");
    }

    #[test]
    fn test_remove_force_bypasses_dependent_check() {
        let tmp = TempDir::new().unwrap();
        create_fake_plugin(tmp.path(), "aiki", "alpha", &[]);
        create_fake_plugin(tmp.path(), "aiki", "beta", &["aiki/alpha/tmpl"]);

        // With force=true, removal should succeed despite dependents
        remove_plugin_from("aiki/alpha", true, tmp.path()).unwrap();
        assert!(!tmp.path().join("aiki").join("alpha").exists());
    }
}
