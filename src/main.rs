mod agents;

mod cache;
mod commands;
mod config;
mod epic;
mod editors;
mod error;
mod event_bus;
mod events;
mod expressions;
mod flows;
mod global;
mod harnesses;
mod history;
mod instructions;
mod jj;
mod output_utils;
mod parsing;
mod plugins;
mod prerequisites;
mod provenance;
mod repos;
mod reviews;

mod plans;
mod session;
mod settings;
mod tasks;
mod tools;
mod tui;
mod utils;
mod validation;
mod workflow;

use clap::{Parser, Subcommand};
use error::Result;

#[derive(Parser)]
#[command(name = "aiki")]
#[command(version)]
#[command(about = "AI code review engine", long_about = None)]
#[command(disable_help_subcommand = true)]
#[command(help_template = HELP_TEMPLATE)]
struct Cli {
    /// Enable debug output (equivalent to AIKI_DEBUG=1)
    #[arg(long, global = true, hide = true)]
    debug: bool,

    #[command(subcommand)]
    command: Commands,
}

const HELP_TEMPLATE: &str = "\
{about-with-newline}
{usage-heading} {usage}

Setup:
  init        Initialize Aiki in the current repository
  remove      Remove Aiki from a repo, or disable it for you
  doctor      Diagnose and fix configuration issues
  plugin      Manage plugins (install, update, list, remove)
  config      Manage config settings (get, set, unset, file)

For Humans:
  plan        Interactive plan authoring with AI agent
  build       Build from a plan file (decompose and execute all subtasks)
  review      Create and run code review tasks
  fix         Create and run followup tasks from review comments
  run         Spawn an agent session for a task

For Agents:
  epic        Manage epics (create from plan files, show status, list)
  task        Manage tasks
  explore     Explore a scope (plan, code, task, or session)
  decompose   Decompose a plan into subtasks under a target task
  loop        Orchestrate a parent task's subtasks via lanes
  resolve     Resolve JJ merge conflicts
  recover     List/reclaim preserved unabsorbed session work

For Everyone:
  tldr        Summarize what a closed epic changed
  session     Manage sessions
  blame       Show line-by-line AI attribution for a file
  authors     Show authors who contributed to changes
  benchmark   Run end-to-end performance benchmark

Options:
  -h, --help     Print help
  -V, --version  Print version
";

