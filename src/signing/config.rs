use anyhow::{Context, Result};
use std::fs;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, PartialEq)]
pub enum SigningBackend {
    Gpg,
    Ssh,
    GpgSm,
}

impl std::fmt::Display for SigningBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SigningBackend::Gpg => write!(f, "gpg"),
            SigningBackend::Ssh => write!(f, "ssh"),
            SigningBackend::GpgSm => write!(f, "gpgsm"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SigningConfig {
    pub backend: SigningBackend,
    pub key: String,
    pub behavior: String,
}

/// Detect signing configuration in priority order:
/// 1. Git signing config (if already configured)
/// 2. GPG keys (if available)
/// 3. SSH keys (if available)
pub fn detect_signing_config() -> Result<Option<SigningConfig>> {
    // Priority 1: Check if Git signing is already configured
    if let Some(config) = detect_git_signing_config()? {
        return Ok(Some(config));
    }

    // Priority 2: Check for GPG keys
    if let Some(config) = detect_gpg_keys()? {
        return Ok(Some(config));
    }

    // Priority 3: Check for SSH keys
    if let Some(config) = detect_ssh_keys()? {
        return Ok(Some(config));
    }

    // No keys found
    Ok(None)
}

/// Detect Git signing configuration by reading git config
pub fn detect_git_signing_config() -> Result<Option<SigningConfig>> {
    // Check if git commit signing is enabled
    let signing_enabled = Command::new("git")
        .args(["config", "--get", "commit.gpgsign"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout)
                    .ok()
                    .map(|s| s.trim() == "true")
            } else {
                None
            }
        })
        .unwrap_or(false);

    if !signing_enabled {
        return Ok(None);
    }

    // Get the signing key
    let key = Command::new("git")
        .args(["config", "--get", "user.signingkey"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
            } else {
                None
            }
        });

    if key.is_none() {
        return Ok(None);
    }

    // Get the GPG format (openpgp, ssh, or x509)
    let format = Command::new("git")
        .args(["config", "--get", "gpg.format"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "openpgp".to_string()); // Default to openpgp

    let backend = match format.as_str() {
        "ssh" => SigningBackend::Ssh,
        "x509" => SigningBackend::GpgSm,
        _ => SigningBackend::Gpg, // "openpgp" or default
    };

    Ok(Some(SigningConfig {
        backend,
        key: key.unwrap(),
        behavior: "own".to_string(),
    }))
}

/// Detect GPG keys on the system
pub fn detect_gpg_keys() -> Result<Option<SigningConfig>> {
    // Check if gpg command is available
    let gpg_available = Command::new("gpg")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);

    if !gpg_available {
        return Ok(None);
    }

    // List secret keys
    let output = Command::new("gpg")
        .args(["--list-secret-keys", "--keyid-format", "LONG"])
        .output()
        .context("Failed to list GPG keys")?;

    if !output.status.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse GPG output to find a key
    // Format:
    // sec   rsa4096/4ED556E9729E000F 2025-01-15 [SC]
    // uid                 [ultimate] User Name <user@example.com>
    for line in stdout.lines() {
        if line.starts_with("sec") {
            // Extract key ID from line like "sec   rsa4096/4ED556E9729E000F 2025-01-15 [SC]"
            if let Some(key_part) = line.split_whitespace().nth(1) {
                if let Some(key_id) = key_part.split('/').nth(1) {
                    return Ok(Some(SigningConfig {
                        backend: SigningBackend::Gpg,
                        key: key_id.to_string(),
                        behavior: "own".to_string(),
                    }));
                }
            }
        }
    }

    Ok(None)
}

/// Detect SSH keys on the system
pub fn detect_ssh_keys() -> Result<Option<SigningConfig>> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    let ssh_dir = home.join(".ssh");

    // Check for common SSH key files in priority order
    let key_candidates = vec!["id_ed25519.pub", "id_ecdsa.pub", "id_rsa.pub"];

    for candidate in key_candidates {
        let key_path = ssh_dir.join(candidate);
        if key_path.exists() {
            return Ok(Some(SigningConfig {
                backend: SigningBackend::Ssh,
                key: key_path.to_string_lossy().to_string(),
                behavior: "own".to_string(),
            }));
        }
    }

    Ok(None)
}

