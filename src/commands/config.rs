//! `aiki config` CLI command
//!
//! Subcommands: get, set, unset, file

use clap::Subcommand;
use std::env;
use std::path::Path;

use crate::error::{AikiError, Result};
use crate::repos::RepoDetector;
use crate::settings;

#[derive(Subcommand)]
#[command(disable_help_subcommand = true)]
pub enum ConfigCommands {
    /// Print the merged value of a config key
    Get {
        /// Dot-separated config key (e.g. plan.dir)
        key: String,
    },
    /// Set a config value
    Set {
        /// Dot-separated config key (e.g. plan.dir)
        key: String,
        /// Value to set
        value: String,
        /// Write to global (~/.aiki/config.yaml) instead of repo config
        #[arg(long)]
        global: bool,
        /// Show what would change without writing
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove a config key
    Unset {
        /// Dot-separated config key (e.g. plan.dir)
        key: String,
        /// Remove from global config instead of repo config
        #[arg(long)]
        global: bool,
        /// Show what would change without writing
        #[arg(long)]
        dry_run: bool,
    },
    /// Dump merged config as YAML
    File,
}

pub fn run(command: ConfigCommands) -> Result<()> {
    let resolve_repo_root = || -> Result<std::path::PathBuf> {
        let cwd = env::current_dir().map_err(|_| {
            AikiError::InvalidArgument("Failed to get current directory".to_string())
        })?;
        RepoDetector::new(&cwd)
            .find_aiki_root()
            .map_err(|e| AikiError::InvalidArgument(e.to_string()))
    };

    match command {
        ConfigCommands::Get { key } => {
            let root = resolve_repo_root()?;
            cmd_get(&root, &key)
        }
        ConfigCommands::Set {
            key,
            value,
            global: true,
            dry_run,
        } => cmd_set(Path::new(""), &key, &value, true, dry_run),
        ConfigCommands::Set {
            key,
            value,
            global: false,
            dry_run,
        } => cmd_set(&resolve_repo_root()?, &key, &value, false, dry_run),
        ConfigCommands::Unset {
            key,
            global: true,
            dry_run,
        } => cmd_unset(Path::new(""), &key, true, dry_run),
        ConfigCommands::Unset {
            key,
            global: false,
            dry_run,
        } => cmd_unset(&resolve_repo_root()?, &key, false, dry_run),
        ConfigCommands::File => {
            let root = resolve_repo_root()?;
            cmd_file(&root)
        }
    }
}

fn cmd_get(cwd: &Path, key: &str) -> Result<()> {
    if !settings::is_known_key(key) {
        return Err(AikiError::InvalidArgument(format!(
            "Unknown config key: {key}"
        )));
    }

    let cfg = settings::Config::load(cwd)?;
    let doc = serde_yaml::to_value(&cfg)
        .map_err(|e| AikiError::InvalidArgument(format!("Failed to serialize config: {e}")))?;

    match settings::get_dot_path(&doc, key) {
        Some(v) => {
            println!("{}", settings::value_to_display_string(&v));
            Ok(())
        }
        None => {
            println!("");
            Ok(())
        }
    }
}

fn cmd_set(cwd: &Path, key: &str, value: &str, global: bool, dry_run: bool) -> Result<()> {
    let validated = settings::validate_value(key, value)?;

    let target_path = if global {
        settings::global_config_path()
    } else {
        let repo_aiki = cwd.join(".aiki");
        if !repo_aiki.is_dir() {
            return Err(AikiError::InvalidArgument(
                "Run 'aiki init' first to initialize this repository.".to_string(),
            ));
        }
        settings::repo_config_path(cwd)
    };

    // Load existing file content (or empty mapping)
    let contents = std::fs::read_to_string(&target_path).unwrap_or_default();
    let mut doc: serde_yaml::Value = if contents.is_empty() {
        serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
    } else {
        serde_yaml::from_str(&contents).map_err(|e| {
            AikiError::InvalidArgument(format!("Malformed YAML in {}: {e}", target_path.display()))
        })?
    };

    settings::set_dot_path(&mut doc, key, validated);

    if dry_run {
        println!("Would write to {}:", target_path.display());
        let yaml = serde_yaml::to_string(&doc).unwrap_or_default();
        print!("{yaml}");
        return Ok(());
    }

    // Ensure parent directory exists
    if let Some(parent) = target_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            AikiError::InvalidArgument(format!("Cannot create directory {}: {e}", parent.display()))
        })?;
    }

    let yaml = serde_yaml::to_string(&doc).unwrap_or_default();
    std::fs::write(&target_path, &yaml).map_err(|e| {
        AikiError::InvalidArgument(format!("Cannot write {}: {e}", target_path.display()))
    })?;

    Ok(())
}

fn cmd_unset(cwd: &Path, key: &str, global: bool, dry_run: bool) -> Result<()> {
    if !settings::is_known_key(key) {
        return Err(AikiError::InvalidArgument(format!(
            "Unknown config key: {key}"
        )));
    }

    let target_path = if global {
        settings::global_config_path()
    } else {
        let repo_aiki = cwd.join(".aiki");
        if !repo_aiki.is_dir() {
            return Err(AikiError::InvalidArgument(
                "Run 'aiki init' first to initialize this repository.".to_string(),
            ));
        }
        settings::repo_config_path(cwd)
    };

    let contents = match std::fs::read_to_string(&target_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(AikiError::InvalidArgument(format!(
                "Cannot read {}: {e}",
                target_path.display()
            )))
        }
    };

    let mut doc: serde_yaml::Value = serde_yaml::from_str(&contents).map_err(|e| {
        AikiError::InvalidArgument(format!("Malformed YAML in {}: {e}", target_path.display()))
    })?;

    let removed = settings::unset_dot_path(&mut doc, key);

    if dry_run {
        if removed {
            println!("Would remove '{key}' from {}:", target_path.display());
            let yaml = serde_yaml::to_string(&doc).unwrap_or_default();
            print!("{yaml}");
        } else {
            println!("Key '{key}' not found in {}", target_path.display());
        }
        return Ok(());
    }

    if removed {
        let yaml = serde_yaml::to_string(&doc).unwrap_or_default();
        std::fs::write(&target_path, &yaml).map_err(|e| {
            AikiError::InvalidArgument(format!("Cannot write {}: {e}", target_path.display()))
        })?;
    }

    Ok(())
}