#[derive(Subcommand)]
enum Commands {
    /// Initialize Aiki in the current repository
    Init {
        /// Only print error and warning messages (suppress normal output)
        #[arg(short, long)]
        quiet: bool,
    },
    /// Remove aiki from this repo (or just disable it for you)
    Remove {
        /// Also tear down the checked-in repo integration (.aiki/, the <aiki>
        /// block, the instruction symlink, git config). Working-tree changes;
        /// committing them affects teammates.
        #[arg(long, alias = "repo")]
        shared: bool,
        /// Machine-wide teardown: editor hooks, the OTel receiver, and ~/.aiki
        /// (which removes every per-user marker). Leaves checked-in .aiki/ alone.
        #[arg(long)]
        global: bool,
        /// Skip confirmation prompts. Required for non-interactive --shared/--global.
        #[arg(long)]
        force: bool,
    },
    /// Diagnose and fix configuration issues
    #[command(override_usage = "aiki doctor [--fix] [--claude] [--codex]\n       aiki doctor --fix --quarantined [--<agent>]")]
    Doctor {
        /// Automatically fix detected issues
        #[arg(long)]
        fix: bool,
        /// Scope to Claude Code checks only (skips repo-wide checks and mutations)
        #[arg(long)]
        claude: bool,
        /// Scope to Codex checks only (skips repo-wide checks and mutations)
        #[arg(long)]
        codex: bool,
        /// Authorize stripping macOS quarantine attributes (valid form: aiki doctor --fix --quarantined [--<agent>])
        #[arg(long, requires = "fix")]
        quarantined: bool,
    },
    /// Manage plugins (install, update, list, remove)
    Plugin {
        #[command(subcommand)]
        command: commands::plugin::PluginCommands,
    },
    /// Manage Aiki hooks
    #[command(hide = true)]
    Hooks {
        #[command(subcommand)]
        command: HooksCommands,
    },
    /// Show line-by-line AI attribution for a file
    Blame {
        /// File to show blame for
        file: std::path::PathBuf,
        /// Filter by agent type (e.g., claude-code, cursor)
        #[arg(long)]
        agent: Option<String>,
    },
    /// Show authors who contributed to changes
    Authors {
        /// Scope changes: "staged" for Git staging area, default is working copy (@)
        #[arg(long)]
        changes: Option<String>,

        /// Output format: plain (default), git, json
        #[arg(long, default_value = "plain")]
        format: String,
    },
    /// Run end-to-end performance benchmark
    Benchmark {
        /// Number of edits to simulate (default: 50)
        #[arg(short, long, default_value = "50")]
        edits: usize,
    },
    /// Spawn an agent session for a task
    Run {
        /// Task ID to run (or parent ID with --next-thread)
        id: Option<String>,
        /// Return after spawn instead of blocking until session ends
        #[arg(long = "async")]
        run_async: bool,
        /// Force direct run on reserved/in-progress tasks by resetting state
        #[arg(long)]
        force: bool,
        /// Pick next ready thread (needs-context chain or standalone task)
        #[arg(long)]
        next_thread: bool,
        /// Scope --next-thread to a specific lane (head task ID, prefix matching)
        #[arg(long, requires = "next_thread")]
        lane: Option<String>,
        /// Override assignee agent (claude-code, codex)
        #[arg(long)]
        agent: Option<String>,
        /// Shorthand for --agent claude-code
        #[arg(long, group = "agent_shorthand", conflicts_with = "agent")]
        claude: bool,
        /// Shorthand for --agent codex
        #[arg(long, group = "agent_shorthand", conflicts_with = "agent")]
        codex: bool,
        /// Shorthand for --agent cursor
        #[arg(long, group = "agent_shorthand", conflicts_with = "agent")]
        cursor: bool,
        /// Shorthand for --agent gemini
        #[arg(long, group = "agent_shorthand", conflicts_with = "agent")]
        gemini: bool,
        /// Create task from template before running
        #[arg(long, conflicts_with_all = ["id", "next_thread"])]
        template: Option<String>,
        /// Key=value pairs for template variables
        #[arg(long, requires = "template")]
        data: Option<Vec<String>>,
        /// Output format (e.g., `id` for bare session UUID on stdout)
        #[arg(long, short = 'o')]
        output: Option<commands::OutputFormat>,
    },
    /// Manage sessions
    Session {
        #[command(subcommand)]
        command: commands::session::SessionCommands,
    },
    /// Manage config settings
    Config {
        #[command(subcommand)]
        command: commands::config::ConfigCommands,
    },
    /// Manage tasks
    Task {
        #[command(subcommand)]
        command: Option<commands::task::TaskCommands>,
    },
    /// Dispatch Aiki events (internal use)
    #[command(hide = true)]
    Event {
        #[command(subcommand)]
        command: EventCommands,
    },
    /// Create and run followup tasks from review comments
    Fix(commands::fix::FixArgs),
    /// Explore a scope (plan, code, task, or session)
    Explore(commands::explore::ExploreArgs),
    /// Create and run code review tasks
    Review(commands::review::ReviewArgs),
    /// Resolve JJ merge conflicts
    Resolve(commands::resolve::ResolveArgs),
    /// List and reclaim preserved-but-unabsorbed session work
    Recover(commands::recover::RecoverArgs),
    /// Manage epics (create from plan files, show status, list)
    Epic {
        #[command(subcommand)]
        command: commands::epic::EpicCommands,
    },
    /// Decompose a plan into subtasks under a target task
    Decompose(commands::decompose::DecomposeArgs),
    /// Build from a plan file (decompose and execute all subtasks)
    Build(commands::build::BuildArgs),
    /// Orchestrate a parent task's subtasks via lanes
    Loop(commands::loop_cmd::LoopArgs),
    /// Summarize what a closed epic changed
    Tldr(commands::tldr::TldrArgs),
    /// Interactive plan authoring with AI agent.
    /// Subcommands: epic (default), fix.
    /// Examples: `aiki plan feature.md`, `aiki plan epic Add auth`,
    /// `aiki plan fix <review-id>`
    Plan {
        /// Subcommand and arguments. First arg can be 'epic' or 'fix',
        /// otherwise defaults to epic behavior.
        args: Vec<String>,
        /// Plan template to use (default: plan)
        #[arg(long)]
        template: Option<String>,
        /// Agent for plan session (default: claude-code)
        #[arg(long)]
        agent: Option<String>,
        /// Shorthand for --agent claude-code
        #[arg(long, group = "plan_agent_shorthand", conflicts_with = "agent")]
        claude: bool,
        /// Shorthand for --agent codex
        #[arg(long, group = "plan_agent_shorthand", conflicts_with = "agent")]
        codex: bool,
        /// Shorthand for --agent cursor
        #[arg(long, group = "plan_agent_shorthand", conflicts_with = "agent")]
        cursor: bool,
        /// Shorthand for --agent gemini
        #[arg(long, group = "plan_agent_shorthand", conflicts_with = "agent")]
        gemini: bool,
        /// Output format (e.g., `id` for bare task ID on stdout)
        #[arg(long, short = 'o', value_name = "FORMAT")]
        output: Option<commands::OutputFormat>,
    },
    /// (deprecated alias for 'aiki plan')
    #[command(hide = true)]
    Spec {
        args: Vec<String>,
        #[arg(long)]
        template: Option<String>,
        #[arg(long)]
        agent: Option<String>,
    },
}

