//! Deprecated alias for `aiki epic add`
//!
//! This module preserves backward compatibility for `aiki decompose`.
//! All logic has moved to `epic.rs`. This shim delegates to the epic command.

use clap::Subcommand;

use super::OutputFormat;
use super::epic::EpicCommands;
use crate::error::Result;

/// Decompose subcommands (deprecated — use `aiki epic show` instead)
#[derive(Subcommand)]
pub enum DecomposeSubcommands {
    /// Show epic status and subtasks
    Show {
        /// Plan path or epic task ID (32 lowercase letters)
        arg: String,

        /// Output format (e.g., `id` for bare task ID)
        #[arg(long, short = 'o', value_name = "FORMAT")]
        output: Option<OutputFormat>,
    },
}

/// Arguments for the decompose command (deprecated — use `aiki epic add` instead)
#[derive(clap::Args)]
pub struct DecomposeArgs {
    /// Path to plan file (e.g., ops/now/my-feature.md)
    pub plan_path: Option<String>,

    /// Ignore existing epic and create a new one from scratch
    #[arg(long)]
    pub restart: bool,

    /// Decompose template to use (default: aiki/decompose)
    #[arg(long)]
    pub template: Option<String>,

    /// Agent for decomposition (default: claude-code)
    #[arg(long)]
    pub agent: Option<String>,

    /// Subcommand (show)
    #[command(subcommand)]
    pub subcommand: Option<DecomposeSubcommands>,
}

/// Run the decompose command by delegating to epic.
pub fn run(args: DecomposeArgs) -> Result<()> {
    // Dispatch subcommand
    if let Some(subcommand) = args.subcommand {
        return match subcommand {
            DecomposeSubcommands::Show { arg, output } => {
                super::epic::run(EpicCommands::Show { arg, output })
            }
        };
    }

    // Delegate add to epic
    let plan_path = args.plan_path.ok_or_else(|| {
        crate::error::AikiError::InvalidArgument(
            "No plan path provided. Usage: aiki epic add <plan-path>".to_string(),
        )
    })?;

    super::epic::run(EpicCommands::Add {
        plan_path,
        restart: args.restart,
        template: args.template,
        agent: args.agent,
    })
}
