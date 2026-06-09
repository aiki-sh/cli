//! Integration tests for the plugin system.
//!
//! These tests exercise reference scanning, dependency resolution, template
//! resolution with plugins, and CLI subcommand wiring — all without network
//! access (no actual git clones).

use std::fs;
use std::path::Path;
use tempfile::TempDir;

use aiki::plugins::scanner::derive_plugin_refs;
use aiki::plugins::{check_install_status, InstallStatus, PluginRef};

// ---------------------------------------------------------------------------
// Helper: create a fake installed plugin (dir + .git/ marker)
// ---------------------------------------------------------------------------
fn fake_install(plugins_base: &Path, ns: &str, name: &str) {
    let dir = plugins_base.join(ns).join(name);
    fs::create_dir_all(dir.join(".git")).unwrap();
}

/// Create a fake installed plugin that itself references other plugins
/// (via a hooks.yaml with template: lines under unique parent keys).
fn fake_install_with_deps(plugins_base: &Path, ns: &str, name: &str, dep_refs: &[&str]) {
    let dir = plugins_base.join(ns).join(name);
    fs::create_dir_all(dir.join(".git")).unwrap();
    let mut yaml = String::new();
    for (i, dep) in dep_refs.iter().enumerate() {
        yaml.push_str(&format!("hook{}:\n  template: {}\n", i, dep));
    }
    fs::write(dir.join("hooks.yaml"), yaml).unwrap();
}

// ---------------------------------------------------------------------------
// Reference scanning — end-to-end
// ---------------------------------------------------------------------------

#[test]
fn test_scan_project_finds_yaml_and_markdown_refs() {
    let tmp = TempDir::new().unwrap();
    let aiki_dir = tmp.path().join(".aiki");

    // Create hooks.yaml with a three-part template ref
    fs::create_dir_all(&aiki_dir).unwrap();
    fs::write(
        aiki_dir.join("hooks.yaml"),
        "on_review:\n  template: acme/security/scan\n",
    )
    .unwrap();

    // Create a template with a partial ref
    let tpl_dir = aiki_dir.join("tasks");
    fs::create_dir_all(&tpl_dir).unwrap();
    fs::write(
        tpl_dir.join("review.md"),
        "# Review\n\n{{> corp/lint/check}}\n",
    )
    .unwrap();

    let refs = derive_plugin_refs(&aiki_dir, None);

    let ref_strs: Vec<String> = refs.iter().map(|r| r.to_string()).collect();
    assert!(ref_strs.contains(&"acme/security".to_string()));
    assert!(ref_strs.contains(&"corp/lint".to_string()));
    assert_eq!(refs.len(), 2);
}

#[test]
fn test_scan_excludes_code_blocks_and_inline_code() {
    let tmp = TempDir::new().unwrap();
    let aiki_dir = tmp.path().join(".aiki");
    let tpl_dir = aiki_dir.join("tasks");
    fs::create_dir_all(&tpl_dir).unwrap();

    // This template has refs in code blocks (should be excluded) and real refs
    let content = r#"# Template

Real partial: {{> real/plugin/template}}

```markdown
Fenced code: {{> fake/plugin/excluded}}
```

Inline code: `{{> inline/plugin/also_excluded}}`

<!-- HTML comment: {{> comment/plugin/hidden}} -->
"#;
    fs::write(tpl_dir.join("mixed.md"), content).unwrap();

    let refs = derive_plugin_refs(&aiki_dir, None);
    let ref_strs: Vec<String> = refs.iter().map(|r| r.to_string()).collect();

    assert!(ref_strs.contains(&"real/plugin".to_string()));
    assert!(!ref_strs.contains(&"fake/plugin".to_string()));
    assert!(!ref_strs.contains(&"inline/plugin".to_string()));
    assert!(!ref_strs.contains(&"comment/plugin".to_string()));
    assert_eq!(refs.len(), 1);
}

#[test]
fn test_scan_deduplicates_across_files() {
    let tmp = TempDir::new().unwrap();
    let aiki_dir = tmp.path().join(".aiki");
    let tpl_dir = aiki_dir.join("tasks");
    fs::create_dir_all(&tpl_dir).unwrap();

    // Same plugin referenced from two different templates
    fs::write(tpl_dir.join("a.md"), "{{> shared/plugin/one}}\n").unwrap();
    fs::write(tpl_dir.join("b.md"), "{{> shared/plugin/two}}\n").unwrap();

    let refs = derive_plugin_refs(&aiki_dir, None);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].to_string(), "shared/plugin");
}

#[test]
fn test_scan_filters_self_ref() {
    let tmp = TempDir::new().unwrap();
    let aiki_dir = tmp.path().join(".aiki");
    let tpl_dir = aiki_dir.join("tasks");
    fs::create_dir_all(&tpl_dir).unwrap();

    fs::write(
        tpl_dir.join("a.md"),
        "{{> myself/plugin/tpl}}\n{{> other/plugin/tpl}}\n",
    )
    .unwrap();

    let self_ref: PluginRef = "myself/plugin".parse().unwrap();
    let refs = derive_plugin_refs(&aiki_dir, Some(&self_ref));

    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].to_string(), "other/plugin");
}

// ---------------------------------------------------------------------------
// Dependency resolution — end-to-end with temp plugin tree
// ---------------------------------------------------------------------------

