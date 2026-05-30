//! `aiki plugin` CLI subcommand — install, update, list, and remove plugins.

use clap::Subcommand;
use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::Path;

use crate::error::{AikiError, Result};
use crate::plugins::deps::{install_with_deps, resolve_deps, InstallReport};
use crate::plugins::git::{pull_plugin, remove_plugin};
use crate::plugins::scanner::derive_plugin_refs;
use crate::plugins::{check_install_status, plugins_base_dir, InstallStatus, PluginRef};

#[derive(Subcommand)]
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
    List,
    /// Remove an installed plugin
    Remove {
        /// Plugin reference to remove (e.g., aiki/way)
        reference: String,
    },
}

pub fn run(command: PluginCommands) -> Result<()> {
    match command {
        PluginCommands::Install { reference } => run_install(reference),
        PluginCommands::Update { reference } => run_update(reference),
        PluginCommands::List => run_list(),
        PluginCommands::Remove { reference } => run_remove(reference),
    }
}

fn run_install(reference: Option<String>) -> Result<()> {
    let plugins_base = plugins_base_dir()?;

    match reference {
        Some(ref_str) => {
            let plugin: PluginRef = ref_str.parse()?;
            let report = install_with_deps(&plugin, &plugins_base)?;
            print_install_report(&report, &plugin);

            if !report.failed.is_empty() {
                std::process::exit(1);
            }
        }
        None => {
            // Scan current project for plugin references
            let cwd = env::current_dir()?;
            let aiki_dir = find_aiki_dir(&cwd)?;

            let refs = derive_plugin_refs(&aiki_dir, None);
            if refs.is_empty() {
                println!("No plugin references found in project.");
                return Ok(());
            }

            let mut any_failed = false;
            for plugin in &refs {
                let report = install_with_deps(plugin, &plugins_base)?;
                print_install_report(&report, plugin);
                if !report.failed.is_empty() {
                    any_failed = true;
                }
            }

            if any_failed {
                std::process::exit(1);
            }
        }
    }

    Ok(())
}

fn run_update(reference: Option<String>) -> Result<()> {
    let plugins_base = plugins_base_dir()?;

    match reference {
        Some(ref_str) => {
            let plugin: PluginRef = ref_str.parse()?;
            update_single(&plugin, &plugins_base)?;
        }
        None => {
            // Update all installed plugins
            let installed = list_installed_plugins(&plugins_base);
            if installed.is_empty() {
                println!("No plugins installed.");
                return Ok(());
            }

            let mut any_failed = false;
            for plugin in &installed {
                if let Err(e) = update_single(plugin, &plugins_base) {
                    eprintln!("Error: failed to update {} — {}", plugin, e);
                    any_failed = true;
                }
            }

            if any_failed {
                std::process::exit(1);
            }
        }
    }

    Ok(())
}

fn update_single(plugin: &PluginRef, plugins_base: &Path) -> Result<()> {
    // 1. Pull the root plugin
    pull_plugin(plugin, plugins_base)?;
    println!("Updated: {}", plugin);

    let mut failures: Vec<String> = Vec::new();

    // 2. Resolve deps after pulling root, then pull existing deps
    let deps = resolve_deps(plugin, plugins_base);
    let mut pulled: HashSet<PluginRef> = HashSet::new();
    for dep in &deps {
        if check_install_status(dep, plugins_base) == InstallStatus::Installed {
            if let Err(e) = pull_plugin(dep, plugins_base) {
                eprintln!("Error: failed to update dependency {}: {}", dep, e);
                // Don't record failure yet — dep may be retried in second pass
            } else {
                pulled.insert(dep.clone());
            }
        }
    }

    // 3. Re-resolve deps after pulls — pulled deps may now reference new plugins
    let deps_after = resolve_deps(plugin, plugins_base);
    for dep in &deps_after {
        match check_install_status(dep, plugins_base) {
            InstallStatus::Installed => {
                // Pull deps discovered or retried after re-resolve (not yet pulled)
                if !pulled.contains(dep) {
                    if let Err(e) = pull_plugin(dep, plugins_base) {
                        eprintln!("Error: failed to update dependency {}: {}", dep, e);
                        failures.push(format!("{}: {}", dep, e));
                    }
                }
            }
            _ => {
                // New dep — install with its own deps
                let report = install_with_deps(dep, plugins_base)?;
                for installed in &report.installed {
                    println!("Installed (dependency): {}", installed);
                }
                for (failed, err) in &report.failed {
                    eprintln!("Error: failed to install {}: {}", failed, err);
                    failures.push(format!("{}: {}", failed, err));
                }
            }
        }
    }

    if !failures.is_empty() {
        return Err(AikiError::PluginOperationFailed {
            plugin: plugin.to_string(),
            details: format!("{} dependency failure(s): {}", failures.len(), failures.join("; ")),
        });
    }

    Ok(())
}

