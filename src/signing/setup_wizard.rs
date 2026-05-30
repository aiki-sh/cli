use anyhow::{Context, Result};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config;
use crate::signing::config::{self as signing_config, SigningBackend, SigningConfig};

pub struct SignSetupWizard {
    repo_path: PathBuf,
}

pub enum SetupMode {
    Generate(SigningBackend),
    Manual {
        backend: SigningBackend,
        key: String,
    },
}

impl SignSetupWizard {
    #[must_use]
    pub fn new(repo_path: impl AsRef<Path>) -> Self {
        Self {
            repo_path: repo_path.as_ref().to_path_buf(),
        }
    }

    /// Main entry point for the wizard
    pub fn run(&self, mode: Option<SetupMode>) -> Result<()> {
        println!();
        println!("Welcome to Aiki Signing Setup");
        println!("==============================");
        println!();
        println!("Commit signing provides cryptographic proof that AI-attributed");
        println!("changes haven't been tampered with.");
        println!();

        // Determine setup mode
        let setup_mode = match mode {
            Some(m) => m,
            None => self.prompt_setup_choice()?,
        };

        // Execute setup based on mode
        let config = match setup_mode {
            SetupMode::Generate(SigningBackend::Gpg) => self.generate_gpg_key()?,
            SetupMode::Generate(SigningBackend::Ssh) => self.generate_ssh_key()?,
            SetupMode::Generate(SigningBackend::GpgSm) => {
                anyhow::bail!("GPG-SM key generation not yet supported. Use --key to specify an existing key.");
            }
            SetupMode::Manual { backend, key } => self.configure_manual_key(backend, key)?,
        };

        // Verify the key is accessible
        self.verify_key(&config)?;

        // Apply configuration to JJ
        self.apply_config(&config)?;

        // Optionally update git config
        self.offer_git_config_update(&config)?;

        // Show success message
        self.show_success_message(&config);

        Ok(())
    }

    /// Prompt user for setup choice
    fn prompt_setup_choice(&self) -> Result<SetupMode> {
        println!("Which signing method would you like to use?");
        println!();
        println!("  1. GPG (recommended for maximum compatibility)");
        println!("  2. SSH (simpler, requires JJ 0.12+)");
        println!();

        let choice = prompt_choice("Choice", 1, 2)?;

        let backend = match choice {
            1 => SigningBackend::Gpg,
            2 => SigningBackend::Ssh,
            _ => unreachable!(),
        };

        Ok(SetupMode::Generate(backend))
    }

    /// Generate a GPG key
    fn generate_gpg_key(&self) -> Result<SigningConfig> {
        println!("GPG Key Generation");
        println!("==================");
        println!();

        // Get user details from git config or prompt
        let (name, email) = get_user_details_from_git().or_else(|_| {
            println!("Please provide your details for the GPG key:");
            let name = prompt_string("Full name", None)?;
            let email = prompt_string("Email address", None)?;
            Ok::<_, anyhow::Error>((name, email))
        })?;

        println!();
        println!("Generating GPG key... (this may take 30-60 seconds)");
        println!();

        // Generate key
        let key_id = generate_gpg_key_with_details(&name, &email)?;

        println!("✓ GPG key created: {}", key_id);
        println!();

        Ok(SigningConfig {
            backend: SigningBackend::Gpg,
            key: key_id,
            behavior: "own".to_string(),
        })
    }