fn cmd_file(cwd: &Path) -> Result<()> {
    let global_path = settings::global_config_path();
    let repo_path = settings::repo_config_path(cwd);

    // Print comment header
    if global_path.exists() {
        println!("# global: {}", global_path.display());
    }
    if repo_path.exists() {
        println!("# repo:   {}", repo_path.display());
    }
    if global_path.exists() || repo_path.exists() {
        println!();
    }

    let cfg = settings::Config::load(cwd)?;
    let doc = serde_yaml::to_value(&cfg)
        .map_err(|e| AikiError::InvalidArgument(format!("Failed to serialize config: {e}")))?;
    let yaml = serde_yaml::to_string(&doc).unwrap_or_default();
    print!("{yaml}");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_roundtrip() {
        let _lock = crate::global::AIKI_HOME_TEST_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let aiki_dir = repo.join(".aiki");
        std::fs::create_dir_all(&aiki_dir).unwrap();

        let global_dir = tmp.path().join("global");
        std::fs::create_dir_all(&global_dir).unwrap();
        std::env::set_var("AIKI_HOME", &global_dir);

        // Set a value in repo config
        cmd_set(&repo, "plan.dir", "specs/active", false, false).unwrap();

        // Verify file was written
        let contents = std::fs::read_to_string(aiki_dir.join("config.yaml")).unwrap();
        assert!(contents.contains("specs/active"), "got: {contents}");

        // Load and verify via Config
        let cfg = settings::Config::load(&repo).unwrap();
        assert_eq!(cfg.plan.dir, std::path::PathBuf::from("specs/active"));

        std::env::remove_var("AIKI_HOME");
    }

    #[test]
    fn set_global_writes_to_global_dir() {
        let _lock = crate::global::AIKI_HOME_TEST_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::tempdir().unwrap();
        let global_dir = tmp.path().join("global");
        std::fs::create_dir_all(&global_dir).unwrap();
        std::env::set_var("AIKI_HOME", &global_dir);

        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();

        cmd_set(&repo, "plan.dir", "global-plans", true, false).unwrap();

        let contents = std::fs::read_to_string(global_dir.join("config.yaml")).unwrap();
        assert!(contents.contains("global-plans"), "got: {contents}");

        std::env::remove_var("AIKI_HOME");
    }

    #[test]
    fn set_requires_init_for_repo() {
        let _lock = crate::global::AIKI_HOME_TEST_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        // No .aiki/ dir

        std::env::set_var("AIKI_HOME", tmp.path().join("global"));
        let err = cmd_set(&repo, "plan.dir", "specs", false, false).unwrap_err();
        assert!(err.to_string().contains("aiki init"), "got: {err}");

        std::env::remove_var("AIKI_HOME");
    }

    #[test]
    fn dry_run_does_not_write() {
        let _lock = crate::global::AIKI_HOME_TEST_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let aiki_dir = repo.join(".aiki");
        std::fs::create_dir_all(&aiki_dir).unwrap();

        std::env::set_var("AIKI_HOME", tmp.path().join("global"));

        cmd_set(&repo, "plan.dir", "specs", false, true).unwrap();

        // File should NOT exist
        assert!(!aiki_dir.join("config.yaml").exists());

        std::env::remove_var("AIKI_HOME");
    }

    #[test]
    fn unset_removes_key() {
        let _lock = crate::global::AIKI_HOME_TEST_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let aiki_dir = repo.join(".aiki");
        std::fs::create_dir_all(&aiki_dir).unwrap();

        let global_dir = tmp.path().join("global");
        std::fs::create_dir_all(&global_dir).unwrap();
        std::env::set_var("AIKI_HOME", &global_dir);

        // Set then unset
        cmd_set(&repo, "plan.dir", "specs", false, false).unwrap();
        cmd_unset(&repo, "plan.dir", false, false).unwrap();

        // Load — should get default
        let cfg = settings::Config::load(&repo).unwrap();
        assert_eq!(cfg.plan.dir, std::path::PathBuf::from("ops/now"));

        std::env::remove_var("AIKI_HOME");
    }

    #[test]
    fn get_unknown_key_errors() {
        let _lock = crate::global::AIKI_HOME_TEST_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("AIKI_HOME", tmp.path().join("global"));

        let err = cmd_get(tmp.path(), "unknown.key").unwrap_err();
        assert!(err.to_string().contains("Unknown config key"), "got: {err}");

        std::env::remove_var("AIKI_HOME");
    }

    #[test]
    fn set_validates_value() {
        let _lock = crate::global::AIKI_HOME_TEST_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let aiki_dir = repo.join(".aiki");
        std::fs::create_dir_all(&aiki_dir).unwrap();

        std::env::set_var("AIKI_HOME", tmp.path().join("global"));

        let err = cmd_set(&repo, "plan.dir", "/absolute", false, false).unwrap_err();
        assert!(err.to_string().contains("relative path"), "got: {err}");

        std::env::remove_var("AIKI_HOME");
    }
}
