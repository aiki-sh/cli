use anyhow::{Context, Result};
use crate::jj::jj_cmd;
use std::path::Path;

use crate::provenance::record::ProvenanceRecord;

/// Signature verification status
#[derive(Debug, Clone, PartialEq)]
pub enum SignatureStatus {
    /// Signature is valid and verified
    Good,
    /// Signature is invalid or verification failed
    Bad,
    /// Signature verification status is unknown
    Unknown,
    /// Change is not signed
    Unsigned,
}

impl SignatureStatus {
    /// Parse signature status from JJ template output
    fn from_jj_output(output: &str) -> Self {
        let trimmed = output.trim();
        match trimmed {
            "good" => SignatureStatus::Good,
            "bad" => SignatureStatus::Bad,
            "unknown" => SignatureStatus::Unknown,
            "" => SignatureStatus::Unsigned,
            _ => SignatureStatus::Unknown,
        }
    }

    /// Get display symbol for status
    pub fn symbol(&self) -> &'static str {
        match self {
            SignatureStatus::Good => "✓",
            SignatureStatus::Bad => "✗",
            SignatureStatus::Unknown => "⚠",
            SignatureStatus::Unsigned => "⚠",
        }
    }

    /// Get human-readable description
    pub fn description(&self) -> &'static str {
        match self {
            SignatureStatus::Good => "Valid signature",
            SignatureStatus::Bad => "Invalid signature",
            SignatureStatus::Unknown => "Unknown signature status",
            SignatureStatus::Unsigned => "Not signed",
        }
    }

    /// Check if this is a valid (good) signature
    pub fn is_valid(&self) -> bool {
        matches!(self, SignatureStatus::Good)
    }
}

/// Complete verification result for a change
#[derive(Debug)]
pub struct VerificationResult {
    /// The change ID that was verified
    pub change_id: String,
    /// Signature verification status
    pub signature_status: SignatureStatus,
    /// Signer information (if signed)
    pub signer: Option<String>,
    /// Key fingerprint/ID (if available)
    pub key: Option<String>,
    /// Provenance metadata (if present)
    pub provenance: Option<ProvenanceRecord>,
}

impl VerificationResult {
    /// Check if this change is fully verified (valid signature + provenance)
    pub fn is_verified(&self) -> bool {
        self.signature_status.is_valid() && self.provenance.is_some()
    }

    /// Get a human-readable result summary
    pub fn result_summary(&self) -> String {
        match (&self.signature_status, &self.provenance) {
            (SignatureStatus::Good, Some(_)) => "VERIFIED ✓".to_string(),
            (SignatureStatus::Good, None) => "SIGNED (no AI metadata)".to_string(),
            (SignatureStatus::Bad, _) => "FAILED ✗ (invalid signature)".to_string(),
            (SignatureStatus::Unknown, _) => "UNKNOWN ⚠ (cannot verify signature)".to_string(),
            (SignatureStatus::Unsigned, Some(_)) => "UNVERIFIED (no signature)".to_string(),
            (SignatureStatus::Unsigned, None) => "NOT AN AI CHANGE".to_string(),
        }
    }
}

