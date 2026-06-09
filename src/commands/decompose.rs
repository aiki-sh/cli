//! Decompose command — break a plan into subtasks under a target task.
//!
//! Provides both a CLI entry point (`aiki decompose <plan> --target <id>`)
//! and a public `run_decompose()` function reusable from `epic.rs` and `fix.rs`.

use std::env;

use super::OutputFormat;
use crate::error::{AikiError, Result};
pub use crate::workflow::steps::decompose::{run_decompose, DecomposeOptions};

/// Arguments for the `aiki decompose` CLI command.
#[derive(clap::Args)]
pub struct DecomposeArgs {
    /// Path to plan file (e.g., ops/now/my-feature.md)
    pub plan_path: String,

    /// Target task ID to decompose into (subtasks are created under this task)
    #[arg(long)]
    pub target: String,

    /// Decompose template to use (default: decompose)
    #[arg(long)]
    pub template: Option<String>,

    /// Agent for decomposition (default: claude-code)
    #[arg(long)]
    pub agent: Option<String>,

    /// Shorthand for --agent claude-code
    #[arg(long, group = "agent_shorthand", conflicts_with = "agent")]
    pub claude: bool,
    /// Shorthand for --agent codex
    #[arg(long, group = "agent_shorthand", conflicts_with = "agent")]
    pub codex: bool,
    /// Shorthand for --agent cursor
    #[arg(long, group = "agent_shorthand", conflicts_with = "agent")]
    pub cursor: bool,
    /// Shorthand for --agent gemini
    #[arg(long, group = "agent_shorthand", conflicts_with = "agent")]
    pub gemini: bool,

    /// Output format (e.g., `id` for bare task ID)
    #[arg(long, short = 'o', value_name = "FORMAT")]
    pub output: Option<OutputFormat>,
}

/// CLI entry point for `aiki decompose`.
pub fn run(args: DecomposeArgs) -> Result<()> {
    use crate::session::flags::resolve_agent_shorthand;
    let agent_type = resolve_agent_shorthand(args.agent, args.claude, args.codex, args.cursor, args.gemini);

    let cwd = env::current_dir()
        .map_err(|_| AikiError::InvalidArgument("Failed to get current directory".to_string()))?;

    let options = DecomposeOptions {
        template: args.template,
        agent: agent_type,
    };

    let decompose_task_id = run_decompose(&cwd, &args.plan_path, &args.target, options, false, None)?;

    match args.output {
        Some(OutputFormat::Id) => println!("{}", decompose_task_id),
        None => eprintln!("Decomposed: {}", decompose_task_id),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decompose_args_required_fields() {
        use clap::Parser;

        #[derive(Parser)]
        struct Cli {
            #[command(flatten)]
            args: DecomposeArgs,
        }

        let cli = Cli::parse_from(["test", "ops/now/feat.md", "--target", "abc123"]);
        assert_eq!(cli.args.plan_path, "ops/now/feat.md");
        assert_eq!(cli.args.target, "abc123");
        assert!(cli.args.template.is_none());
        assert!(cli.args.agent.is_none());
        assert!(cli.args.output.is_none());
    }

    #[test]
    fn test_decompose_args_with_optional_fields() {
        use clap::Parser;

        #[derive(Parser)]
        struct Cli {
            #[command(flatten)]
            args: DecomposeArgs,
        }

        let cli = Cli::parse_from([
            "test",
            "plan.md",
            "--target",
            "t1",
            "--template",
            "my/tmpl",
            "--agent",
            "codex",
            "-o",
            "id",
        ]);
        assert_eq!(cli.args.plan_path, "plan.md");
        assert_eq!(cli.args.target, "t1");
        assert_eq!(cli.args.template, Some("my/tmpl".to_string()));
        assert_eq!(cli.args.agent, Some("codex".to_string()));
        assert!(matches!(cli.args.output, Some(OutputFormat::Id)));
    }
}