/// Verify that a signing key is accessible
pub fn verify_key_accessible(config: &SigningConfig) -> Result<bool> {
    match config.backend {
        SigningBackend::Gpg | SigningBackend::GpgSm => {
            // Verify GPG key exists
            let output = Command::new("gpg")
                .args(["--list-secret-keys", &config.key])
                .output()
                .context("Failed to verify GPG key")?;

            Ok(output.status.success())
        }
        SigningBackend::Ssh => {
            // Verify SSH public key file exists
            Ok(Path::new(&config.key).exists())
        }
    }
}

/// Create SSH allowed-signers file for SSH signing
pub fn create_ssh_allowed_signers(repo_path: &Path, email: &str, pubkey_path: &str) -> Result<()> {
    // Read the public key
    let pubkey_content =
        fs::read_to_string(pubkey_path).context("Failed to read SSH public key")?;
    let pubkey = pubkey_content.trim();

    // Create allowed-signers file content
    let allowed_signers_content = format!("{} {}\n", email, pubkey);

    // Write to .jj/allowed-signers
    let allowed_signers_path = repo_path.join(".jj").join("allowed-signers");
    fs::write(&allowed_signers_path, allowed_signers_content)
        .context("Failed to write allowed-signers file")?;

    Ok(())
}

/// Read signing configuration from JJ repo config
pub fn read_signing_config(repo_path: &Path) -> Result<Option<SigningConfig>> {
    let config_path = repo_path.join(".jj").join("repo").join("config.toml");

    if !config_path.exists() {
        return Ok(None);
    }

    let config_content = fs::read_to_string(&config_path).context("Failed to read JJ config")?;

    let config: toml::Value =
        toml::from_str(&config_content).context("Failed to parse JJ config")?;

    // Check for [signing] section
    if let Some(signing) = config.get("signing") {
        let backend_str = signing
            .get("backend")
            .and_then(|v| v.as_str())
            .unwrap_or("gpg");

        let backend = match backend_str {
            "ssh" => SigningBackend::Ssh,
            "gpgsm" => SigningBackend::GpgSm,
            _ => SigningBackend::Gpg,
        };

        let behavior = signing
            .get("behavior")
            .and_then(|v| v.as_str())
            .unwrap_or("own")
            .to_string();

        // For SSH, key might be in [signing] section or [signing.backends.ssh]
        let key = if backend == SigningBackend::Ssh {
            signing
                .get("key")
                .and_then(|v| v.as_str())
                .or_else(|| {
                    signing
                        .get("backends")
                        .and_then(|b| b.get("ssh"))
                        .and_then(|ssh| ssh.get("key"))
                        .and_then(|v| v.as_str())
                })
                .unwrap_or("")
                .to_string()
        } else {
            // For GPG, the key is usually auto-detected, not in config
            String::new()
        };

        return Ok(Some(SigningConfig {
            backend,
            key,
            behavior,
        }));
    }

    Ok(None)
}