#[test]
fn test_resolve_deps_transitive_chain() {
    use aiki::plugins::deps::resolve_deps;

    let tmp = TempDir::new().unwrap();
    let base = tmp.path();

    // A depends on B, B depends on C
    fake_install_with_deps(base, "ns", "a", &["ns/b/tpl"]);
    fake_install_with_deps(base, "ns", "b", &["ns/c/tpl"]);
    fake_install(base, "ns", "c");

    let root: PluginRef = "ns/a".parse().unwrap();
    let deps = resolve_deps(&root, base);

    let dep_strs: Vec<String> = deps.iter().map(|r| r.to_string()).collect();
    assert!(dep_strs.contains(&"ns/b".to_string()));
    assert!(dep_strs.contains(&"ns/c".to_string()));
    // Root should not be in deps
    assert!(!dep_strs.contains(&"ns/a".to_string()));
}

#[test]
fn test_resolve_deps_diamond() {
    use aiki::plugins::deps::resolve_deps;

    let tmp = TempDir::new().unwrap();
    let base = tmp.path();

    // A depends on B and C; both B and C depend on D
    fake_install_with_deps(base, "ns", "a", &["ns/b/tpl", "ns/c/tpl"]);
    fake_install_with_deps(base, "ns", "b", &["ns/d/tpl"]);
    fake_install_with_deps(base, "ns", "c", &["ns/d/tpl"]);
    fake_install(base, "ns", "d");

    let root: PluginRef = "ns/a".parse().unwrap();
    let deps = resolve_deps(&root, base);

    let dep_strs: Vec<String> = deps.iter().map(|r| r.to_string()).collect();
    assert!(dep_strs.contains(&"ns/b".to_string()));
    assert!(dep_strs.contains(&"ns/c".to_string()));
    assert!(dep_strs.contains(&"ns/d".to_string()));

    // D should appear exactly once (dedup)
    assert_eq!(dep_strs.iter().filter(|s| *s == "ns/d").count(), 1);
}

#[test]
fn test_resolve_deps_cycle_does_not_hang() {
    use aiki::plugins::deps::resolve_deps;

    let tmp = TempDir::new().unwrap();
    let base = tmp.path();

    // A depends on B, B depends on A (cycle)
    fake_install_with_deps(base, "ns", "a", &["ns/b/tpl"]);
    fake_install_with_deps(base, "ns", "b", &["ns/a/tpl"]);

    let root: PluginRef = "ns/a".parse().unwrap();
    let deps = resolve_deps(&root, base);

    // Should complete without hanging; B is a dep of A
    let dep_strs: Vec<String> = deps.iter().map(|r| r.to_string()).collect();
    assert!(dep_strs.contains(&"ns/b".to_string()));
}

// ---------------------------------------------------------------------------
// Install status checking
// ---------------------------------------------------------------------------

#[test]
fn test_install_status_lifecycle() {
    let tmp = TempDir::new().unwrap();
    let base = tmp.path();
    let plugin: PluginRef = "test/myplugin".parse().unwrap();

    // Not installed
    assert_eq!(
        check_install_status(&plugin, base),
        InstallStatus::NotInstalled
    );

    // Create directory without .git/ → partial
    let dir = plugin.install_dir(base);
    fs::create_dir_all(&dir).unwrap();
    assert_eq!(
        check_install_status(&plugin, base),
        InstallStatus::PartialInstall
    );

    // Add .git/ → installed
    fs::create_dir_all(dir.join(".git")).unwrap();
    assert_eq!(
        check_install_status(&plugin, base),
        InstallStatus::Installed
    );
}

// ---------------------------------------------------------------------------
// Template resolution with plugin fallback (via public load_template API)
// ---------------------------------------------------------------------------

/// Mutex to serialize tests that modify the AIKI_HOME env var.
static AIKI_HOME_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Run a closure with AIKI_HOME pointing to a temp directory.
/// Restores the original value (or removes it) when done, even on panic.
fn with_temp_aiki_home<F, R>(aiki_home: &Path, f: F) -> R
where
    F: FnOnce() -> R,
{
    struct RestoreEnv(Option<String>);
    impl Drop for RestoreEnv {
        fn drop(&mut self) {
            match self.0.take() {
                Some(v) => std::env::set_var("AIKI_HOME", v),
                None => std::env::remove_var("AIKI_HOME"),
            }
        }
    }

    let _lock = AIKI_HOME_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let _restore = RestoreEnv(std::env::var("AIKI_HOME").ok());
    std::env::set_var("AIKI_HOME", aiki_home);
    f()
}

#[test]
fn test_template_resolution_three_part_ref_falls_back_to_plugin() {
    use aiki::tasks::templates::resolver::load_template;

    let tmp = TempDir::new().unwrap();
    let aiki_home = TempDir::new().unwrap();

    // Create project .aiki/tasks/ (empty — no override)
    let project_templates = tmp.path().join(".aiki").join("tasks");
    fs::create_dir_all(&project_templates).unwrap();

    // Create a fake plugin with a template under the temp AIKI_HOME
    let plugins_base = aiki_home.path().join("plugins");
    let plugin_tpl_dir = plugins_base
        .join("testns")
        .join("testplug")
        .join("tasks");
    fs::create_dir_all(&plugin_tpl_dir).unwrap();
    fs::create_dir_all(plugins_base.join("testns").join("testplug").join(".git")).unwrap();
    fs::write(
        plugin_tpl_dir.join("review.md"),
        "---\nname: Plugin Review\n---\n# Plugin review template\n",
    )
    .unwrap();

    // Point AIKI_HOME to our temp dir so plugins_base_dir() resolves there
    with_temp_aiki_home(aiki_home.path(), || {
        let result = load_template("testns/testplug/review", &project_templates);
        assert!(
            result.is_ok(),
            "Should resolve three-part ref to plugin: {:?}",
            result
        );
    });
}

