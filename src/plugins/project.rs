//! Project-level plugin operations.
//!
//! Shared functions used by `aiki init`, `aiki doctor`, and `aiki plugin install`.

use std::path::Path;

use super::scanner::derive_plugin_refs;
use super::{check_install_status, plugins_base_dir, InstallStatus, PluginRef};
use crate::error::Result;

/// Derive plugin references from a project's `.aiki/` directory.
pub fn derive_project_plugin_refs(project_root: &Path) -> Vec<PluginRef> {
    let aiki_dir = project_root.join(".aiki");
    if !aiki_dir.is_dir() {
        return Vec::new();
    }
    derive_plugin_refs(&aiki_dir, None)
}

/// Check the installation status of all plugins referenced by a project.
pub fn check_project_plugins(project_root: &Path) -> Result<Vec<(PluginRef, InstallStatus)>> {
    let plugins_base = plugins_base_dir()?;
    let refs = derive_project_plugin_refs(project_root);
    Ok(refs
        .into_iter()
        .map(|r| {
            let status = check_install_status(&r, &plugins_base);
            (r, status)
        })
        .collect())
}

/// Install all missing plugins referenced by a project (with deps).
///
/// Returns the number of plugins newly installed. Returns an error if any
/// plugin fails to install.
pub fn install_project_plugins(project_root: &Path) -> Result<usize> {
    let plugins_base = plugins_base_dir()?;
    let refs = derive_project_plugin_refs(project_root);
    let mut installed_count = 0;
    let mut all_failures: Vec<(PluginRef, String)> = Vec::new();

    for plugin in &refs {
        let report = super::install(plugin, &plugins_base, Some(project_root), None)?;
        for (p, _) in &report.installed {
            if refs.iter().any(|r| r == p) {
                println!("Installed plugin: {}", p);
            } else {
                println!("Installed plugin (dependency): {}", p);
            }
            installed_count += 1;
        }
        for (failed, err) in &report.failed {
            eprintln!("Error: failed to install plugin {}: {}", failed, err);
        }
        all_failures.extend(report.failed);
    }

    if !all_failures.is_empty() {
        let details = all_failures
            .iter()
            .map(|(p, e)| format!("{}: {}", p, e))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(crate::error::AikiError::PluginOperationFailed {
            plugin: "project plugins".to_string(),
            details: format!(
                "{} plugin(s) failed to install: {}",
                all_failures.len(),
                details
            ),
        });
    }

    Ok(installed_count)
}
