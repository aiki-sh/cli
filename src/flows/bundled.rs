use std::collections::HashMap;
use std::sync::LazyLock;

use anyhow::Result;

use super::parser::HookParser;
use super::types::Hook;

/// Registry of built-in plugins embedded in the binary.
/// These serve as fallbacks when no file override exists on disk.
static BUILTIN_PLUGINS: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    m.insert("aiki/default", include_str!("plugins/aiki_default.yaml"));
    m.insert(
        "aiki/git-coauthors",
        include_str!("plugins/aiki_git_coauthors.yaml"),
    );
    m.insert(
        "aiki/review-loop",
        include_str!("plugins/aiki_review_loop.yaml"),
    );
    m
});

/// Look up a built-in plugin by its include path (e.g. "aiki/default").
/// Returns the parsed Hook if found in the registry, None otherwise.
pub fn load_builtin_plugin(name: &str) -> Option<Result<Hook>> {
    BUILTIN_PLUGINS
        .get(name)
        .map(|yaml| HookParser::parse_str(yaml))
}

/// Returns `true` if `name` (an `ns/name` include path, e.g. `aiki/default`)
/// names a built-in plugin.
///
/// First-party reference ergonomics use this so that built-ins — which are
/// bundled, not GitHub repos following the `aiki-plugin-` convention — are
/// never rewritten with the boilerplate prefix.
#[must_use]
pub fn is_builtin(name: &str) -> bool {
    BUILTIN_PLUGINS.contains_key(name)
}

/// Load the core system hook (uncached).
///
/// The core hook is embedded in the binary and handles all event types.
/// This function parses the YAML on every call. For cached access, use
/// `crate::cache::get_core_hook()` instead.
///
/// # Note
///
/// This is the uncached version used internally by the cache module.
/// Most code should use `load_core_hook()` which returns a cached reference.
pub fn load_core_hook_uncached() -> Result<Hook> {
    let core_yaml = include_str!("core/hooks.yaml");
    HookParser::parse_str(core_yaml)
}

/// Load the core system flow (cached).
///
/// Returns a reference to the cached core hook that is parsed once per process.
/// This is the preferred entry point for accessing the core hook.
#[must_use]
pub fn load_core_hook() -> &'static Hook {
    crate::cache::get_core_hook()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flows::types::HookStatement;

    #[test]
    fn test_load_core_hook_uncached() {
        // Test the uncached version directly
        let core = load_core_hook_uncached().unwrap();
        assert_eq!(core.name, "Aiki Core");
    }

    #[test]
    fn test_load_core_hook_cached() {
        // Test the cached version
        let core = load_core_hook();
        assert_eq!(core.name, "Aiki Core");
    }

    #[test]
    fn test_core_hook_has_start() {
        let core = load_core_hook();

        // Should have session.started handler
        assert!(!core.handlers.session_started.is_empty());
    }

    #[test]
    fn test_core_hook_has_change_completed() {
        let core = load_core_hook();

        // Should have change.completed handler
        assert!(!core.handlers.change_completed.is_empty());
    }

    #[test]
    fn test_core_hook_has_change_permission_asked() {
        let core = load_core_hook();

        // Should have change.permission_asked handler
        assert!(!core.handlers.change_permission_asked.is_empty());
    }

    #[test]
    fn test_core_hook_metadata() {
        let core = load_core_hook();

        assert_eq!(core.name, "Aiki Core");
        assert_eq!(core.version, "1");
    }

    #[test]
    fn test_core_hook_uses_let_syntax() {
        let core = load_core_hook();

        // Should have change.completed handler
        assert!(!core.handlers.change_completed.is_empty());

        // First statement is the write operation check (change.completed handles all operations)
        match &core.handlers.change_completed[0] {
            HookStatement::If(write_if) => {
                // Verify checks for write operation
                assert!(
                    write_if.condition.contains("event.write"),
                    "Flow should check for write operation"
                );
                // First statement in then block should be the classify_edits check
                assert!(!write_if.then.is_empty());
                match &write_if.then[0] {
                    HookStatement::If(classify_if) => {
                        // Verify uses inline self.classify_edits_change for condition
                        assert!(
                            classify_if.condition.contains("self.classify_edits_change"),
                            "Flow should use inline 'self.classify_edits_change' in condition"
                        );
                    }
                    _ => panic!("Expected If statement for classify_edits_change"),
                }
            }
            _ => panic!("Expected If statement for write operation check"),
        }
    }

    #[test]
    fn test_load_builtin_plugin_exists() {
        let result = load_builtin_plugin("aiki/default");
        assert!(result.is_some());
        let hook = result.unwrap().unwrap();
        assert_eq!(hook.name, "aiki/default");
    }

    #[test]
    fn test_load_builtin_plugin_git_coauthors() {
        let result = load_builtin_plugin("aiki/git-coauthors");
        assert!(result.is_some());
        let hook = result.unwrap().unwrap();
        assert_eq!(hook.name, "aiki/git-coauthors");
    }

    #[test]
    fn test_load_builtin_plugin_review_loop() {
        let result = load_builtin_plugin("aiki/review-loop");
        assert!(result.is_some());
        let hook = result.unwrap().unwrap();
        assert_eq!(hook.name, "aiki/review-loop");
    }

    #[test]
    fn test_load_builtin_plugin_not_found() {
        let result = load_builtin_plugin("nonexistent/plugin");
        assert!(result.is_none());
    }
}
