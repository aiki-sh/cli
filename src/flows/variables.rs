use std::collections::HashMap;

use crate::error::AikiError;
use crate::parsing::interpolation::substitute_template;

/// Resolves variables in strings using {{var}} syntax.
/// Also supports JSON field access (e.g., {{metadata.author}} for JSON variables)
pub struct VariableResolver {
    variables: HashMap<String, String>,
    cache_valid: bool,
    // Track which variables contain JSON for field access
    json_variables: HashMap<String, serde_json::Value>,
    // Lazy environment variable lookup function
    // Called on-demand when a {{VAR}} is not found in variables
    env_lookup: Option<Box<dyn Fn(&str) -> Option<String>>>,
    // Lazy variables - computed on first access
    // Uses interior mutability pattern to allow taking the closure
    lazy_variables: HashMap<String, Option<Box<dyn FnOnce() -> String>>>,
}

impl VariableResolver {
    /// Create a new variable resolver
    #[must_use]
    pub fn new() -> Self {
        Self {
            variables: HashMap::new(),
            cache_valid: false,
            json_variables: HashMap::new(),
            env_lookup: None,
            lazy_variables: HashMap::new(),
        }
    }

    /// Set the environment variable lookup function
    ///
    /// This enables lazy per-key lookup of environment variables instead of
    /// eagerly collecting all env vars into a HashMap. This ensures that
    /// runtime `std::env::set_var` / `remove_var` mutations are immediately
    /// visible during hook execution.
    pub fn set_env_lookup<F>(&mut self, lookup: F)
    where
        F: Fn(&str) -> Option<String> + 'static,
    {
        self.env_lookup = Some(Box::new(lookup));
    }

    /// Register a lazy variable that computes its value on first access
    ///
    /// The compute function is called only when the variable is actually
    /// resolved. After computation, the value is cached for subsequent
    /// accesses.
    pub fn add_lazy_var<F>(&mut self, key: impl Into<String>, compute: F)
    where
        F: FnOnce() -> String + 'static,
    {
        self.lazy_variables
            .insert(key.into(), Some(Box::new(compute)));
        self.cache_valid = false;
    }

    /// Add a variable
    pub fn add_var(&mut self, key: impl Into<String>, value: impl Into<String>) {
        let key = key.into();
        let value = value.into();

        // Try to parse as JSON - if successful, store for field access
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&value) {
            if json.is_object() {
                self.json_variables.insert(key.clone(), json);
            }
        }