#[derive(Subcommand)]
#[command(disable_help_subcommand = true)]
enum EventCommands {
    /// Trigger PrepareCommitMessage event (for Git's prepare-commit-msg hook)
    #[command(name = "prepare-commit-msg")]
    PrepareCommitMessage,
}

#[derive(Subcommand)]
#[command(disable_help_subcommand = true)]
enum HooksCommands {
    /// Stdin integration point (Claude Code, Cursor - reads JSON from stdin)
    #[command(hide = true)]
    Stdin {
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        event: Option<String>,
        /// Shorthand for --agent claude-code --event <EVENT>
        #[arg(long, value_name = "EVENT", group = "agent_shorthand", conflicts_with = "agent")]
        claude: Option<String>,
        /// Shorthand for --agent codex --event <EVENT>
        #[arg(long, value_name = "EVENT", group = "agent_shorthand", conflicts_with = "agent")]
        codex: Option<String>,
        /// Shorthand for --agent cursor --event <EVENT>
        #[arg(long, value_name = "EVENT", group = "agent_shorthand", conflicts_with = "agent")]
        cursor: Option<String>,
        /// Hidden flag: when set, this is the background continuation of an async hook.
        /// The original hook payload is piped via stdin.
        #[arg(long = "_continue-async", hide = true)]
        continue_async: bool,
        #[arg(trailing_var_arg = true)]
        payload: Vec<String>,
    },
    /// ACP integration point (proxy for ACP protocol agents)
    #[command(hide = true)]
    Acp {
        #[arg(long)]
        agent: Option<String>,
        /// Shorthand for --agent claude-code
        #[arg(long, group = "agent_shorthand", conflicts_with = "agent")]
        claude: bool,
        /// Shorthand for --agent codex
        #[arg(long, group = "agent_shorthand", conflicts_with = "agent")]
        codex: bool,
        /// Shorthand for --agent cursor
        #[arg(long, group = "agent_shorthand", conflicts_with = "agent")]
        cursor: bool,
        /// Shorthand for --agent gemini
        #[arg(long, group = "agent_shorthand", conflicts_with = "agent")]
        gemini: bool,
        #[arg(short, long)]
        bin: Option<String>,
        #[arg(last = true)]
        agent_args: Vec<String>,
    },
    /// OTel integration point (Codex - reads HTTP/OTLP from stdin)
    #[command(hide = true)]
    Otel {
        #[arg(long, default_value = "codex")]
        agent: String,
    },
}

