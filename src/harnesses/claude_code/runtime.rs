use crate::agents::AgentSpawnOptions;
use crate::harnesses::runtime::{CliArgs, RuntimeEnv};

pub(super) fn args(opts: &AgentSpawnOptions) -> CliArgs {
    let mut args = CliArgs::new();
    args.push("--print");
    args.push("--dangerously-skip-permissions");
    args.push(opts.task_prompt());
    args
}

// Unset nesting guards so child Claude Code sessions can start.
pub(super) fn env(env: &mut RuntimeEnv) {
    env.remove("CLAUDECODE");
    env.remove("CLAUDE_CODE_ENTRYPOINT");
}