#[test]
fn test_template_resolution_project_override_wins() {
    use aiki::tasks::templates::resolver::load_template;

    let tmp = TempDir::new().unwrap();
    let aiki_home = TempDir::new().unwrap();

    // Create project templates dir
    let project_templates = tmp.path().join(".aiki").join("tasks");

    let plugins_base = aiki_home.path().join("plugins");

    // Create plugin template
    let plugin_tpl_dir = plugins_base
        .join("testns2")
        .join("overplug")
        .join("tasks");
    fs::create_dir_all(&plugin_tpl_dir).unwrap();
    fs::create_dir_all(plugins_base.join("testns2").join("overplug").join(".git")).unwrap();
    fs::write(
        plugin_tpl_dir.join("task.md"),
        "---\nname: Plugin Version\n---\n# Plugin version\n",
    )
    .unwrap();

    // Create project override at .aiki/tasks/testns2/overplug/task.md
    let project_tpl = project_templates.join("testns2").join("overplug");
    fs::create_dir_all(&project_tpl).unwrap();
    fs::write(
        project_tpl.join("task.md"),
        "---\nname: Project Override\n---\n# Project override\n",
    )
    .unwrap();

    // Point AIKI_HOME to our temp dir so plugins_base_dir() resolves there
    with_temp_aiki_home(aiki_home.path(), || {
        let result = load_template("testns2/overplug/task", &project_templates);
        assert!(
            result.is_ok(),
            "Should resolve to project override: {:?}",
            result
        );

        let template = result.unwrap();
        // The project version should win — source_path should point to the project dir, not plugins
        let source = template.source_path.unwrap_or_default();
        assert!(
            source.starts_with(tmp.path().to_str().unwrap()),
            "Project override should win over plugin. source_path: {}",
            source
        );
    });
}

// ---------------------------------------------------------------------------
// CLI wiring tests (assert_cmd — no network)
// ---------------------------------------------------------------------------

#[test]
fn test_plugin_help() {
    use assert_cmd::prelude::*;
    use predicates::prelude::*;
    use std::process::Command;

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.args(["plugin", "--help"]);

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("install"))
        .stdout(predicate::str::contains("update"))
        .stdout(predicate::str::contains("list"))
        .stdout(predicate::str::contains("remove"));
}

#[test]
fn test_plugin_remove_invalid_ref() {
    use assert_cmd::prelude::*;
    use predicates::prelude::*;
    use std::process::Command;

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.args(["plugin", "remove", "not-a-valid-ref"]);

    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("namespace/name"));
}

#[test]
fn test_plugin_remove_with_dot_in_namespace() {
    use assert_cmd::prelude::*;
    use predicates::prelude::*;
    use std::process::Command;

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.args(["plugin", "remove", "github.com/repo"]);

    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("Only GitHub plugins"));
}

/// Outside-repo `plugin list` reports no project plugin references.
#[test]
fn test_plugin_list_outside_repo_cycle_shows_all_top_level() {
    use std::process::Command;

    let aiki_home = TempDir::new().unwrap();
    let plugins_base = aiki_home.path().join("plugins");

    // A→B→A (cycle: every plugin is someone's dependency)
    fake_install_with_deps(plugins_base.as_path(), "ns", "alpha", &["ns/beta/tmpl"]);
    fake_install_with_deps(plugins_base.as_path(), "ns", "beta", &["ns/alpha/tmpl"]);

    // Run from a temp dir without .aiki (outside-repo path)
    let workdir = TempDir::new().unwrap();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.args(["plugin", "list"])
        .current_dir(workdir.path())
        .env("AIKI_HOME", aiki_home.path());

    let output = cmd.output().unwrap();
    assert!(output.status.success(), "plugin list should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("No plugin references found in project."),
        "expected outside-repo plugin list message, got: {}",
        stdout
    );
}

/// Outside-repo `plugin list` reports no project plugin references.
#[test]
fn test_plugin_list_outside_repo_hides_deps_of_roots() {
    use std::process::Command;

    let aiki_home = TempDir::new().unwrap();
    let plugins_base = aiki_home.path().join("plugins");

    // root→dep (root is not a dependency of anyone)
    fake_install_with_deps(plugins_base.as_path(), "ns", "root", &["ns/dep/tmpl"]);
    fake_install(plugins_base.as_path(), "ns", "dep");

    let workdir = TempDir::new().unwrap();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.args(["plugin", "list"])
        .current_dir(workdir.path())
        .env("AIKI_HOME", aiki_home.path());

    let output = cmd.output().unwrap();
    assert!(output.status.success(), "plugin list should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("No plugin references found in project."),
        "expected outside-repo plugin list message, got: {}",
        stdout
    );
}

// ---------------------------------------------------------------------------
// Update-all continue-on-error
// ---------------------------------------------------------------------------

