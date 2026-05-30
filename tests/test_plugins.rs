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
    let tpl_dir = aiki_dir.join("templates");
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
    let tpl_dir = aiki_dir.join("templates");
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
    let tpl_dir = aiki_dir.join("templates");
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
    let tpl_dir = aiki_dir.join("templates");
    fs::create_dir_all(&tpl_dir).unwrap();

    fs::write(tpl_dir.join("a.md"), "{{> myself/plugin/tpl}}\n{{> other/plugin/tpl}}\n").unwrap();

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
    assert_eq!(check_install_status(&plugin, base), InstallStatus::NotInstalled);

    // Create directory without .git/ → partial
    let dir = plugin.install_dir(base);
    fs::create_dir_all(&dir).unwrap();
    assert_eq!(check_install_status(&plugin, base), InstallStatus::PartialInstall);

    // Add .git/ → installed
    fs::create_dir_all(dir.join(".git")).unwrap();
    assert_eq!(check_install_status(&plugin, base), InstallStatus::Installed);
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

    // Create project .aiki/templates/ (empty — no override)
    let project_templates = tmp.path().join(".aiki").join("templates");
    fs::create_dir_all(&project_templates).unwrap();

    // Create a fake plugin with a template under the temp AIKI_HOME
    let plugins_base = aiki_home.path().join("plugins");
    let plugin_tpl_dir = plugins_base.join("testns").join("testplug").join("templates");
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
    let project_templates = tmp.path().join(".aiki").join("templates");

    let plugins_base = aiki_home.path().join("plugins");

    // Create plugin template
    let plugin_tpl_dir = plugins_base.join("testns2").join("overplug").join("templates");
    fs::create_dir_all(&plugin_tpl_dir).unwrap();
    fs::create_dir_all(plugins_base.join("testns2").join("overplug").join(".git")).unwrap();
    fs::write(
        plugin_tpl_dir.join("task.md"),
        "---\nname: Plugin Version\n---\n# Plugin version\n",
    )
    .unwrap();

    // Create project override at .aiki/templates/testns2/overplug/task.md
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

/// Outside-repo `plugin list` shows all cycle-member plugins as top-level.
///
/// When every installed plugin is a dependency of another (full cycle), no plugin
/// qualifies as a "root", so the cycle-aware logic should treat *all* of them as
/// top-level entries rather than hiding them.
#[test]
fn test_plugin_list_outside_repo_cycle_shows_all_top_level() {
    use assert_cmd::prelude::*;
    use predicates::prelude::*;
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

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("ns/alpha"))
        .stdout(predicate::str::contains("ns/beta"));
}

/// Outside-repo `plugin list` hides deps of root plugins.
///
/// When there is a clear root (not a dependency of anyone), its deps should be
/// shown indented under it, not as top-level entries.
#[test]
fn test_plugin_list_outside_repo_hides_deps_of_roots() {
    use assert_cmd::prelude::*;
    use predicates::prelude::*;
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

    // root should appear as top-level, dep should appear indented (as dependency)
    let output = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    // root is top-level (preceded by two spaces)
    assert!(stdout.contains("  ns/root"), "root should be top-level: {}", stdout);
    // dep should appear indented as a dependency, not as top-level
    assert!(
        stdout.contains("(dependency)"),
        "dep should be shown as dependency: {}",
        stdout
    );
    // dep should NOT appear as a top-level entry (only as a dependency)
    let top_level_dep = stdout.lines().any(|line| {
        line.starts_with("  ns/dep") && !line.contains("(dependency)")
    });
    assert!(
        !top_level_dep,
        "dep should not appear as top-level entry: {}",
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
