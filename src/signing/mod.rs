pub mod config;
pub mod setup_wizard;

// Re-export commonly used types for convenience
#[allow(unused_imports)]
pub use config::{
    create_ssh_allowed_signers, detect_git_signing_config, detect_gpg_keys, detect_signing_config,
    detect_ssh_keys, get_user_email, read_signing_config, verify_key_accessible, SigningBackend,
    SigningConfig,
};
pub use setup_wizard::{SetupMode, SignSetupWizard};