/// `plugin update` (update-all) attempts every installed plugin even when
/// one fails, then exits non-zero.  This ensures the continue-on-error
/// semantics are not regressed.
#[test]
fn test_plugin_update_all_continues_on_error() {
    use assert_cmd::prelude::*;
    use predicates::prelude::*;
    use std::process::Command;

    let aiki_home = TempDir::new().unwrap();
    let plugins_base = aiki_home.path().join("plugins");

    // Create two fake plugins with .git/ dirs (count as "installed") but
    // without real git repos — `git pull` will fail for both.
    fake_install(&plugins_base, "org", "alpha");
    fake_install(&plugins_base, "org", "beta");

    let workdir = TempDir::new().unwrap();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.args(["plugin", "update"])
        .current_dir(workdir.path())
        .env("AIKI_HOME", aiki_home.path());

    // Both plugins should be attempted (per-plugin error messages) and
    // the command should exit non-zero.
    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("org/alpha"))
        .stderr(predicate::str::contains("org/beta"));
}

// ---------------------------------------------------------------------------
// Project-level plugin operations
// ---------------------------------------------------------------------------

#[test]
fn test_check_project_plugins_empty_project() {
    use aiki::plugins::project::derive_project_plugin_refs;

    let tmp = TempDir::new().unwrap();
    // No .aiki dir → no refs
    let refs = derive_project_plugin_refs(tmp.path());
    assert!(refs.is_empty());
}

#[test]
fn test_check_project_plugins_with_refs() {
    use aiki::plugins::project::derive_project_plugin_refs;

    let tmp = TempDir::new().unwrap();
    let aiki_dir = tmp.path().join(".aiki");
    fs::create_dir_all(&aiki_dir).unwrap();
    fs::write(
        aiki_dir.join("hooks.yaml"),
        "hook:\n  template: vendor/tool/scan\n",
    )
    .unwrap();

    let refs = derive_project_plugin_refs(tmp.path());
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].to_string(), "vendor/tool");
}

// ---------------------------------------------------------------------------
// Scanner — tasks/ directory (not templates/)
// ---------------------------------------------------------------------------

#[test]
fn test_scan_uses_tasks_dir_not_templates() {
    let tmp = TempDir::new().unwrap();
    let aiki_dir = tmp.path().join(".aiki");

    // Put a ref in the old templates/ dir — should NOT be found
    let old_dir = aiki_dir.join("templates");
    fs::create_dir_all(&old_dir).unwrap();
    fs::write(
        old_dir.join("review.md"),
        "{{> old/plugin/ref}}\n",
    )
    .unwrap();

    // Put a ref in the new tasks/ dir — should be found
    let new_dir = aiki_dir.join("tasks");
    fs::create_dir_all(&new_dir).unwrap();
    fs::write(
        new_dir.join("review.md"),
        "{{> new/plugin/ref}}\n",
    )
    .unwrap();

    let refs = derive_plugin_refs(&aiki_dir, None);
    let ref_strs: Vec<String> = refs.iter().map(|r| r.to_string()).collect();

    assert!(ref_strs.contains(&"new/plugin".to_string()));
    assert!(!ref_strs.contains(&"old/plugin".to_string()));
    assert_eq!(refs.len(), 1);
}

// ---------------------------------------------------------------------------
// Hook resolver — installed plugin lookup
// ---------------------------------------------------------------------------

#[test]
fn test_hook_resolver_not_found_error() {
    use aiki::flows::hook_resolver::HookResolver;

    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".aiki/hooks")).unwrap();

    let resolver = HookResolver::with_start_dir(tmp.path()).unwrap();
    let result = resolver.resolve("nonexistent/plugin");
    assert!(result.is_err(), "Should return error for missing plugin");
}

// ---------------------------------------------------------------------------
// Template resolver — three-part ref, TemplateNotFound
// ---------------------------------------------------------------------------

#[test]
fn test_template_resolution_three_part_ref_uses_tasks_dir() {
    use aiki::tasks::templates::resolver::load_template;

    let tmp = TempDir::new().unwrap();
    let aiki_home = TempDir::new().unwrap();

    // Create project .aiki/tasks/ (empty)
    let project_templates = tmp.path().join(".aiki").join("tasks");
    fs::create_dir_all(&project_templates).unwrap();

    // Create plugin with template in tasks/ dir (not templates/)
    let plugins_base = aiki_home.path().join("plugins");
    let plugin_tasks_dir = plugins_base.join("ns").join("plug").join("tasks");
    fs::create_dir_all(&plugin_tasks_dir).unwrap();
    fs::create_dir_all(plugins_base.join("ns").join("plug").join(".git")).unwrap();
    fs::write(
        plugin_tasks_dir.join("mytpl.md"),
        "---\nname: Plugin Template\n---\n# From plugin tasks dir\n",
    )
    .unwrap();

    with_temp_aiki_home(aiki_home.path(), || {
        let result = load_template("ns/plug/mytpl", &project_templates);
        assert!(
            result.is_ok(),
            "Should resolve three-part ref from plugin tasks/ dir: {:?}",
            result
        );
    });
}