    /// Generate an SSH key
    fn generate_ssh_key(&self) -> Result<SigningConfig> {
        println!("SSH Key Generation");
        println!("==================");
        println!();

        // Get email from git config or prompt
        let email = Command::new("git")
            .args(["config", "--get", "user.email"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| {
                prompt_string("Email address", Some("aiki@local"))
                    .unwrap_or_else(|_| "aiki@local".to_string())
            });

        // Key path
        let home = dirs::home_dir().context("Could not find home directory")?;
        let key_path = home.join(".ssh/id_ed25519_aiki");
        let pub_key_path = key_path.with_extension("pub");

        println!("Generating SSH key...");
        println!("  Type: ed25519");
        println!("  Path: {}", key_path.display());
        println!();

        // Check if key already exists
        if key_path.exists() {
            println!("⚠ Key already exists at {}", key_path.display());
            let overwrite = prompt_yes_no("Overwrite existing key?", false)?;
            if !overwrite {
                anyhow::bail!(
                    "Setup canceled. Use existing key with: aiki sign setup --key {} --backend ssh",
                    pub_key_path.display()
                );
            }
        }

        // Run ssh-keygen
        let output = Command::new("ssh-keygen")
            .args([
                "-t",
                "ed25519",
                "-C",
                &email,
                "-f",
                key_path.to_str().unwrap(),
                "-N",
                "", // No passphrase
            ])
            .output()
            .context("Failed to run ssh-keygen. Is SSH installed?")?;

        if !output.status.success() {
            anyhow::bail!(
                "SSH key generation failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        println!("✓ Generated SSH key pair");
        println!("  Public:  {}", pub_key_path.display());
        println!("  Private: {}", key_path.display());
        println!();

        // Create allowed-signers file
        signing_config::create_ssh_allowed_signers(
            &self.repo_path,
            &email,
            pub_key_path.to_str().unwrap(),
        )?;

        println!("✓ Created allowed-signers file");
        println!();

        Ok(SigningConfig {
            backend: SigningBackend::Ssh,
            key: pub_key_path.to_string_lossy().to_string(),
            behavior: "own".to_string(),
        })
    }

    /// Configure a manually specified key
    fn configure_manual_key(&self, backend: SigningBackend, key: String) -> Result<SigningConfig> {
        println!("Configuring manual key...");
        println!();

        // Verify key exists/is accessible
        match backend {
            SigningBackend::Gpg | SigningBackend::GpgSm => {
                // Check GPG key exists
                let output = Command::new("gpg")
                    .args(["--list-secret-keys", &key])
                    .output()
                    .context("Failed to verify GPG key. Is GPG installed?")?;

                if !output.status.success() {
                    anyhow::bail!(
                        "GPG key '{}' not found. Run 'gpg --list-secret-keys' to see available keys.",
                        key
                    );
                }

                println!("✓ Verified GPG key: {}", key);
            }
            SigningBackend::Ssh => {
                // Expand ~ if present
                let expanded_path = if key.starts_with("~/") {
                    let home = dirs::home_dir().context("Could not find home directory")?;
                    home.join(&key[2..])
                } else {
                    PathBuf::from(&key)
                };

                if !expanded_path.exists() {
                    anyhow::bail!("SSH key file not found: {}", expanded_path.display());
                }

                println!("✓ Found SSH key: {}", expanded_path.display());

                // Create allowed-signers file
                let email = Command::new("git")
                    .args(["config", "--get", "user.email"])
                    .output()
                    .ok()
                    .and_then(|o| String::from_utf8(o.stdout).ok())
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|| prompt_string("Email for allowed-signers", None).unwrap());

                signing_config::create_ssh_allowed_signers(
                    &self.repo_path,
                    &email,
                    expanded_path.to_str().unwrap(),
                )?;

                println!("✓ Created allowed-signers file");
            }
        }

        println!();

        Ok(SigningConfig {
            backend,
            key,
            behavior: "own".to_string(),
        })
    }

    /// Verify the key is accessible
    fn verify_key(&self, config: &SigningConfig) -> Result<()> {
        signing_config::verify_key_accessible(config)?;
        Ok(())
    }

    /// Apply configuration to JJ
    fn apply_config(&self, config: &SigningConfig) -> Result<()> {
        config::update_jj_signing_config(
            &self.repo_path,
            &config.backend.to_string(),
            Some(&config.key),
            &config.behavior,
        )?;

        println!("✓ Configured JJ commit signing");
        println!();

        Ok(())
    }

    /// Optionally update git config
    fn offer_git_config_update(&self, config: &SigningConfig) -> Result<()> {
        println!("Git Integration");
        println!("===============");
        println!();
        println!("Would you like to set this as your default Git signing key?");
        println!("This will make all future Git commits signed automatically.");
        println!();

        let update = prompt_yes_no("Update Git config?", true)?;

        if !update {
            println!("Skipping Git config update.");
            println!();
            return Ok(());
        }

        // Set signing key
        Command::new("git")
            .args(["config", "--global", "user.signingkey", &config.key])
            .status()?;

        // Enable signing
        Command::new("git")
            .args(["config", "--global", "commit.gpgsign", "true"])
            .status()?;

        // Set format
        let format = match config.backend {
            SigningBackend::Gpg => "openpgp",
            SigningBackend::Ssh => "ssh",
            SigningBackend::GpgSm => "x509",
        };

        Command::new("git")
            .args(["config", "--global", "gpg.format", format])
            .status()?;

        println!();
        println!("✓ Updated Git config:");
        println!("  • commit.gpgsign = true");
        println!("  • user.signingkey = {}", config.key);
        println!("  • gpg.format = {}", format);
        println!();

        Ok(())
    }

    /// Show success message
    fn show_success_message(&self, config: &SigningConfig) {
        println!("✓ Signing setup complete!");
        println!();
        println!("Configuration:");
        println!("  Backend: {:?}", config.backend);
        println!("  Key: {}", config.key);
        println!(
            "  JJ config: {}/.jj/repo/config.toml",
            self.repo_path.display()
        );
        println!();
        println!("Next steps:");
        println!("  1. Make an edit with Claude Code or Cursor");
        println!("  2. Run: jj log -r @ --summary");
        println!("  3. Look for: \"Signed with {:?} key...\"", config.backend);
        println!();
        println!("Verification:");
        println!("  • Check status: aiki doctor");

        match config.backend {
            SigningBackend::Gpg => {
                println!("  • View key: gpg --list-keys {}", config.key);
            }
            SigningBackend::Ssh => {
                println!("  • View key: cat {}", config.key);
            }
            _ => {}
        }
        println!();
    }
}

// Helper functions

/// Get user name and email from git config
fn get_user_details_from_git() -> Result<(String, String)> {
    let name_output = Command::new("git")
        .args(["config", "--get", "user.name"])
        .output()
        .context("Failed to get git user.name")?;

    let email_output = Command::new("git")
        .args(["config", "--get", "user.email"])
        .output()
        .context("Failed to get git user.email")?;

    if !name_output.status.success() || !email_output.status.success() {
        anyhow::bail!("Git user.name or user.email not configured");
    }

    let name = String::from_utf8(name_output.stdout)?.trim().to_string();
    let email = String::from_utf8(email_output.stdout)?.trim().to_string();

    Ok((name, email))
}

/// Generate a GPG key with the given details
fn generate_gpg_key_with_details(name: &str, email: &str) -> Result<String> {
    // Create batch input for GPG
    let batch_input = format!(
        "Key-Type: RSA
Key-Length: 4096
Subkey-Type: RSA
Subkey-Length: 4096
Name-Real: {}
Name-Email: {}
Expire-Date: 2y
%no-protection
%commit
",
        name, email
    );

    // Run gpg --batch --generate-key
    let mut child = Command::new("gpg")
        .arg("--batch")
        .arg("--generate-key")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("Failed to spawn gpg command. Is GPG installed?")?;

    // Write batch input to stdin
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(batch_input.as_bytes())?;
    }

    // Wait for completion
    let output = child.wait_with_output()?;

    if !output.status.success() {
        anyhow::bail!(
            "GPG key generation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Extract key ID
    let list_output = Command::new("gpg")
        .args(["--list-secret-keys", "--keyid-format", "LONG", email])
        .output()
        .context("Failed to list GPG keys")?;

    let stdout = String::from_utf8_lossy(&list_output.stdout);

    // Parse key ID from output (format: "sec   rsa4096/KEY_ID date")
    for line in stdout.lines() {
        if line.starts_with("sec") {
            if let Some(key_part) = line.split_whitespace().nth(1) {
                if let Some(key_id) = key_part.split('/').nth(1) {
                    return Ok(key_id.to_string());
                }
            }
        }
    }

    anyhow::bail!("Could not extract key ID from GPG output")
}

/// Prompt for a numbered choice
fn prompt_choice(prompt: &str, min: usize, max: usize) -> Result<usize> {
    loop {
        print!("{} [{}]: ", prompt, min);
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        if input.is_empty() {
            return Ok(min);
        }

        match input.parse::<usize>() {
            Ok(n) if n >= min && n <= max => return Ok(n),
            _ => println!("Please enter a number between {} and {}", min, max),
        }
    }
}

/// Prompt for a string value
fn prompt_string(prompt: &str, default: Option<&str>) -> Result<String> {
    if let Some(def) = default {
        print!("{} [{}]: ", prompt, def);
    } else {
        print!("{}: ", prompt);
    }
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim().to_string();

    if input.is_empty() {
        if let Some(def) = default {
            return Ok(def.to_string());
        }
    }

    Ok(input)
}

/// Prompt for yes/no
fn prompt_yes_no(prompt: &str, default: bool) -> Result<bool> {
    let default_str = if default { "Y/n" } else { "y/N" };
    print!("{} [{}]: ", prompt, default_str);
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim().to_lowercase();

    if input.is_empty() {
        return Ok(default);
    }

    Ok(input == "y" || input == "yes")
}
