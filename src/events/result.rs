/// Failure message type
#[derive(Debug, Clone)]
pub struct Failure(pub String);

/// Decision about how to respond to a hook event
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Allow the operation to proceed
    Allow,

    /// Block the operation (error messages are in HookResponse.failures)
    Block,
}

impl Decision {
    /// Check if this decision is to block the operation
    #[must_use]
    pub fn is_block(&self) -> bool {
        matches!(self, Decision::Block)
    }
}

/// Generic hook result (editor-agnostic)
#[derive(Debug, Clone)]
pub struct HookResult {
    /// Context string for PrePrompt (modified prompt) or PostResponse (autoreply)
    pub context: Option<String>,

    /// Decision about whether to allow or block the operation
    pub decision: Decision,

    /// Failure messages
    pub failures: Vec<Failure>,
}

impl HookResult {
    #[must_use]
    pub fn success() -> Self {
        Self {
            context: None,
            decision: Decision::Allow,
            failures: Vec::new(),
        }
    }

    #[must_use]
    pub fn failure(user_msg: impl Into<String>) -> Self {
        Self {
            context: None,
            decision: Decision::Allow, // Non-blocking - allow operation
            failures: vec![Failure(user_msg.into())],
        }
    }

    /// Check if this response should block the operation
    #[must_use]
    pub fn is_blocking(&self) -> bool {
        self.decision.is_block()
    }

    /// Format failure messages with emoji prefixes
    ///
    /// Converts failures into formatted strings with ❌ emoji prefix.
    ///
    /// These formatted messages can be:
    /// - Shown to user (stderr)
    /// - Combined with context and sent to agent (PrePrompt, PostResponse)
    /// - Used in vendor-specific output (Cursor followup_message, Claude Code reason)
    #[must_use]
    pub fn format_messages(&self) -> String {
        self.failures
            .iter()
            .map(|Failure(s)| format!("❌ {}", s))
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Combine formatted messages and context according to Phase 8 architecture
    ///
    /// Returns Some(combined_string) if either messages or context are non-empty,
    /// None if both are empty.
    ///
    /// Used by vendor translators to combine validation messages with context
    /// for events like PrePrompt and PostResponse.
    #[must_use]
    pub fn combined_output(&self) -> Option<String> {
        let formatted_messages = self.format_messages();
        let context = self.context.as_deref().unwrap_or("");

        match (!formatted_messages.is_empty(), !context.is_empty()) {
            (true, true) => Some(format!("{}\n\n{}", formatted_messages, context)),
            (true, false) => Some(formatted_messages),
            (false, true) => Some(context.to_string()),
            (false, false) => None,
        }
    }

    /// Check if this response has meaningful context
    ///
    /// Returns true if the context field contains a non-empty string.
    /// Used by the dispatcher to determine if PostResponse generated an autoreply.
    ///
    /// # Examples
    /// ```
    /// # use aiki::events::result::HookResult;
    /// let resp1 = HookResult { context: Some("autoreply text".into()), ..HookResult::success() };
    /// assert!(resp1.has_context());
    ///
    /// let resp2 = HookResult { context: Some("".into()), ..HookResult::success() };
    /// assert!(!resp2.has_context());
    ///
    /// let resp3 = HookResult::success();
    /// assert!(!resp3.has_context());
    /// ```
    #[must_use]
    pub fn has_context(&self) -> bool {
        self.context.as_ref().map_or(false, |s| !s.is_empty())
    }
}