#[test]
fn test_template_not_found_for_missing_plugin_template() {
    use aiki::tasks::templates::resolver::load_template;

    let tmp = TempDir::new().unwrap();
    let aiki_home = TempDir::new().unwrap();

    let project_templates = tmp.path().join(".aiki").join("tasks");
    fs::create_dir_all(&project_templates).unwrap();

    with_temp_aiki_home(aiki_home.path(), || {
        let result = load_template("nonexist/noplugin/notemplate", &project_templates);
        assert!(result.is_err(), "Should return TemplateNotFound");
    });
}

#[test]
fn test_template_short_names_and_two_part_refs_still_work() {
    use aiki::tasks::templates::resolver::load_template;

    let tmp = TempDir::new().unwrap();
    let project_templates = tmp.path().join(".aiki").join("tasks");
    fs::create_dir_all(&project_templates).unwrap();

    // Create a simple template
    fs::write(
        project_templates.join("review.md"),
        "---\nname: Review\n---\n# Review template\n",
    )
    .unwrap();

    // Create a namespaced (two-part) template
    let ns_dir = project_templates.join("myns");
    fs::create_dir_all(&ns_dir).unwrap();
    fs::write(
        ns_dir.join("custom.md"),
        "---\nname: Custom\n---\n# Custom template\n",
    )
    .unwrap();

    // Short name should work
    let result = load_template("review", &project_templates);
    assert!(result.is_ok(), "Short name should work: {:?}", result);

    // Two-part ref should work
    let result = load_template("myns/custom", &project_templates);
    assert!(result.is_ok(), "Two-part ref should work: {:?}", result);
}

// ---------------------------------------------------------------------------
// Plugin manifest — integration tests
// ---------------------------------------------------------------------------

#[test]
fn test_manifest_display_name_from_plugin_yaml() {
    use aiki::plugins::manifest::load_manifest;

    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join("plugin.yaml"),
        "name: My Awesome Plugin\n",
    )
    .unwrap();

    let manifest = load_manifest(tmp.path()).unwrap();
    assert_eq!(manifest.name.as_deref(), Some("My Awesome Plugin"));
}

#[test]
fn test_manifest_missing_plugin_yaml_error() {
    use aiki::plugins::manifest::load_manifest;

    let tmp = TempDir::new().unwrap();
    // No plugin.yaml — should error
    let err = load_manifest(tmp.path()).unwrap_err();
    assert!(
        err.to_string().contains("Missing plugin.yaml"),
        "Should report missing plugin.yaml: {}",
        err
    );
}

#[test]
fn test_manifest_invalid_plugin_yaml_error() {
    use aiki::plugins::manifest::load_manifest;

    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join("plugin.yaml"),
        "totally: not\nthe: right\nformat: at all\n",
    )
    .unwrap();

    let err = load_manifest(tmp.path()).unwrap_err();
    assert!(
        err.to_string().contains("Failed to parse plugin.yaml"),
        "Should report invalid plugin.yaml: {}",
        err
    );
}

// ---------------------------------------------------------------------------
// Plugin install/remove — validates plugin.yaml
// ---------------------------------------------------------------------------

#[test]
fn test_plugin_remove_nonexistent_fails() {
    use assert_cmd::prelude::*;
    use predicates::prelude::*;
    use std::process::Command;

    let aiki_home = TempDir::new().unwrap();
    let workdir = TempDir::new().unwrap();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.args(["plugin", "remove", "nonexist/plugin"])
        .current_dir(workdir.path())
        .env("AIKI_HOME", aiki_home.path());

    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("not installed"));
}

// ---------------------------------------------------------------------------
// Install directory structure — verifies {ns}/{name}/ layout
// ---------------------------------------------------------------------------

#[test]
fn test_install_dir_creates_ns_name_structure() {
    let tmp = TempDir::new().unwrap();
    let plugin: PluginRef = "myorg/security-hooks".parse().unwrap();
    let dir = plugin.install_dir(tmp.path());

    // Verify the path components match the namespace/name structure
    assert_eq!(
        dir,
        tmp.path().join("myorg").join("security-hooks"),
        "install_dir should produce base/namespace/name"
    );

    // Create the structure and verify it exists as expected
    fs::create_dir_all(dir.join(".git")).unwrap();
    assert!(tmp.path().join("myorg").is_dir());
    assert!(tmp.path().join("myorg").join("security-hooks").is_dir());
    assert!(
        tmp.path()
            .join("myorg")
            .join("security-hooks")
            .join(".git")
            .is_dir()
    );
}

#[test]
fn test_install_dir_preserves_aiki_namespace() {
    let plugin: PluginRef = "aiki/way".parse().unwrap();
    let base = Path::new("/fake/plugins");
    let dir = plugin.install_dir(base);

    // install_dir uses the original namespace, not the resolved GitHub owner
    assert_eq!(dir, Path::new("/fake/plugins/aiki/way"));
}

// ---------------------------------------------------------------------------
// Display name resolution — full fallback chain (integration)
// ---------------------------------------------------------------------------