fn main() {
    if let Err(err) = run() {
        eprintln!("Error: {}", err);
        std::process::exit(1);
    }
}

/// Commands that always run, regardless of init state. `--version`/`--help`
/// (and `<sub> --help`) are handled by clap during `parse()` and never reach
/// here. `hooks` is gated separately inside `run_stdin`.
fn is_gate_allowlisted(command: &Commands) -> bool {
    matches!(
        command,
        Commands::Init { .. }
            | Commands::Remove { .. }
            | Commands::Doctor { .. }
            | Commands::Hooks { .. }
    )
}

/// Hard-enforcement CLI gate: every non-allowlisted subcommand requires aiki to
/// be Active in the current directory. Soft SessionStart signals are the
/// proactive layer; this is the safety net for agents that drift onto `aiki`
/// commands in a repo where aiki is installed but not enabled.
fn enforce_cli_gate(command: &Commands) -> Result<()> {
    use repos::InitState;

    if is_gate_allowlisted(command) {
        return Ok(());
    }

    let cwd = std::env::current_dir().unwrap_or_default();
    match repos::init_state(&cwd)? {
        InitState::Active { .. } => Ok(()),
        InitState::OrphanedMarker { root } => {
            // CLI is not a hot path — opportunistically reap the stale marker
            // (a teammate ran `aiki remove --shared` and pushed the removal).
            let _ = std::fs::remove_file(repos::marker_path(&root));
            Err(gate_refusal(&InitState::NotAikiRepo))
        }
        state => Err(gate_refusal(&state)),
    }
}