/// Get user email from git config
pub fn get_user_email(repo_path: &Path) -> Result<String> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(["config", "--get", "user.email"])
        .output()
        .context("Failed to get user email from git config")?;

    if !output.status.success() {
        anyhow::bail!("No user.email configured in git config");
    }

    let email = String::from_utf8(output.stdout)
        .context("Invalid UTF-8 in user.email")?
        .trim()
        .to_string();

    Ok(email)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_signing_backend_display() {
        assert_eq!(SigningBackend::Gpg.to_string(), "gpg");
        assert_eq!(SigningBackend::Ssh.to_string(), "ssh");
        assert_eq!(SigningBackend::GpgSm.to_string(), "gpgsm");
    }

    #[test]
    fn test_signing_config_creation() {
        let config = SigningConfig {
            backend: SigningBackend::Gpg,
            key: "4ED556E9729E000F".to_string(),
            behavior: "own".to_string(),
        };

        assert_eq!(config.backend, SigningBackend::Gpg);
        assert_eq!(config.key, "4ED556E9729E000F");
        assert_eq!(config.behavior, "own");
    }

    #[test]
    fn test_create_ssh_allowed_signers() {
        let temp_dir = TempDir::new().unwrap();
        let repo_path = temp_dir.path();

        // Create .jj directory
        fs::create_dir_all(repo_path.join(".jj")).unwrap();

        // Create a fake SSH public key
        let ssh_dir = temp_dir.path().join("ssh");
        fs::create_dir_all(&ssh_dir).unwrap();
        let pubkey_path = ssh_dir.join("test_key.pub");
        let pubkey_content =
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAITest1234567890 test@example.com\n";
        fs::write(&pubkey_path, pubkey_content).unwrap();

        // Create allowed-signers file
        let email = "user@example.com";
        let result = create_ssh_allowed_signers(repo_path, email, pubkey_path.to_str().unwrap());

        assert!(result.is_ok());

        // Verify file was created with correct content
        let allowed_signers_path = repo_path.join(".jj/allowed-signers");
        assert!(allowed_signers_path.exists());

        let content = fs::read_to_string(&allowed_signers_path).unwrap();
        assert!(content.contains("user@example.com"));
        assert!(content.contains("ssh-ed25519"));
        assert!(content.contains("AAAAC3NzaC1lZDI1NTE5AAAAITest1234567890"));
    }

    #[test]
    fn test_read_signing_config_missing_file() {
        let temp_dir = TempDir::new().unwrap();
        let result = read_signing_config(temp_dir.path());

        // Should return Ok(None) when config doesn't exist
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_read_signing_config_gpg() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join(".jj/repo");
        fs::create_dir_all(&config_path).unwrap();

        // Write a config with GPG signing
        let config_content = r#"
[signing]
backend = "gpg"
behavior = "own"
"#;
        fs::write(config_path.join("config.toml"), config_content).unwrap();

        let result = read_signing_config(temp_dir.path());
        assert!(result.is_ok());

        let config = result.unwrap();
        assert!(config.is_some());

        let config = config.unwrap();
        assert_eq!(config.backend, SigningBackend::Gpg);
        assert_eq!(config.behavior, "own");
    }

    #[test]
    fn test_read_signing_config_ssh() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join(".jj/repo");
        fs::create_dir_all(&config_path).unwrap();

        // Write a config with SSH signing
        let config_content = r#"
[signing]
backend = "ssh"
behavior = "own"
key = "~/.ssh/id_ed25519.pub"
"#;
        fs::write(config_path.join("config.toml"), config_content).unwrap();

        let result = read_signing_config(temp_dir.path());
        assert!(result.is_ok());

        let config = result.unwrap();
        assert!(config.is_some());

        let config = config.unwrap();
        assert_eq!(config.backend, SigningBackend::Ssh);
        assert_eq!(config.behavior, "own");
        assert_eq!(config.key, "~/.ssh/id_ed25519.pub");
    }

    #[test]
    fn test_verify_key_accessible_ssh_missing() {
        let config = SigningConfig {
            backend: SigningBackend::Ssh,
            key: "/nonexistent/path/to/key.pub".to_string(),
            behavior: "own".to_string(),
        };

        let result = verify_key_accessible(&config);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), false);
    }

    #[test]
    fn test_verify_key_accessible_ssh_exists() {
        let temp_dir = TempDir::new().unwrap();
        let key_path = temp_dir.path().join("test_key.pub");
        fs::write(&key_path, "ssh-ed25519 AAAA... test@example.com").unwrap();

        let config = SigningConfig {
            backend: SigningBackend::Ssh,
            key: key_path.to_str().unwrap().to_string(),
            behavior: "own".to_string(),
        };

        let result = verify_key_accessible(&config);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), true);
    }
}