fn run_list() -> Result<()> {
    let plugins_base = plugins_base_dir()?;
    let cwd = env::current_dir()?;

    // Check if we're in a project
    let aiki_dir = find_aiki_dir_optional(&cwd);

    match aiki_dir {
        Some(aiki_dir) => {
            // In repo: show project context
            let project_refs = derive_plugin_refs(&aiki_dir, None);

            if project_refs.is_empty() {
                println!("No plugin references found in project.");
            } else {
                println!("Plugins:");
                for plugin in &project_refs {
                    let status = check_install_status(plugin, &plugins_base);
                    match status {
                        InstallStatus::Installed => {
                            println!("  {}    installed", plugin);
                            // Show deps
                            let deps = resolve_deps(plugin, &plugins_base);
                            for dep in &deps {
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
            let templates_dir = aiki_dir.join("templates");
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

            // Collect all deps so we can exclude them from top-level listing.
            // A plugin is a "root" (top-level) if no root plugin depends on it.
            // To handle cycles: only exclude a plugin from top-level if it is a
            // dependency of another plugin that is itself NOT a dependency of anyone.
            let mut plugin_deps: Vec<(PluginRef, Vec<PluginRef>)> = Vec::new();
            for plugin in &installed {
                let deps = resolve_deps(plugin, &plugins_base);
                plugin_deps.push((plugin.clone(), deps));
            }

            // First pass: deps of all plugins
            let all_deps: HashSet<PluginRef> = plugin_deps
                .iter()
                .flat_map(|(_, deps)| deps.iter().cloned())
                .collect();

            // A plugin is a root if it's not in all_deps. If there are no roots
            // (e.g. cycles where every plugin is someone's dependency), then deps
            // contributed only by non-root plugins shouldn't suppress top-level
            // display. Recompute: only deps of root plugins count as "hidden".
            let roots: HashSet<&PluginRef> = installed
                .iter()
                .filter(|p| !all_deps.contains(p))
                .collect();

            let hidden: HashSet<PluginRef> = if roots.is_empty() {
                // No roots means all plugins are in cycles — show all as top-level
                HashSet::new()
            } else {
                // Only deps reachable from root plugins are hidden from top-level
                plugin_deps
                    .iter()
                    .filter(|(p, _)| roots.contains(p))
                    .flat_map(|(_, deps)| deps.iter().cloned())
                    .collect()
            };

            println!("Installed (~/.aiki/plugins/):");
            for (plugin, deps) in &plugin_deps {
                if hidden.contains(plugin) {
                    continue; // Only show as dependency under its parent
                }
                println!("  {}", plugin);
                for dep in deps {
                    println!("    └ {}    (dependency)", dep);
                }
            }
        }
    }

    Ok(())
}

fn run_remove(reference: String) -> Result<()> {
    let plugins_base = plugins_base_dir()?;
    let plugin: PluginRef = reference.parse()?;
    remove_plugin(&plugin, &plugins_base)?;
    println!("Removed: {}", plugin);
    Ok(())
}

// --- Helpers ---

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

fn print_install_report(report: &InstallReport, root: &PluginRef) {
    for plugin in &report.installed {
        if plugin == root {
            println!("Installed: {}", plugin);
        } else {
            println!("Installed (dependency): {}", plugin);
        }
    }
    for (failed, err) in &report.failed {
        eprintln!("Error: failed to install {} — {}", failed, err);
    }
}

/// List all installed plugins by scanning the plugins directory.
fn list_installed_plugins(plugins_base: &Path) -> Vec<PluginRef> {
    let mut plugins = Vec::new();

    if !plugins_base.is_dir() {
        return plugins;
    }

    let ns_entries = match fs::read_dir(plugins_base) {
        Ok(e) => e,
        Err(_) => return plugins,
    };

    for ns_entry in ns_entries.flatten() {
        let ns_path = ns_entry.path();
        if !ns_path.is_dir() {
            continue;
        }

        let namespace = match ns_path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };

        let name_entries = match fs::read_dir(&ns_path) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for name_entry in name_entries.flatten() {
            let name_path = name_entry.path();
            if !name_path.is_dir() {
                continue;
            }

            let name = match name_path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };

            // Only count as installed if .git/ exists
            if name_path.join(".git").is_dir() {
                if let Ok(plugin) = format!("{}/{}", namespace, name).parse::<PluginRef>() {
                    plugins.push(plugin);
                }
            }
        }
    }

    plugins.sort_by(|a, b| a.to_string().cmp(&b.to_string()));
    plugins
}

/// Find project-level template overrides of plugin templates.
fn find_overrides(
    templates_dir: &Path,
    project_refs: &[PluginRef],
) -> Vec<(String, String)> {
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
