use crate::error::Result;
use anyhow::anyhow;

pub const PREREQUISITES: &[(&str, &str)] = &[
    ("git", "Git version control"),
    ("jj", "Jujutsu version control"),
];

const INSTALL_LINKS: &[(&str, &str)] = &[
    ("git", "https://git-scm.com/downloads"),
    (
        "jj",
        "https://martinvonz.github.io/jj/latest/install-and-setup/",
    ),
];

/// Check if a command exists and return its version
pub fn check_command_version(cmd: &str) -> Option<String> {
    std::process::Command::new(cmd)
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| {
            String::from_utf8(output.stdout)
                .ok()
                .and_then(|s| s.lines().next().map(|line| line.to_string()))
        })
}

fn install_link(cmd: &str) -> Option<&'static str> {
    INSTALL_LINKS
        .iter()
        .find(|(c, _)| *c == cmd)
        .map(|(_, url)| *url)
}

/// Check that all prerequisites are installed.
///
/// When `quiet` is false, prints a status line for each tool.
/// When `quiet` is true, only reports errors.
pub fn check_prerequisites(quiet: bool) -> Result<()> {
    let mut missing: Vec<(&str, &str)> = Vec::new();

    for &(cmd, description) in PREREQUISITES {
        match check_command_version(cmd) {
            Some(version) => {
                if !quiet {
                    println!("  \u{2713} {} ({})", description, version);
                }
            }
            None => {
                if !quiet {
                    println!("  \u{2717} {} not found", description);
                }
                missing.push((cmd, description));
            }
        }
    }

    if missing.is_empty() {
        return Ok(());
    }

    let mut msg = String::from(
        "Missing required prerequisites\n\nThe following tools are required but not found:",
    );
    for (cmd, description) in &missing {
        msg.push_str(&format!("\n  \u{2717} {} ({})", description, cmd));
        if let Some(url) = install_link(cmd) {
            msg.push_str(&format!("\n    \u{2192} Install from {}", url));
        }
    }
    msg.push_str("\n\nRun `aiki doctor` for a full system check.");

    Err(anyhow!(msg).into())
}