#[test]
fn test_display_name_resolution_full_chain() {
    use aiki::plugins::manifest::resolve_display_name;

    let tmp = TempDir::new().unwrap();

    // 1. With plugin.yaml name → uses manifest name
    fs::write(tmp.path().join("plugin.yaml"), "name: From Manifest\n").unwrap();
    fs::write(
        tmp.path().join("hooks.yaml"),
        "name: From Hooks\nversion: '1'\n",
    )
    .unwrap();
    assert_eq!(
        resolve_display_name(tmp.path(), "ns/plugin"),
        "From Manifest"
    );

    // 2. Remove manifest name, keep hooks → uses hooks name
    fs::write(tmp.path().join("plugin.yaml"), "{}\n").unwrap();
    assert_eq!(
        resolve_display_name(tmp.path(), "ns/plugin"),
        "From Hooks"
    );

    // 3. Remove hooks name too → falls back to path
    fs::remove_file(tmp.path().join("hooks.yaml")).unwrap();
    fs::remove_file(tmp.path().join("plugin.yaml")).unwrap();
    assert_eq!(
        resolve_display_name(tmp.path(), "ns/plugin"),
        "ns/plugin"
    );
}

// ---------------------------------------------------------------------------
// Hook resolver — installed plugin lookup via AIKI_HOME
// ---------------------------------------------------------------------------

#[test]
fn test_hook_resolver_installed_plugin_via_aiki_home() {
    use aiki::flows::hook_resolver::HookResolver;

    let tmp = TempDir::new().unwrap();
    let aiki_home = TempDir::new().unwrap();

    // Create minimal .aiki dir in project (no hooks override)
    fs::create_dir_all(tmp.path().join(".aiki/hooks")).unwrap();

    // Create installed plugin under AIKI_HOME: $AIKI_HOME/plugins/{ns}/{name}/hooks.yaml
    let plugin_dir = aiki_home
        .path()
        .join("plugins")
        .join("vendor")
        .join("lint");
    fs::create_dir_all(&plugin_dir).unwrap();
    let hooks_path = plugin_dir.join("hooks.yaml");
    fs::write(&hooks_path, "name: vendor-lint\nversion: \"1\"\n").unwrap();

    // Point AIKI_HOME to our temp dir so the resolver's step 3 finds the plugin there
    with_temp_aiki_home(aiki_home.path(), || {
        let resolver = HookResolver::with_start_dir(tmp.path()).unwrap();
        let resolved = resolver.resolve("vendor/lint").unwrap();
        assert_eq!(
            resolved.canonicalize().unwrap(),
            hooks_path.canonicalize().unwrap()
        );
    });
}

// ---------------------------------------------------------------------------
// Scanner — plugin dependency scanning uses hooks.yaml in tasks/ dir
// ---------------------------------------------------------------------------

#[test]
fn test_scan_plugin_deps_via_hooks_yaml() {
    let tmp = TempDir::new().unwrap();
    let plugin_dir = tmp.path().join("ns").join("myplugin");
    fs::create_dir_all(plugin_dir.join(".git")).unwrap();

    // Plugin has hooks.yaml referencing another plugin's template
    fs::write(
        plugin_dir.join("hooks.yaml"),
        "review:\n  template: dep/other/scan\n",
    )
    .unwrap();

    // Scanner should find the dependency ref when scanning the plugin's .aiki-like dir
    // (derive_plugin_refs scans hooks.yaml at the aiki_dir level)
    let refs = derive_plugin_refs(&plugin_dir, None);
    let ref_strs: Vec<String> = refs.iter().map(|r| r.to_string()).collect();
    assert!(
        ref_strs.contains(&"dep/other".to_string()),
        "Should find plugin refs from hooks.yaml: {:?}",
        ref_strs
    );
}

#[test]
fn test_scan_plugin_with_tasks_dir_refs() {
    let tmp = TempDir::new().unwrap();
    let plugin_dir = tmp.path().join("ns").join("myplugin");
    fs::create_dir_all(plugin_dir.join(".git")).unwrap();

    // Plugin has templates in tasks/ dir that reference other plugins
    let tasks_dir = plugin_dir.join("tasks");
    fs::create_dir_all(&tasks_dir).unwrap();
    fs::write(
        tasks_dir.join("review.md"),
        "# Review\n\n{{> dep/lint/check}}\n",
    )
    .unwrap();

    let refs = derive_plugin_refs(&plugin_dir, None);
    let ref_strs: Vec<String> = refs.iter().map(|r| r.to_string()).collect();
    assert!(
        ref_strs.contains(&"dep/lint".to_string()),
        "Should find plugin refs from tasks/ dir partials: {:?}",
        ref_strs
    );
}

// ---------------------------------------------------------------------------
// Template resolution — plugin templates/ dir NOT used (only tasks/)
// ---------------------------------------------------------------------------

#[test]
fn test_template_resolution_ignores_old_templates_dir_in_plugin() {
    use aiki::tasks::templates::resolver::load_template;

    let tmp = TempDir::new().unwrap();
    let aiki_home = TempDir::new().unwrap();

    let project_templates = tmp.path().join(".aiki").join("tasks");
    fs::create_dir_all(&project_templates).unwrap();

    // Create plugin with template in old templates/ dir (should NOT resolve)
    let plugins_base = aiki_home.path().join("plugins");
    let plugin_old_dir = plugins_base.join("ns").join("oldplug").join("templates");
    fs::create_dir_all(&plugin_old_dir).unwrap();
    fs::create_dir_all(plugins_base.join("ns").join("oldplug").join(".git")).unwrap();
    fs::write(
        plugin_old_dir.join("check.md"),
        "---\nname: Old Template\n---\n# Should not be found\n",
    )
    .unwrap();

    with_temp_aiki_home(aiki_home.path(), || {
        let result = load_template("ns/oldplug/check", &project_templates);
        assert!(
            result.is_err(),
            "Should NOT resolve template from plugin templates/ dir (only tasks/)"
        );
    });
}