/// Run a JJ template query and return the output
fn run_jj_template(repo_path: &Path, revision: &str, template: &str) -> Result<String> {
    let output = jj_cmd()
        .args(["log", "-r", revision, "-T", template, "--no-graph"])
        .current_dir(repo_path)
        .output()
        .context("Failed to run jj command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("jj command failed: {}", stderr);
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Verify a change's cryptographic signature and provenance metadata
pub fn verify_change(repo_path: &Path, revision: &str) -> Result<VerificationResult> {
    // Get the change ID
    let change_id = run_jj_template(repo_path, revision, "change_id")?
        .trim()
        .to_string();

    // Check signature status
    let sig_status_output = run_jj_template(
        repo_path,
        revision,
        r#"if(signature, signature.status(), "")"#,
    )?;
    let signature_status = SignatureStatus::from_jj_output(&sig_status_output);

    // Get signer info if signed
    let signer = if signature_status != SignatureStatus::Unsigned {
        let signer_output = run_jj_template(
            repo_path,
            revision,
            r#"if(signature, signature.display(), "")"#,
        )?;
        let trimmed = signer_output.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    } else {
        None
    };

    // Get key fingerprint if available
    let key = if signature_status != SignatureStatus::Unsigned {
        let key_output =
            run_jj_template(repo_path, revision, r#"if(signature, signature.key(), "")"#)?;
        let trimmed = key_output.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    } else {
        None
    };

    // Get change description and parse provenance
    let description = run_jj_template(repo_path, revision, "description")?;
    let provenance = ProvenanceRecord::from_description(&description)?;

    Ok(VerificationResult {
        change_id,
        signature_status,
        signer,
        key,
        provenance,
    })
}

/// Format and display verification results
pub fn format_verification_result(result: &VerificationResult) {
    println!("Verifying change {}...\n", result.change_id);

    // Signature section
    println!("Signature:");
    println!(
        "  {} {}",
        result.signature_status.symbol(),
        result.signature_status.description()
    );

    if let Some(signer) = &result.signer {
        println!("  Signer: {}", signer);
    }

    if let Some(key) = &result.key {
        println!("  Key ID: {}", key);
    }

    println!();

    // Provenance section
    println!("Provenance:");
    if let Some(prov) = &result.provenance {
        println!("  ✓ Metadata present and valid");
        println!("  Agent: {}", prov.agent.agent_type);
        println!("  Session: {}", prov.session_id);
        println!("  Tool: {}", prov.tool_name);
        println!("  Confidence: {:?}", prov.agent.confidence);
        println!("  Method: {:?}", prov.agent.detection_method);
    } else {
        println!("  ⚠ No AI metadata found");
    }

    println!();

    // Result summary
    println!("Result: {}", result.result_summary());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signature_status_from_jj_output() {
        assert_eq!(
            SignatureStatus::from_jj_output("good"),
            SignatureStatus::Good
        );
        assert_eq!(SignatureStatus::from_jj_output("bad"), SignatureStatus::Bad);
        assert_eq!(
            SignatureStatus::from_jj_output("unknown"),
            SignatureStatus::Unknown
        );
        assert_eq!(
            SignatureStatus::from_jj_output(""),
            SignatureStatus::Unsigned
        );
        assert_eq!(
            SignatureStatus::from_jj_output("  "),
            SignatureStatus::Unsigned
        );
    }

    #[test]
    fn test_signature_status_is_valid() {
        assert!(SignatureStatus::Good.is_valid());
        assert!(!SignatureStatus::Bad.is_valid());
        assert!(!SignatureStatus::Unknown.is_valid());
        assert!(!SignatureStatus::Unsigned.is_valid());
    }

    #[test]
    fn test_verification_result_is_verified() {
        use crate::provenance::record::{AgentInfo, AgentType, AttributionConfidence, DetectionMethod};
        use chrono::Utc;

        let prov = ProvenanceRecord {
            agent: AgentInfo {
                agent_type: AgentType::ClaudeCode,
                version: None,
                detected_at: Utc::now(),
                confidence: AttributionConfidence::High,
                detection_method: DetectionMethod::Hook,
            },
            client_name: None,
            client_version: None,
            agent_version: None,
            session_id: "test-session".to_string(),
            tool_name: "Edit".to_string(),
            turn: 0,
            turn_id: String::new(),
            turn_source: String::new(),
            coauthor: None,
            tasks: Vec::new(),
            prompt_change_id: None,
        };

        // Valid signature + provenance = verified
        let result = VerificationResult {
            change_id: "abc123".to_string(),
            signature_status: SignatureStatus::Good,
            signer: Some("test@example.com".to_string()),
            key: Some("123456".to_string()),
            provenance: Some(prov.clone()),
        };
        assert!(result.is_verified());

        // Valid signature without provenance = not verified
        let result = VerificationResult {
            change_id: "abc123".to_string(),
            signature_status: SignatureStatus::Good,
            signer: Some("test@example.com".to_string()),
            key: Some("123456".to_string()),
            provenance: None,
        };
        assert!(!result.is_verified());

        // Unsigned with provenance = not verified
        let result = VerificationResult {
            change_id: "abc123".to_string(),
            signature_status: SignatureStatus::Unsigned,
            signer: None,
            key: None,
            provenance: Some(prov),
        };
        assert!(!result.is_verified());
    }
}
