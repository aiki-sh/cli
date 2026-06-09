//! Layered YAML settings for aiki
//!
//! Loads `config.yaml` from two layers:
//! 1. Global: `$AIKI_HOME/config.yaml` (or `~/.aiki/config.yaml`)
//! 2. Repo:   `<repo>/.aiki/config.yaml`
//!
//! Repo values override global values with deep merge (nested mappings are
//! merged recursively; repo wins for leaf values).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_yaml::Value;

use crate::agents::AgentType;
use crate::error::{AikiError, Result};
use crate::global::global_aiki_dir;

// ── Typed config structs ──────────────────────────────────────────────

fn default_plan_dir() -> PathBuf {
    PathBuf::from("ops/now")
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    #[serde(default)]
    pub plan: PlanConfig,
}

impl Default for Config {
    fn default() -> Self {
        serde_yaml::from_str("{}").expect("default config")
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PlanConfig {
    #[serde(default = "default_plan_dir")]
    pub dir: PathBuf,
    #[serde(default)]
    pub agent: Option<AgentType>,
}

impl Default for PlanConfig {
    fn default() -> Self {
        PlanConfig {
            dir: default_plan_dir(),
            agent: None,
        }
    }
}

// ── Loading & merging ─────────────────────────────────────────────────

const CONFIG_FILENAME: &str = "config.yaml";

impl Config {
    /// Load merged config from global + repo layers.
    ///
    /// Missing files are silently ignored. Malformed YAML produces an error
    /// that names the offending file.
    pub fn load(repo_root: &Path) -> Result<Self> {
        let global_path = global_aiki_dir().join(CONFIG_FILENAME);
        let repo_path = repo_root.join(".aiki").join(CONFIG_FILENAME);

        let global_doc = load_yaml_file(&global_path)?;
        let repo_doc = load_yaml_file(&repo_path)?;

        let merged = deep_merge(global_doc, repo_doc);

        let config: Self = serde_yaml::from_value(merged)
            .map_err(|e| AikiError::InvalidArgument(format!("Invalid config: {e}")))?;
        config.validate_plan_dir()?;
        Ok(config)
    }

    /// Validate the `plan.dir` config value.
    ///
    /// Rejects values that are absolute, empty, or contain
    /// parent-directory traversal (`..`).
    pub fn validate_plan_dir(&self) -> Result<()> {
        let dir = &self.plan.dir;
        let dir_str = dir.to_string_lossy();
        if dir_str.is_empty() {
            return Err(AikiError::InvalidArgument(
                "plan.dir must not be empty".to_string(),
            ));
        }
        if dir.is_absolute() {
            return Err(AikiError::InvalidArgument(
                "plan.dir must be a relative path inside the repository".to_string(),
            ));
        }
        for component in dir.components() {
            if let std::path::Component::ParentDir = component {
                return Err(AikiError::InvalidArgument(
                    "plan.dir must be a relative path inside the repository".to_string(),
                ));
            }
        }
        Ok(())
    }
}

/// Load a YAML file, returning `Value::Null` if the file doesn't exist.
fn load_yaml_file(path: &Path) -> Result<Value> {
    match std::fs::read_to_string(path) {
        Ok(contents) => serde_yaml::from_str(&contents).map_err(|e| {
            AikiError::InvalidArgument(format!("Malformed YAML in {}: {e}", path.display()))
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Value::Null),
        Err(e) => Err(AikiError::InvalidArgument(format!(
            "Cannot read {}: {e}",
            path.display()
        ))),
    }
}

/// Deep merge: recursively merge mappings; repo wins for leaf values.
fn deep_merge(global: Value, repo: Value) -> Value {
    match (global, repo) {
        (Value::Mapping(mut g), Value::Mapping(r)) => {
            for (k, v) in r {
                let merged = match g.remove(&k) {
                    Some(existing) => deep_merge(existing, v),
                    None => v,
                };
                g.insert(k, merged);
            }
            Value::Mapping(g)
        }
        (g, Value::Null) => g,
        (_, r) => r,
    }
}

// ── Dot-path helpers (operate on raw serde_yaml::Value) ───────────────

/// Get a value at a dot-separated path (e.g. `"plan.dir"`).
pub fn get_dot_path(doc: &Value, key: &str) -> Option<Value> {
    let parts: Vec<&str> = key.split('.').collect();
    let mut current = doc;
    for part in &parts {
        match current {
            Value::Mapping(m) => {
                current = m.get(Value::String((*part).to_string()))?;
            }
            _ => return None,
        }
    }
    Some(current.clone())
}

/// Set a value at a dot-separated path, creating intermediate mappings as needed.
pub fn set_dot_path(doc: &mut Value, key: &str, value: Value) {
    let parts: Vec<&str> = key.split('.').collect();
    let mut current = doc;

    // Ensure root is a mapping
    if !current.is_mapping() {
        *current = Value::Mapping(serde_yaml::Mapping::new());
    }

    for (i, part) in parts.iter().enumerate() {
        let yaml_key = Value::String((*part).to_string());
        if i == parts.len() - 1 {
            // Last segment: set the value
            current.as_mapping_mut().unwrap().insert(yaml_key, value);
            return;
        }
        // Intermediate: ensure mapping exists
        let map = current.as_mapping_mut().unwrap();
        if !map.contains_key(&yaml_key) {
            map.insert(yaml_key.clone(), Value::Mapping(serde_yaml::Mapping::new()));
        }
        current = map.get_mut(&yaml_key).unwrap();
        if !current.is_mapping() {
            *current = Value::Mapping(serde_yaml::Mapping::new());
        }
    }
}

/// Remove a key at a dot-separated path. Returns true if the key was present.
pub fn unset_dot_path(doc: &mut Value, key: &str) -> bool {
    let parts: Vec<&str> = key.split('.').collect();
    if parts.is_empty() {
        return false;
    }

    // Navigate to the parent of the final key
    let mut current = doc;
    for part in &parts[..parts.len() - 1] {
        match current {
            Value::Mapping(m) => {
                let yaml_key = Value::String((*part).to_string());
                match m.get_mut(&yaml_key) {
                    Some(v) => current = v,
                    None => return false,
                }
            }
            _ => return false,
        }
    }

    // Remove the final key
    let last = Value::String(parts.last().unwrap().to_string());
    match current.as_mapping_mut() {
        Some(m) => m.remove(&last).is_some(),
        None => false,
    }
}

/// Format a `serde_yaml::Value` as a plain string for display.
pub fn value_to_display_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Null => "null".to_string(),
        _ => serde_yaml::to_string(v)
            .unwrap_or_default()
            .trim()
            .to_string(),
    }
}

/// Known config keys and their validation rules.
pub fn is_known_key(key: &str) -> bool {
    matches!(key, "plan.dir" | "plan.agent")
}

/// Validate a value for a given config key.
pub fn validate_value(key: &str, value: &str) -> Result<Value> {
    match key {
        "plan.dir" => {
            if value.is_empty() {
                return Err(AikiError::InvalidArgument(
                    "plan.dir must not be empty".to_string(),
                ));
            }
            let path = Path::new(value);
            if path.is_absolute() {
                return Err(AikiError::InvalidArgument(
                    "plan.dir must be a relative path inside the repository".to_string(),
                ));
            }
            // Check for path traversal
            for component in path.components() {
                if let std::path::Component::ParentDir = component {
                    return Err(AikiError::InvalidArgument(
                        "plan.dir must be a relative path inside the repository".to_string(),
                    ));
                }
            }
            Ok(Value::String(value.to_string()))
        }
        "plan.agent" => {
            let agent = AgentType::from_str(value).ok_or_else(|| {
                AikiError::InvalidArgument(format!(
                    "Invalid agent type: '{value}'. Supported: claude-code, codex, cursor, gemini"
                ))
            })?;
            // Serialize to get the canonical kebab-case form serde expects on load
            let canonical = serde_yaml::to_value(&agent).map_err(|e| {
                AikiError::InvalidArgument(format!("Failed to serialize agent: {e}"))
            })?;
            Ok(canonical)
        }
        _ => Err(AikiError::InvalidArgument(format!(
            "Unknown config key: {key}"
        ))),
    }
}

// ── Config file paths ─────────────────────────────────────────────────

/// Return the path to the global config file.
pub fn global_config_path() -> PathBuf {
    global_aiki_dir().join(CONFIG_FILENAME)
}

/// Return the path to the repo config file.
pub fn repo_config_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".aiki").join(CONFIG_FILENAME)
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvGuard {
        name: String,
        original: Option<String>,
    }

    impl EnvGuard {
        fn set(
            name: &str,
            value: impl AsRef<std::ffi::OsStr>,
            _proof: &std::sync::MutexGuard<'_, ()>,
        ) -> Self {
            let original = std::env::var(name).ok();
            // SAFETY: Thread safety is handled by AIKI_HOME_TEST_MUTEX; the `_proof`
            // parameter guarantees the caller holds the mutex lock.
            unsafe {
                std::env::set_var(name, value);
            }
            Self {
                name: name.to_string(),
                original,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: Thread safety is handled by AIKI_HOME_TEST_MUTEX; these tests
            // run serially under the mutex lock.
            match &self.original {
                Some(v) => unsafe { std::env::set_var(&self.name, v) },
                None => unsafe { std::env::remove_var(&self.name) },
            }
        }
    }

    #[test]
    fn default_config_has_ops_now() {
        let cfg = Config::default();
        assert_eq!(cfg.plan.dir, PathBuf::from("ops/now"));
        assert!(cfg.plan.agent.is_none());
    }

    #[test]
    fn missing_files_produce_defaults() {
        let _lock = crate::global::AIKI_HOME_TEST_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::tempdir().unwrap();
        let _guard = EnvGuard::set("AIKI_HOME", tmp.path().join("global"), &_lock);
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();

        let cfg = Config::load(&repo).unwrap();
        assert_eq!(cfg.plan.dir, PathBuf::from("ops/now"));
    }

    #[test]
    fn malformed_yaml_reports_path() {
        let _lock = crate::global::AIKI_HOME_TEST_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::tempdir().unwrap();
        let _guard = EnvGuard::set("AIKI_HOME", tmp.path().join("global"), &_lock);
        let repo = tmp.path().join("repo");
        let aiki_dir = repo.join(".aiki");
        std::fs::create_dir_all(&aiki_dir).unwrap();
        std::fs::write(aiki_dir.join("config.yaml"), "{{bad yaml").unwrap();

        let err = Config::load(&repo).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Malformed YAML"), "got: {msg}");
        assert!(msg.contains("config.yaml"), "got: {msg}");
    }

    #[test]
    fn repo_overrides_global() {
        let _lock = crate::global::AIKI_HOME_TEST_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::tempdir().unwrap();
        let global_dir = tmp.path().join("global");
        std::fs::create_dir_all(&global_dir).unwrap();
        std::fs::write(
            global_dir.join("config.yaml"),
            "plan:\n  dir: global/plans\n",
        )
        .unwrap();

        let repo = tmp.path().join("repo");
        let aiki_dir = repo.join(".aiki");
        std::fs::create_dir_all(&aiki_dir).unwrap();
        std::fs::write(aiki_dir.join("config.yaml"), "plan:\n  dir: specs/active\n").unwrap();

        let _guard = EnvGuard::set("AIKI_HOME", &global_dir, &_lock);
        let cfg = Config::load(&repo).unwrap();
        assert_eq!(cfg.plan.dir, PathBuf::from("specs/active"));
    }

    #[test]
    fn global_only_works() {
        let _lock = crate::global::AIKI_HOME_TEST_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::tempdir().unwrap();
        let global_dir = tmp.path().join("global");
        std::fs::create_dir_all(&global_dir).unwrap();
        std::fs::write(global_dir.join("config.yaml"), "plan:\n  dir: my-plans\n").unwrap();

        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();

        let _guard = EnvGuard::set("AIKI_HOME", &global_dir, &_lock);
        let cfg = Config::load(&repo).unwrap();
        assert_eq!(cfg.plan.dir, PathBuf::from("my-plans"));
    }

    #[test]
    fn deep_merge_preserves_sibling_keys() {
        let _lock = crate::global::AIKI_HOME_TEST_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::tempdir().unwrap();
        let global_dir = tmp.path().join("global");
        std::fs::create_dir_all(&global_dir).unwrap();
        std::fs::write(
            global_dir.join("config.yaml"),
            "plan:\n  dir: ops/now\n  agent: claude-code\n",
        )
        .unwrap();

        let repo = tmp.path().join("repo");
        let aiki_dir = repo.join(".aiki");
        std::fs::create_dir_all(&aiki_dir).unwrap();
        std::fs::write(aiki_dir.join("config.yaml"), "plan:\n  dir: specs\n").unwrap();

        let _guard = EnvGuard::set("AIKI_HOME", &global_dir, &_lock);
        let cfg = Config::load(&repo).unwrap();
        // Repo overrides dir
        assert_eq!(cfg.plan.dir, PathBuf::from("specs"));
        // Global's agent is preserved
        assert!(cfg.plan.agent.is_some());
    }

    #[test]
    fn deep_merge_non_mapping_override() {
        let global: Value = serde_yaml::from_str("key: global_val").unwrap();
        let repo: Value = serde_yaml::from_str("key: repo_val").unwrap();
        let merged = deep_merge(global, repo);
        assert_eq!(
            get_dot_path(&merged, "key"),
            Some(Value::String("repo_val".into()))
        );
    }

    #[test]
    fn dot_path_get_set_unset() {
        let mut doc = Value::Mapping(serde_yaml::Mapping::new());

        // Set nested value
        set_dot_path(&mut doc, "plan.dir", Value::String("specs/active".into()));
        assert_eq!(
            get_dot_path(&doc, "plan.dir"),
            Some(Value::String("specs/active".into()))
        );

        // Overwrite
        set_dot_path(&mut doc, "plan.dir", Value::String("docs/plans".into()));
        assert_eq!(
            get_dot_path(&doc, "plan.dir"),
            Some(Value::String("docs/plans".into()))
        );

        // Unset
        assert!(unset_dot_path(&mut doc, "plan.dir"));
        assert_eq!(get_dot_path(&doc, "plan.dir"), None);

        // Unset non-existent
        assert!(!unset_dot_path(&mut doc, "plan.dir"));
    }

    #[test]
    fn dot_path_creates_intermediate_mappings() {
        let mut doc = Value::Null;
        set_dot_path(&mut doc, "a.b.c", Value::String("deep".into()));
        assert_eq!(
            get_dot_path(&doc, "a.b.c"),
            Some(Value::String("deep".into()))
        );
    }

    #[test]
    fn validate_plan_dir_rejects_absolute() {
        let err = validate_value("plan.dir", "/absolute/path").unwrap_err();
        assert!(err.to_string().contains("relative path"));
    }

    #[test]
    fn validate_plan_dir_rejects_empty() {
        let err = validate_value("plan.dir", "").unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn validate_plan_dir_rejects_parent_traversal() {
        let err = validate_value("plan.dir", "../outside").unwrap_err();
        assert!(err.to_string().contains("relative path"));
    }

    #[test]
    fn validate_plan_dir_accepts_valid() {
        let val = validate_value("plan.dir", "specs/active").unwrap();
        assert_eq!(val, Value::String("specs/active".into()));
    }

    #[test]
    fn validate_unknown_key_errors() {
        let err = validate_value("unknown.key", "value").unwrap_err();
        assert!(err.to_string().contains("Unknown config key"));
    }

    #[test]
    fn validate_plan_agent_writes_canonical_kebab_case() {
        // "codex" (lowercase CLI input) must produce kebab-case for serde roundtrip
        let val = validate_value("plan.agent", "codex").unwrap();
        assert_eq!(val, Value::String("codex".into()));

        let val = validate_value("plan.agent", "claude-code").unwrap();
        assert_eq!(val, Value::String("claude-code".into()));

        let val = validate_value("plan.agent", "gemini").unwrap();
        assert_eq!(val, Value::String("gemini".into()));

        let val = validate_value("plan.agent", "cursor").unwrap();
        assert_eq!(val, Value::String("cursor".into()));
    }

    #[test]
    fn plan_agent_roundtrip_through_config_load() {
        use crate::agents::AgentType;

        let _lock = crate::global::AIKI_HOME_TEST_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::tempdir().unwrap();
        let _guard = EnvGuard::set("AIKI_HOME", tmp.path().join("global"), &_lock);
        let repo = tmp.path().join("repo");
        let aiki_dir = repo.join(".aiki");
        std::fs::create_dir_all(&aiki_dir).unwrap();

        // Simulate what `aiki config set plan.agent codex` does
        let validated = validate_value("plan.agent", "codex").unwrap();
        let mut doc = Value::Mapping(serde_yaml::Mapping::new());
        set_dot_path(&mut doc, "plan.agent", validated);
        let yaml = serde_yaml::to_string(&doc).unwrap();
        std::fs::write(aiki_dir.join("config.yaml"), &yaml).unwrap();

        // Config::load must deserialize successfully
        let cfg = Config::load(&repo).unwrap();
        assert_eq!(cfg.plan.agent, Some(AgentType::Codex));
    }

    #[test]
    fn plan_agent_roundtrip_legacy_pascal_case() {
        use crate::agents::AgentType;

        let _lock = crate::global::AIKI_HOME_TEST_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::tempdir().unwrap();
        let _guard = EnvGuard::set("AIKI_HOME", tmp.path().join("global"), &_lock);
        let repo = tmp.path().join("repo");
        let aiki_dir = repo.join(".aiki");
        std::fs::create_dir_all(&aiki_dir).unwrap();

        std::fs::write(aiki_dir.join("config.yaml"), "plan:\n  agent: ClaudeCode\n").unwrap();

        let cfg = Config::load(&repo).unwrap();
        assert_eq!(cfg.plan.agent, Some(AgentType::ClaudeCode));
    }

    #[test]
    fn value_to_display_string_formats() {
        assert_eq!(
            value_to_display_string(&Value::String("hello".into())),
            "hello"
        );
        assert_eq!(value_to_display_string(&Value::Bool(true)), "true");
        assert_eq!(value_to_display_string(&Value::Null), "null");
    }

    #[test]
    fn config_load_rejects_absolute_plan_dir() {
        let _lock = crate::global::AIKI_HOME_TEST_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::tempdir().unwrap();
        let _guard = EnvGuard::set("AIKI_HOME", tmp.path().join("global"), &_lock);
        let repo = tmp.path().join("repo");
        let aiki_dir = repo.join(".aiki");
        std::fs::create_dir_all(&aiki_dir).unwrap();
        std::fs::write(aiki_dir.join("config.yaml"), "plan:\n  dir: /tmp/evil\n").unwrap();

        let err = Config::load(&repo).unwrap_err();
        assert!(err.to_string().contains("relative path"), "got: {}", err);
    }

    #[test]
    fn config_load_rejects_traversal_plan_dir() {
        let _lock = crate::global::AIKI_HOME_TEST_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::tempdir().unwrap();
        let _guard = EnvGuard::set("AIKI_HOME", tmp.path().join("global"), &_lock);
        let repo = tmp.path().join("repo");
        let aiki_dir = repo.join(".aiki");
        std::fs::create_dir_all(&aiki_dir).unwrap();
        std::fs::write(
            aiki_dir.join("config.yaml"),
            "plan:\n  dir: ../../outside\n",
        )
        .unwrap();

        let err = Config::load(&repo).unwrap_err();
        assert!(err.to_string().contains("relative path"), "got: {}", err);
    }

    #[test]
    fn config_load_accepts_valid_relative_plan_dir() {
        let _lock = crate::global::AIKI_HOME_TEST_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::tempdir().unwrap();
        let _guard = EnvGuard::set("AIKI_HOME", tmp.path().join("global"), &_lock);
        let repo = tmp.path().join("repo");
        let aiki_dir = repo.join(".aiki");
        std::fs::create_dir_all(&aiki_dir).unwrap();
        std::fs::write(aiki_dir.join("config.yaml"), "plan:\n  dir: specs/active\n").unwrap();

        let cfg = Config::load(&repo).unwrap();
        assert_eq!(cfg.plan.dir, PathBuf::from("specs/active"));
    }
}