// ---------------------------------------------------------------------------
// Manifest — deny_unknown_fields rejects unexpected keys
// ---------------------------------------------------------------------------

#[test]
fn test_manifest_rejects_unknown_fields() {
    use aiki::plugins::manifest::load_manifest;

    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join("plugin.yaml"),
        "name: Valid\nunknown_field: should_fail\n",
    )
    .unwrap();

    let err = load_manifest(tmp.path()).unwrap_err();
    assert!(
        err.to_string().contains("Failed to parse plugin.yaml"),
        "Should reject unknown fields: {}",
        err
    );
}

// ---------------------------------------------------------------------------
// Remove — verifies directory cleanup end-to-end
// ---------------------------------------------------------------------------

#[test]
fn test_remove_plugin_cleans_up_directory_structure() {
    use aiki::plugins::git::remove_plugin;

    let tmp = TempDir::new().unwrap();
    let plugin: PluginRef = "myns/myplugin".parse().unwrap();
    let dir = plugin.install_dir(tmp.path());

    // Create a fully installed plugin with various files
    fs::create_dir_all(dir.join(".git")).unwrap();
    fs::write(dir.join("plugin.yaml"), "name: Test Plugin\n").unwrap();
    fs::write(dir.join("hooks.yaml"), "name: test\n").unwrap();
    let tasks_dir = dir.join("tasks");
    fs::create_dir_all(&tasks_dir).unwrap();
    fs::write(tasks_dir.join("review.md"), "# Review\n").unwrap();

    // Remove it
    let result = remove_plugin(&plugin, tmp.path());
    assert!(result.is_ok());

    // Plugin dir and all contents should be gone
    assert!(!dir.exists());
    // Namespace dir should be cleaned up (empty after removal)
    assert!(!tmp.path().join("myns").exists());
}

// ---------------------------------------------------------------------------
// Auto-fetch: install() report and lock-file tests
// ---------------------------------------------------------------------------

#[test]
fn test_install_already_installed_reports_correctly() {
    use aiki::plugins::deps::install;

    let tmp = TempDir::new().unwrap();
    let base = tmp.path();

    // Create a fully valid installed plugin
    let dir = base.join("ns").join("root");
    fs::create_dir_all(dir.join(".git")).unwrap();
    fs::write(dir.join("plugin.yaml"), "name: Root Plugin\n").unwrap();

    let root: PluginRef = "ns/root".parse().unwrap();
    let report = install(&root, base, None, None).unwrap();

    assert_eq!(report.already_installed.len(), 1);
    assert!(report.installed.is_empty());
    assert!(report.failed.is_empty());
    assert!(report.rolled_back.is_empty());
}

#[test]
fn test_install_lock_not_updated_when_dep_fails() {
    use aiki::plugins::deps::install;

    let tmp = TempDir::new().unwrap();
    let project_root = tmp.path().join("project");
    fs::create_dir_all(project_root.join(".aiki")).unwrap();

    let plugins_base = tmp.path().join("plugins");
    // Root is installed with a dep that will fail to clone
    let root_dir = plugins_base.join("ns").join("root");
    fs::create_dir_all(root_dir.join(".git")).unwrap();
    fs::write(root_dir.join("plugin.yaml"), "name: Root\n").unwrap();
    fs::write(
        root_dir.join("hooks.yaml"),
        "hook0:\n  template: ns/missing/tpl\n",
    )
    .unwrap();

    let root: PluginRef = "ns/root".parse().unwrap();
    let report = install(&root, &plugins_base, Some(&project_root), None).unwrap();

    assert!(!report.failed.is_empty(), "Dep should have failed");
    // Lock file should not be written
    let lock_path = project_root.join(".aiki/plugins.lock");
    assert!(
        !lock_path.exists(),
        "Lock file should not be created when a dependency fails"
    );
}

#[test]
fn test_install_chain_all_installed_preserves_existing_lock() {
    use aiki::plugins::deps::install;
    use aiki::plugins::lock::{PluginLock, PluginLockEntry};

    let tmp = TempDir::new().unwrap();
    let project_root = tmp.path().join("project");
    fs::create_dir_all(project_root.join(".aiki")).unwrap();

    // Write a pre-existing lock entry
    let mut lock = PluginLock::default();
    let existing: PluginRef = "other/plugin".parse().unwrap();
    lock.insert(
        &existing,
        PluginLockEntry {
            sha: "a".repeat(40),
            source: "https://github.com/other/plugin.git".to_string(),
            resolved: "2026-01-01T00:00:00Z".to_string(),
        },
    );
    lock.save(&project_root).unwrap();

    let plugins_base = tmp.path().join("plugins");
    // Root → A, both installed
    let root_dir = plugins_base.join("ns").join("root");
    fs::create_dir_all(root_dir.join(".git")).unwrap();
    fs::write(root_dir.join("plugin.yaml"), "name: Root\n").unwrap();
    fs::write(
        root_dir.join("hooks.yaml"),
        "hook0:\n  template: ns/dep/tpl\n",
    )
    .unwrap();
    let dep_dir = plugins_base.join("ns").join("dep");
    fs::create_dir_all(dep_dir.join(".git")).unwrap();
    fs::write(dep_dir.join("plugin.yaml"), "name: Dep\n").unwrap();

    let root: PluginRef = "ns/root".parse().unwrap();
    let report = install(&root, &plugins_base, Some(&project_root), None).unwrap();

    assert!(report.failed.is_empty());
    // Lock file should still have the pre-existing entry
    let reloaded = PluginLock::load(&project_root).unwrap();
    assert!(reloaded.get(&existing).is_some());
}