        self.variables.insert(key, value);
        self.cache_valid = false;
    }

    /// Add environment variables
    #[allow(dead_code)] // Part of VariableResolver API
    pub fn add_env_vars(&mut self, env_vars: &HashMap<String, String>) {
        self.variables
            .extend(env_vars.iter().map(|(k, v)| (k.clone(), v.clone())));
        self.cache_valid = false;
    }

    /// Get all resolved variables as a flat map.
    ///
    /// This resolves lazy variables that haven't been computed yet and returns
    /// a snapshot of all variable key-value pairs. Used for building Rhai scopes.
    pub fn collect_variables(&mut self) -> HashMap<String, String> {
        // Force resolution of all lazy variables
        let lazy_keys: Vec<String> = self
            .lazy_variables
            .keys()
            .filter(|k| {
                self.lazy_variables
                    .get(*k)
                    .map(|v| v.is_some())
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        for key in lazy_keys {
            if let Some(Some(compute)) = self.lazy_variables.remove(&key) {
                let value = compute();
                self.variables.insert(key, value);
            }
        }
        self.variables.clone()
    }

    /// Resolve {{var}} interpolation in a string.
    ///
    /// Use this for YAML fields that contain free-form text where bare strings
    /// should be treated as literals (shell commands, log messages, jj args, etc.).
    ///
    /// Returns `Err(AikiError::VariableNotFound)` for unknown variables inside `{{}}`.
    /// Env-var candidates (ALL_CAPS_DIGITS_UNDERSCORES) get empty-string passthrough.
    pub fn resolve(&mut self, input: &str) -> Result<String, AikiError> {
        // Fast path: no {{ }} markers
        if !input.contains("{{") {
            return Ok(input.to_string());
        }

        match substitute_template(input, |var_name| self.get_variable(var_name)) {
            Ok(result) => Ok(result),
            Err(unknown_vars) => {
                // Env-var candidates get empty-string passthrough
                let non_env_unknowns: Vec<_> = unknown_vars
                    .iter()
                    .filter(|v| !Self::is_env_var_candidate(v))
                    .collect();

                if non_env_unknowns.is_empty() {
                    // All unknowns were env-var candidates — re-substitute with empty
                    let result = substitute_template(input, |var_name| {
                        self.get_variable(var_name).or_else(|| {
                            if Self::is_env_var_candidate(var_name) {
                                Some(String::new())
                            } else {
                                None
                            }
                        })
                    })
                    .unwrap(); // safe: all unknowns now resolve
                    Ok(result)
                } else {
                    let first_unknown = &non_env_unknowns[0];
                    Err(AikiError::VariableNotFound {
                        variable: first_unknown.to_string(),
                        hint: self.generate_hook_error_hint(first_unknown),
                    })
                }
            }
        }
    }

    /// Resolve with bare-name fallback: if the entire input is a single variable
    /// name (no {{}}), look it up directly. Otherwise, fall back to interpolation.
    ///
    /// Use this ONLY for YAML fields that accept a pure variable reference as their
    /// entire value: `with_author`, `let` RHS, `set` values.
    pub fn resolve_or_lookup(&mut self, input: &str) -> Result<String, AikiError> {
        // If the entire input is a bare variable name, try direct lookup first
        if !input.contains('{') && Self::is_variable_name(input) {
            if let Some(value) = self.get_variable(input) {
                return Ok(value);
            }
            // Bare variable not found - error
            return Err(AikiError::VariableNotFound {
                variable: input.to_string(),
                hint: self.generate_hook_error_hint(input),
            });
        }

        // Otherwise, use standard interpolation
        self.resolve(input)
    }

    /// Get variable value (handles regular vars, lazy vars, JSON fields, env vars)
    fn get_variable(&mut self, var_name: &str) -> Option<String> {
        // 1. Check regular variables first
        if let Some(value) = self.variables.get(var_name) {
            return Some(value.clone());
        }

        // 2. Check lazy variables (compute on first access)
        if let Some(Some(compute)) = self.lazy_variables.remove(var_name) {
            let value = compute();
            self.variables.insert(var_name.to_string(), value.clone());
            self.cache_valid = false;
            return Some(value);
        }

        // 3. Check JSON field access (e.g., metadata.author)
        if let Some((base, field)) = var_name.split_once('.') {
            if let Some(json_value) = self.json_variables.get(base) {
                if let Some(field_value) = json_value.get(field) {
                    let value = match field_value {
                        serde_json::Value::String(s) => s.clone(),
                        _ => field_value.to_string().trim_matches('"').to_string(),
                    };
                    return Some(value);
                }
            }
        }

        // 4. Try environment variable lookup (if configured)
        if let Some(ref lookup) = self.env_lookup {
            if let Some(value) = lookup(var_name) {
                return Some(value);
            }
        }

        None
    }

    /// Returns true if the name looks like an environment variable (ALL_CAPS_DIGITS_UNDERSCORES).
    /// Matches `^[A-Z_][A-Z0-9_]*$`
    fn is_env_var_candidate(name: &str) -> bool {
        let mut chars = name.chars();
        match chars.next() {
            Some(first) if first.is_ascii_uppercase() || first == '_' => {
                chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            }
            _ => false,
        }
    }

    fn generate_hook_error_hint(&self, var_name: &str) -> String {
        if var_name.starts_with("event.") {
            "Variable not found in event context. Available: event.file_paths, event.task.id, etc."
                .to_string()
        } else if var_name.starts_with("session.") {
            "Variable not found in session context. Available: session.task.id, session.cwd, etc."
                .to_string()
        } else {
            format!(
                "Variable '{}' not defined. Use 'let: {}=<value>' to define it.",
                var_name, var_name
            )
        }
    }

    /// Check if string looks like a variable name (no spaces, valid identifier)
    fn is_variable_name(s: &str) -> bool {
        if s.is_empty() || s.contains(char::is_whitespace) {
            return false;
        }
        // Must start with letter or underscore
        let first = s.chars().next().unwrap();
        if !first.is_alphabetic() && first != '_' {
            return false;
        }
        // Rest can be alphanumeric, underscore, or dot
        s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.')
    }
}

impl Default for VariableResolver {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_simple_variable() {
        let mut resolver = VariableResolver::new();
        resolver.add_var("name", "test");

        assert_eq!(resolver.resolve("Hello {{name}}").unwrap(), "Hello test");
    }

    #[test]
    fn test_resolve_multiple_variables() {
        let mut resolver = VariableResolver::new();
        resolver.add_var("cwd", "/home/user");
        resolver.add_var("agent", "claude-code");

        assert_eq!(
            resolver
                .resolve("Running in {{cwd}} with {{agent}}")
                .unwrap(),
            "Running in /home/user with claude-code"
        );
    }

    #[test]
    fn test_resolve_same_variable_multiple_times() {
        let mut resolver = VariableResolver::new();
        resolver.add_var("value", "42");

        assert_eq!(
            resolver
                .resolve("{{value}} + {{value}} = answer")
                .unwrap(),
            "42 + 42 = answer"
        );
    }

    #[test]
    fn test_resolve_no_variables() {
        let mut resolver = VariableResolver::new();
        assert_eq!(resolver.resolve("Plain text").unwrap(), "Plain text");
    }

    #[test]
    fn test_resolve_undefined_variable_errors() {
        let mut resolver = VariableResolver::new();
        let err = resolver.resolve("Value: {{undefined}}").unwrap_err();
        assert!(matches!(err, AikiError::VariableNotFound { .. }));
    }

    #[test]
    fn test_resolve_env_vars_via_add() {
        let mut resolver = VariableResolver::new();
        let mut env_vars = HashMap::new();
        env_vars.insert("HOME".to_string(), "/home/user".to_string());
        env_vars.insert("PATH".to_string(), "/usr/bin".to_string());
        resolver.add_env_vars(&env_vars);

        assert_eq!(
            resolver
                .resolve("Home: {{HOME}}, Path: {{PATH}}")
                .unwrap(),
            "Home: /home/user, Path: /usr/bin"
        );
    }

    #[test]
    fn test_resolve_empty_string() {
        let mut resolver = VariableResolver::new();
        assert_eq!(resolver.resolve("").unwrap(), "");
    }

    #[test]
    fn test_resolve_no_braces() {
        let mut resolver = VariableResolver::new();
        // Dollar sign is no longer special - it's just literal text
        assert_eq!(resolver.resolve("Price: $100").unwrap(), "Price: $100");
    }

    #[test]
    fn test_variable_update() {
        let mut resolver = VariableResolver::new();
        resolver.add_var("x", "1");

        assert_eq!(resolver.resolve("{{x}}").unwrap(), "1");

        // Add new variable value
        resolver.add_var("x", "2");

        assert_eq!(resolver.resolve("{{x}}").unwrap(), "2");
    }

    #[test]
    fn test_fast_path_no_braces() {
        let mut resolver = VariableResolver::new();
        resolver.add_var("x", "value");

        // Should return quickly without parsing
        let result = resolver.resolve("no variables here").unwrap();
        assert_eq!(result, "no variables here");
    }

    #[test]
    fn test_json_field_access() {
        let mut resolver = VariableResolver::new();
        resolver.add_var("metadata", r#"{"author":"Claude <noreply@anthropic.com>","description":"[aiki]\nagent=claude\n[/aiki]"}"#);

        assert_eq!(
            resolver.resolve("Author: {{metadata.author}}").unwrap(),
            "Author: Claude <noreply@anthropic.com>"
        );
    }

    #[test]
    fn test_json_field_access_multiple_fields() {
        let mut resolver = VariableResolver::new();
        resolver.add_var(
            "metadata",
            r#"{"author":"Claude","description":"Test description"}"#,
        );

        let result = resolver
            .resolve("{{metadata.author}} wrote: {{metadata.description}}")
            .unwrap();
        assert_eq!(result, "Claude wrote: Test description");
    }

    #[test]
    fn test_json_field_access_with_regular_vars() {
        let mut resolver = VariableResolver::new();
        resolver.add_var("metadata", r#"{"author":"Claude"}"#);
        resolver.add_var("cwd", "/tmp");

        let result = resolver
            .resolve("{{metadata.author}} in {{cwd}}")
            .unwrap();
        assert_eq!(result, "Claude in /tmp");
    }

    #[test]
    fn test_json_field_access_nonexistent_field_errors() {
        let mut resolver = VariableResolver::new();
        resolver.add_var("metadata", r#"{"author":"Claude"}"#);

        // Unknown field should error
        let err = resolver
            .resolve("{{metadata.nonexistent}}")
            .unwrap_err();
        assert!(matches!(err, AikiError::VariableNotFound { .. }));
    }

    #[test]
    fn test_lazy_env_lookup() {
        let mut resolver = VariableResolver::new();

        resolver.set_env_lookup(|name| {
            if name == "TEST_VAR" {
                Some("lazy_value".to_string())
            } else {
                None
            }
        });

        // Should resolve via lazy lookup
        assert_eq!(
            resolver.resolve("Value: {{TEST_VAR}}").unwrap(),
            "Value: lazy_value"
        );
    }

    #[test]
    fn test_env_var_candidate_passthrough() {
        let mut resolver = VariableResolver::new();

        // UNKNOWN_ENV is an env-var candidate (ALL_CAPS) - should get empty string
        assert_eq!(
            resolver.resolve("Value: {{UNKNOWN_ENV}}").unwrap(),
            "Value: "
        );
    }

    #[test]
    fn test_lazy_env_lookup_priority() {
        let mut resolver = VariableResolver::new();

        // Add a regular variable
        resolver.add_var("MY_VAR", "regular_value");

        // Set up lazy lookup that would return different value
        resolver.set_env_lookup(|name| {
            if name == "MY_VAR" {
                Some("env_value".to_string())
            } else {
                None
            }
        });

        // Regular variable should take priority over env lookup
        assert_eq!(
            resolver.resolve("Value: {{MY_VAR}}").unwrap(),
            "Value: regular_value"
        );
    }

    #[test]
    fn test_lazy_env_lookup_with_real_env() {
        use std::env;

        let mut resolver = VariableResolver::new();
        resolver.set_env_lookup(|name| env::var(name).ok());

        // Set a test env var
        env::set_var("AIKI_TEST_LAZY_VAR", "test_value_123");

        // Should resolve
        assert_eq!(
            resolver.resolve("Value: {{AIKI_TEST_LAZY_VAR}}").unwrap(),
            "Value: test_value_123"
        );

        // Clean up
        env::remove_var("AIKI_TEST_LAZY_VAR");

        // After removal, env-var candidate should get empty string
        let mut resolver2 = VariableResolver::new();
        resolver2.set_env_lookup(|name| env::var(name).ok());
        assert_eq!(
            resolver2.resolve("Value: {{AIKI_TEST_LAZY_VAR}}").unwrap(),
            "Value: "
        );
    }

    #[test]
    fn test_lazy_var_not_computed_until_accessed() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let mut resolver = VariableResolver::new();
        let was_called = Arc::new(AtomicBool::new(false));
        let was_called_clone = Arc::clone(&was_called);

        resolver.add_lazy_var("expensive.value", move || {
            was_called_clone.store(true, Ordering::SeqCst);
            "computed_value".to_string()
        });

        // Lazy var not accessed yet - should not be computed
        assert!(!was_called.load(Ordering::SeqCst));

        // Resolve a different variable - lazy var should still not be computed
        let _ = resolver.resolve("no lazy vars here");
        assert!(!was_called.load(Ordering::SeqCst));
    }

    #[test]
    fn test_lazy_var_computed_when_accessed() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let mut resolver = VariableResolver::new();
        let was_called = Arc::new(AtomicBool::new(false));
        let was_called_clone = Arc::clone(&was_called);

        resolver.add_lazy_var("event.task.files", move || {
            was_called_clone.store(true, Ordering::SeqCst);
            "file1.rs file2.rs".to_string()
        });

        // Access the lazy var
        let result = resolver.resolve("Files: {{event.task.files}}").unwrap();
        assert!(was_called.load(Ordering::SeqCst));
        assert_eq!(result, "Files: file1.rs file2.rs");
    }

    #[test]
    fn test_lazy_var_cached_after_first_access() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let mut resolver = VariableResolver::new();
        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_clone = Arc::clone(&call_count);

        resolver.add_lazy_var("event.task.changes", move || {
            call_count_clone.fetch_add(1, Ordering::SeqCst);
            "change1 change2".to_string()
        });

        // First access - should compute
        let result1 = resolver
            .resolve("Changes: {{event.task.changes}}")
            .unwrap();
        assert_eq!(result1, "Changes: change1 change2");
        assert_eq!(call_count.load(Ordering::SeqCst), 1);

        // Second access - should use cached value, not recompute
        let result2 = resolver
            .resolve("Again: {{event.task.changes}}")
            .unwrap();
        assert_eq!(result2, "Again: change1 change2");
        assert_eq!(call_count.load(Ordering::SeqCst), 1); // Still 1, not 2
    }

    #[test]
    fn test_lazy_var_multiple_vars() {
        let mut resolver = VariableResolver::new();

        resolver.add_lazy_var("event.task.files", || "file1.rs file2.rs".to_string());
        resolver.add_lazy_var("event.task.changes", || "abc123 def456".to_string());

        let result = resolver
            .resolve("Files: {{event.task.files}}, Changes: {{event.task.changes}}")
            .unwrap();
        assert_eq!(
            result,
            "Files: file1.rs file2.rs, Changes: abc123 def456"
        );
    }

    #[test]
    fn test_lazy_var_with_regular_vars() {
        let mut resolver = VariableResolver::new();

        resolver.add_var("event.task.id", "task-123");
        resolver.add_var("event.task.name", "My Task");
        resolver.add_lazy_var("event.task.files", || "modified.rs".to_string());

        let result = resolver
            .resolve(
                "Task {{event.task.id}} ({{event.task.name}}): {{event.task.files}}",
            )
            .unwrap();
        assert_eq!(result, "Task task-123 (My Task): modified.rs");
    }

    #[test]
    fn test_lazy_var_undefined_errors() {
        let mut resolver = VariableResolver::new();

        // Don't add the lazy var, just try to resolve it
        let err = resolver
            .resolve("Files: {{event.task.files}}")
            .unwrap_err();
        assert!(matches!(err, AikiError::VariableNotFound { .. }));
    }

    #[test]
    fn test_resolve_or_lookup_bare_name() {
        let mut resolver = VariableResolver::new();
        resolver.add_var("prep.ai_author", "Claude <noreply@anthropic.com>");

        // Bare variable name should be looked up directly
        assert_eq!(
            resolver.resolve_or_lookup("prep.ai_author").unwrap(),
            "Claude <noreply@anthropic.com>"
        );
    }

    #[test]
    fn test_resolve_or_lookup_interpolation() {
        let mut resolver = VariableResolver::new();
        resolver.add_var("prep.name", "Claude");

        // {{var}} syntax should work via interpolation
        assert_eq!(
            resolver.resolve_or_lookup("AI {{prep.name}}").unwrap(),
            "AI Claude"
        );
    }

    #[test]
    fn test_resolve_or_lookup_bare_name_not_found() {
        let mut resolver = VariableResolver::new();

        let err = resolver.resolve_or_lookup("unknown_var").unwrap_err();
        assert!(matches!(err, AikiError::VariableNotFound { .. }));
    }

    #[test]
    fn test_is_env_var_candidate() {
        assert!(VariableResolver::is_env_var_candidate("HOME"));
        assert!(VariableResolver::is_env_var_candidate("HTTP2_PORT"));
        assert!(VariableResolver::is_env_var_candidate("_PRIVATE"));
        assert!(!VariableResolver::is_env_var_candidate("event.task.id"));
        assert!(!VariableResolver::is_env_var_candidate("myVar"));
        assert!(!VariableResolver::is_env_var_candidate(""));
    }

    #[test]
    fn test_is_variable_name() {
        assert!(VariableResolver::is_variable_name("prep.ai_author"));
        assert!(VariableResolver::is_variable_name("event.task.id"));
        assert!(VariableResolver::is_variable_name("my_var"));
        assert!(!VariableResolver::is_variable_name("echo hello"));
        assert!(!VariableResolver::is_variable_name(""));
        assert!(!VariableResolver::is_variable_name("123abc"));
    }
}
