//! Loop command for orchestrating a parent task's subtasks via lanes
//!
//! This module provides the `aiki loop` command and the shared `run_loop()`
//! function used by `build.rs`, `fix.rs`, and the CLI.
//!
//! `run_loop()` creates a loop task from the `loop` template, wires up
//! the `orchestrates` link, and runs the task (sync or async).

use std::env;

use crate::commands::OutputFormat;
use crate::error::{AikiError, Result};
pub use crate::workflow::steps::r#loop::{run_loop, LoopOptions};
use crate::workflow::OutputKind;

/// Arguments for the loop command
#[derive(clap::Args)]
pub struct LoopArgs {
    /// Parent task ID whose subtasks to orchestrate
    pub parent_id: String,

    /// Run loop asynchronously (return immediately)
    #[arg(long = "async")]
    pub run_async: bool,

    /// Agent for loop orchestration (default: claude-code)
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

    /// Template name override (default: loop)
    #[arg(long)]
    pub template: Option<String>,

    /// Output format (e.g., `id` for bare task ID on stdout)
    #[arg(long, short = 'o', value_name = "FORMAT")]
    pub output: Option<OutputFormat>,
}

/// Run the loop command (CLI entry point)
pub fn run(args: LoopArgs) -> Result<()> {
    use crate::session::flags::resolve_agent_shorthand;
    let agent_type = resolve_agent_shorthand(args.agent, args.claude, args.codex, args.cursor, args.gemini);

    let cwd = env::current_dir()
        .map_err(|_| AikiError::InvalidArgument("Failed to get current directory".to_string()))?;

    let mut options = LoopOptions::new().with_async(args.run_async);
    if let Some(agent) = agent_type {
        options = options.with_agent(agent);
    }
    if let Some(template) = args.template {
        options = options.with_template(template);
    }

    let loop_task_id = run_loop(&cwd, &args.parent_id, options, false, OutputKind::Text, None)?;

    // --output id: emit bare task ID and exit before orchestration
    if matches!(args.output, Some(OutputFormat::Id)) {
        println!("{}", loop_task_id);
    }

    Ok(())
}