// ---------------------------------------------------------------------------
// Template resolver: auto-fetch tests
// ---------------------------------------------------------------------------

#[test]
fn test_template_three_part_ref_auto_fetch_failure_returns_error() {
    use aiki::tasks::templates::resolver::load_template;

    let tmp = TempDir::new().unwrap();
    let aiki_home = TempDir::new().unwrap();

    let project_templates = tmp.path().join(".aiki").join("tasks");
    fs::create_dir_all(&project_templates).unwrap();

    // No plugin installed, no project override
    with_temp_aiki_home(aiki_home.path(), || {
        let result = load_template("fakeorg/fakeplugin/review", &project_templates);
        assert!(result.is_err(), "Should fail when plugin not installed");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("fakeorg/fakeplugin"),
            "Error should mention the plugin: {}",
            msg
        );
    });
}

#[test]
fn test_template_project_override_wins_over_plugin() {
    use aiki::tasks::templates::resolver::load_template;

    let tmp = TempDir::new().unwrap();
    let aiki_home = TempDir::new().unwrap();

    let project_templates = tmp.path().join(".aiki").join("tasks");

    // Create project-level template
    let project_tpl = project_templates.join("myns").join("myplugin");
    fs::create_dir_all(&project_tpl).unwrap();
    fs::write(
        project_tpl.join("task.md"),
        "---\nname: Project Version\n---\n# Project\n",
    )
    .unwrap();

    // Create plugin-level template
    let plugin_tasks = aiki_home
        .path()
        .join("plugins")
        .join("myns")
        .join("myplugin")
        .join("tasks");
    fs::create_dir_all(&plugin_tasks).unwrap();
    fs::create_dir_all(
        aiki_home
            .path()
            .join("plugins")
            .join("myns")
            .join("myplugin")
            .join(".git"),
    )
    .unwrap();
    fs::write(
        plugin_tasks.join("task.md"),
        "---\nname: Plugin Version\n---\n# Plugin\n",
    )
    .unwrap();

    with_temp_aiki_home(aiki_home.path(), || {
        let result = load_template("myns/myplugin/task", &project_templates);
        assert!(result.is_ok(), "Should resolve: {:?}", result);

        let template = result.unwrap();
        let source = template.source_path.unwrap_or_default();
        assert!(
            source.starts_with(tmp.path().to_str().unwrap()),
            "Project override should win. source_path: {}",
            source
        );
    });
}

#[test]
fn test_template_missing_plugin_returns_clear_error() {
    use aiki::tasks::templates::resolver::load_template;

    let tmp = TempDir::new().unwrap();
    let aiki_home = TempDir::new().unwrap();

    let project_templates = tmp.path().join(".aiki").join("tasks");
    fs::create_dir_all(&project_templates).unwrap();

    with_temp_aiki_home(aiki_home.path(), || {
        let result = load_template("nonexist/plugin/template", &project_templates);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("not found"),
            "Error should indicate not found: {}",
            msg
        );
    });
}

// ---------------------------------------------------------------------------
// Skip-with-warning: loader behavior during hook resolution
// ---------------------------------------------------------------------------

#[test]
fn test_hook_loader_auto_fetch_failure_returns_auto_fetch_error() {
    use aiki::error::AikiError;
    use aiki::flows::loader::HookLoader;

    let temp_dir = TempDir::new().unwrap();
    let aiki_home = TempDir::new().unwrap();

    // Create minimal project structure
    fs::create_dir_all(temp_dir.path().join(".aiki/hooks")).unwrap();

    with_temp_aiki_home(aiki_home.path(), || {
        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();

        // Loading a non-existent namespaced hook should return AutoFetchFailed
        let result = loader.load("fake/nonexistent");
        assert!(
            matches!(result, Err(AikiError::AutoFetchFailed { .. })),
            "Should return AutoFetchFailed for missing plugin, got: {:?}",
            result
        );
    });
}

#[test]
fn test_hook_loader_continues_after_auto_fetch_failure() {
    use aiki::flows::loader::HookLoader;

    let temp_dir = TempDir::new().unwrap();
    let aiki_home = TempDir::new().unwrap();

    // Create project with one real hook
    fs::create_dir_all(temp_dir.path().join(".aiki/hooks/aiki")).unwrap();
    let real_hook = temp_dir.path().join(".aiki/hooks/aiki/real.yml");
    fs::write(
        &real_hook,
        "name: Real Hook\nversion: '1'\nchange.completed:\n  - log: done\n",
    )
    .unwrap();

    with_temp_aiki_home(aiki_home.path(), || {
        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();

        // First: auto-fetch failure
        let result1 = loader.load("fake/missing");
        assert!(result1.is_err(), "Auto-fetch should fail for missing plugin");

        // Second: real hook should still be loadable
        let result2 = loader.load("aiki/real");
        assert!(
            result2.is_ok(),
            "Loader should continue working after auto-fetch failure: {:?}",
            result2
        );
    });
}

