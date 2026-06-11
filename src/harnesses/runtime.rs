use crate::agents::AgentSpawnOptions;
use std::ffi::OsString;

pub struct RuntimeConfig {
    pub kind: RuntimeKind,
    /// Optional env declaration: required env vars (checked at availability
    /// time) and env vars to remove from the child process at spawn (CLI only).
    /// Shared between CLI and HTTP harnesses.
    pub env: Option<fn(&mut RuntimeEnv)>,
}

pub enum RuntimeKind {
    Cli(CliRuntime),
}

#[derive(Default)]
pub struct RuntimeEnv {
    required: Vec<OsString>,
    remove: Vec<OsString>,
}

impl RuntimeEnv {
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare an env var that must be set for the runtime to be available.
    #[allow(dead_code)] // No built-in harness declares required env vars yet (HTTP harnesses will).
    pub fn require(&mut self, key: impl Into<OsString>) -> &mut Self {
        self.required.push(key.into());
        self
    }

    /// Declare an env var to remove from the child process at spawn time
    /// (CLI runtimes only; ignored by HTTP).
    pub fn remove(&mut self, key: impl Into<OsString>) -> &mut Self {
        self.remove.push(key.into());
        self
    }

    pub fn requires(&self) -> &[OsString] {
        &self.required
    }

    pub fn removes(&self) -> &[OsString] {
        &self.remove
    }
}

impl RuntimeConfig {
    pub fn is_available_by_default(&self) -> bool {
        if let Some(env_fn) = self.env {
            let mut env = RuntimeEnv::new();
            env_fn(&mut env);
            for required in env.requires() {
                if std::env::var(required).is_err() {
                    return false;
                }
            }
        }
        match &self.kind {
            RuntimeKind::Cli(cli) => which::which(cli.binary).is_ok(),
        }
    }
}

pub struct CliRuntime {
    pub binary: &'static str,
    pub args: fn(&AgentSpawnOptions) -> CliArgs,
}

#[derive(Default)]
pub struct CliArgs(Vec<OsString>);

impl CliArgs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, arg: impl Into<OsString>) -> &mut Self {
        self.0.push(arg.into());
        self
    }

    pub fn as_slice(&self) -> &[OsString] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::lanes::ThreadId;

    fn dummy_args(_opts: &AgentSpawnOptions) -> CliArgs {
        CliArgs::new()
    }

    fn cli_config(binary: &'static str, env: Option<fn(&mut RuntimeEnv)>) -> RuntimeConfig {
        RuntimeConfig {
            kind: RuntimeKind::Cli(CliRuntime {
                binary,
                args: dummy_args,
            }),
            env,
        }
    }

    // ------------------------------ RuntimeEnv ------------------------------

    #[test]
    fn runtime_env_new_is_empty() {
        let env = RuntimeEnv::new();
        assert!(env.requires().is_empty());
        assert!(env.removes().is_empty());
    }

    #[test]
    fn runtime_env_require_collects_in_order() {
        let mut env = RuntimeEnv::new();
        env.require("FOO").require("BAR");
        let required: Vec<&OsString> = env.requires().iter().collect();
        assert_eq!(required.len(), 2);
        assert_eq!(required[0], &OsString::from("FOO"));
        assert_eq!(required[1], &OsString::from("BAR"));
        assert!(env.removes().is_empty());
    }

    #[test]
    fn runtime_env_remove_collects_in_order() {
        let mut env = RuntimeEnv::new();
        env.remove("CLAUDECODE").remove("CLAUDE_CODE_ENTRYPOINT");
        let removed: Vec<&OsString> = env.removes().iter().collect();
        assert_eq!(removed.len(), 2);
        assert_eq!(removed[0], &OsString::from("CLAUDECODE"));
        assert_eq!(removed[1], &OsString::from("CLAUDE_CODE_ENTRYPOINT"));
        assert!(env.requires().is_empty());
    }

    #[test]
    fn runtime_env_require_and_remove_are_independent() {
        let mut env = RuntimeEnv::new();
        env.require("API_KEY").remove("NESTING_GUARD");
        assert_eq!(env.requires().len(), 1);
        assert_eq!(env.removes().len(), 1);
        assert_eq!(env.requires()[0], OsString::from("API_KEY"));
        assert_eq!(env.removes()[0], OsString::from("NESTING_GUARD"));
    }

    // ------------------------------- CliArgs --------------------------------

    #[test]
    fn cli_args_new_is_empty() {
        let args = CliArgs::new();
        assert!(args.as_slice().is_empty());
    }

    #[test]
    fn cli_args_push_appends_in_order() {
        let mut args = CliArgs::new();
        args.push("--print");
        args.push("--flag");
        args.push("payload");
        let slice = args.as_slice();
        assert_eq!(slice.len(), 3);
        assert_eq!(slice[0], OsString::from("--print"));
        assert_eq!(slice[1], OsString::from("--flag"));
        assert_eq!(slice[2], OsString::from("payload"));
    }

    #[test]
    fn cli_args_push_is_chainable() {
        let mut args = CliArgs::new();
        args.push("a").push("b").push("c");
        assert_eq!(args.as_slice().len(), 3);
    }

    #[test]
    fn cli_args_used_as_factory_target() {
        // The args fn signature is `fn(&AgentSpawnOptions) -> CliArgs`.
        // Smoke-check that a typical args fn works end-to-end.
        fn build(_opts: &AgentSpawnOptions) -> CliArgs {
            let mut a = CliArgs::new();
            a.push("--print");
            a
        }
        let opts = AgentSpawnOptions::new("/tmp", ThreadId::single("task".into()));
        let args = build(&opts);
        assert_eq!(args.as_slice(), &[OsString::from("--print")]);
    }

    // ------------------- RuntimeConfig::is_available_by_default -------------------

    #[test]
    fn cli_available_when_binary_on_path_and_no_env_required() {
        // `sh` is virtually always present on Unix CI hosts; skip on Windows.
        if cfg!(windows) || which::which("sh").is_err() {
            return;
        }
        let config = cli_config("sh", None);
        assert!(config.is_available_by_default());
    }

    #[test]
    fn cli_unavailable_when_binary_missing() {
        let config = cli_config("aiki-test-binary-that-should-never-exist-xyz123", None);
        assert!(!config.is_available_by_default());
    }

    #[test]
    fn cli_unavailable_when_required_env_missing() {
        // Use a name unlikely to be set in any reasonable environment.
        fn env(env: &mut RuntimeEnv) {
            env.require("AIKI_TEST_REQUIRED_VAR_THAT_IS_DEFINITELY_UNSET");
        }
        // Even when the binary exists, missing env should short-circuit.
        let config = cli_config("sh", Some(env));
        assert!(!config.is_available_by_default());
    }

    #[test]
    fn env_check_passes_when_required_var_is_set() {
        const KEY: &str = "AIKI_TEST_RUNTIME_ENV_PRESENT_KEY";
        std::env::set_var(KEY, "1");
        fn env(env: &mut RuntimeEnv) {
            env.require("AIKI_TEST_RUNTIME_ENV_PRESENT_KEY");
        }
        // `sh` is virtually always present on Unix CI hosts; skip on Windows.
        if cfg!(windows) || which::which("sh").is_err() {
            return;
        }
        let config = cli_config("sh", Some(env));
        assert!(config.is_available_by_default());
        std::env::remove_var(KEY);
    }
}