/// Build the gate refusal error, worded for the detected audience: an agent
/// harness is coached to ask the user and never auto-init; a human at a terminal
/// gets the terse nudge.
fn gate_refusal(state: &repos::InitState) -> error::AikiError {
    use repos::InitState;
    let under_harness = harnesses::detect_parent_harness().is_some();
    let dormant = matches!(state, InitState::Dormant { .. });

    let msg = match (under_harness, dormant) {
        (true, true) => "This repo declares aiki but it is not enabled for this account. \
            Ask the user to run `aiki init` if they want aiki here. Do NOT run `aiki init` or \
            other `aiki` commands yourself unless the user explicitly requests it."
            .to_string(),
        (true, false) => "Aiki is not active in this repo. Ask the user whether they want to \
            run `aiki init`. Do NOT run it yourself unless the user explicitly requests it."
            .to_string(),
        (false, true) => {
            "This repo declares aiki. Run `aiki init` to opt in for your account.".to_string()
        }
        (false, false) => "aiki not active here. Run `aiki init` to enable.".to_string(),
    };
    error::AikiError::Other(anyhow::anyhow!(msg))
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    if cli.debug {
        std::env::set_var("AIKI_DEBUG", "1");
    }

    // Hard-enforcement gate (allowlist bypasses; `hooks` self-gates in run_stdin).
    enforce_cli_gate(&cli.command)?;

    match cli.command {
        Commands::Init { quiet } => commands::init::run(quiet),
        Commands::Remove { shared, global, force } => commands::remove::run(shared, global, force),
        Commands::Doctor {
            fix,
            claude,
            codex,
            quarantined,
        } => commands::doctor::run(
            fix,
            commands::doctor::DoctorScope {
                claude,
                codex,
                fix_quarantined: quarantined,
            },
        ),
        Commands::Plugin { command } => commands::plugin::run(command),
        Commands::Hooks { command } => match command {
            HooksCommands::Stdin {
                agent,
                event,
                claude,
                codex,
                cursor,
                continue_async,
                payload,
            } => {
                let (agent_type, event) = session::flags::resolve_agent_event_shorthand(
                    agent, event, claude, codex, cursor, None,
                )
                .ok_or_else(|| {
                    error::AikiError::MissingArgument(
                        "--agent and --event, or an agent shorthand (--claude, --codex, --cursor)".into(),
                    )
                })?;
                let payload_str = if payload.is_empty() {
                    None
                } else {
                    Some(payload.join(" "))
                };
                commands::hooks::run_stdin(agent_type.as_str().to_string(), event, continue_async, payload_str)
            }
            HooksCommands::Acp {
                agent,
                claude,
                codex,
                cursor,
                gemini,
                bin,
                agent_args,
            } => {
                let agent_type = session::flags::resolve_agent_shorthand(
                    agent, claude, codex, cursor, gemini,
                )?
                .ok_or_else(|| {
                    error::AikiError::MissingArgument(
                        "--agent or an agent shorthand (--claude, --codex, --cursor, --gemini)".into(),
                    )
                })?;
                commands::acp::run(agent_type.as_str().to_string(), bin, agent_args)
            }
            HooksCommands::Otel { agent } => commands::otel_receive::run(agent),
        },
        Commands::Blame { file, agent } => commands::blame::run(file, agent),
        Commands::Authors { changes, format } => commands::authors::run(changes, format),
        Commands::Benchmark { edits } => commands::benchmark::run("aiki/core".to_string(), edits),
        Commands::Run {
            id,
            run_async,
            force,
            next_thread,
            lane,
            agent,
            claude,
            codex,
            cursor,
            gemini,
            template,
            data,
            output,
        } => commands::run::run(
            id,
            run_async,
            force,
            next_thread,
            lane,
            session::flags::resolve_agent_shorthand(agent, claude, codex, cursor, gemini)?,
            template,
            data,
            output,
        ),
        Commands::Session { command } => commands::session::run(command),
        Commands::Config { command } => commands::config::run(command),
        Commands::Task { command } => commands::task::run(command),
        Commands::Event { command } => match command {
            EventCommands::PrepareCommitMessage => commands::event::run_prepare_commit_message(),
        },
        Commands::Fix(args) => commands::fix::run(args),
        Commands::Explore(args) => commands::explore::run(args),
        Commands::Review(args) => commands::review::run(args),
        Commands::Resolve(args) => commands::resolve::run(args),
        Commands::Recover(args) => commands::recover::run(args),
        Commands::Epic { command } => commands::epic::run(command),
        Commands::Decompose(args) => commands::decompose::run(args),
        Commands::Build(args) => commands::build::run(args),
        Commands::Loop(args) => commands::loop_cmd::run(args),
        Commands::Tldr(args) => commands::tldr::run(args),
        Commands::Plan {
            args,
            template,
            agent,
            claude,
            codex,
            cursor,
            gemini,
            output,
        } => commands::plan::run(
            args,
            template,
            session::flags::resolve_agent_shorthand(agent, claude, codex, cursor, gemini)?,
            output,
        ),
        Commands::Spec {
            args,
            template,
            agent,
        } => {
            eprintln!("Warning: 'aiki spec' is deprecated, use 'aiki plan' instead.");
            commands::plan::run(args, template, agent.as_deref().and_then(agents::AgentType::from_str), None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_parses_agent_after_freeform_args() {
        let cli = Cli::try_parse_from([
            "aiki",
            "plan",
            "ops/now/feature.md",
            "add",
            "auth",
            "--agent",
            "codex",
        ])
        .unwrap();

        match cli.command {
            Commands::Plan { args, agent, .. } => {
                assert_eq!(args, vec!["ops/now/feature.md", "add", "auth"]);
                assert_eq!(agent.as_deref(), Some("codex"));
            }
            _ => panic!("expected plan command"),
        }
    }

    #[test]
    fn spec_parses_agent_after_freeform_args() {
        let cli = Cli::try_parse_from([
            "aiki",
            "spec",
            "ops/now/feature.md",
            "add",
            "auth",
            "--agent",
            "codex",
        ])
        .unwrap();

        match cli.command {
            Commands::Spec { args, agent, .. } => {
                assert_eq!(args, vec!["ops/now/feature.md", "add", "auth"]);
                assert_eq!(agent.as_deref(), Some("codex"));
            }
            _ => panic!("expected spec command"),
        }
    }
}
