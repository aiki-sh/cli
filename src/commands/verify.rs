use crate::error::{AikiError, Result};
use crate::verify;
use anyhow::Context;
use std::env;

pub fn run(revision: String) -> Result<()> {
    let current_dir = env::current_dir().context("Failed to get current directory")?;

    // Verify we're in a JJ repository
    if !current_dir.join(".jj").exists() {
        return Err(AikiError::NotInJjRepo);
    }

    // Perform verification
    let result =
        verify::verify_change(&current_dir, &revision).context("Failed to verify change")?;

    // Display results
    verify::format_verification_result(&result);

    // Exit with error code if verification failed
    if !result.is_verified() && result.signature_status != verify::SignatureStatus::Unsigned {
        std::process::exit(1);
    }

    Ok(())
}
