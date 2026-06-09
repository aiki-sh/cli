//! Sugar pattern expansion for task lifecycle events
//!
//! This module handles syntactic sugar for task lifecycle events:
//! - `{type}.started` expands to `task.started` with `if: event.task.type == "{type}"`
//! - `{type}.completed` expands to `task.closed` with `if: event.task.type == "{type}" && event.task.outcome == "done"`

use anyhow::Result;
use serde_yaml::{Mapping, Value};

/// Known event triggers that are NOT sugar patterns.
/// These are the built-in event types that should be handled directly.
const KNOWN_TRIGGERS: &[&str] = &[
    "session.started",
    "session.resumed",
    "session.ended",
    "turn.started",
    "turn.completed",
    "read.permission_asked",
    "read.completed",
    "change.permission_asked",
    "change.completed",
    "shell.permission_asked",
    "shell.completed",
    "web.permission_asked",
    "web.completed",
    "mcp.permission_asked",
    "mcp.completed",
    "commit.message_started",
    "task.started",
    "task.closed",
];

/// Known event domain prefixes that should NOT be treated as sugar patterns.
/// These are used to avoid false positives when a user defines a custom event
/// like "session.something" that happens to end with ".started" or ".completed".
const KNOWN_DOMAINS: &[&str] = &[
    "session", "turn", "read", "change", "shell", "web", "mcp", "commit", "task",
];

/// Check if a trigger is a sugar pattern (not a known trigger).
///
/// A sugar pattern is a trigger that:
/// 1. Ends with ".started" or ".completed"
/// 2. Is NOT a known built-in trigger
/// 3. Does NOT use a known domain prefix (e.g., "session", "task")
pub fn is_sugar_pattern(trigger: &str) -> bool {
    // Must end with .started or .completed
    if !trigger.ends_with(".started") && !trigger.ends_with(".completed") {
        return false;
    }

    // Must not be a known built-in trigger
    if KNOWN_TRIGGERS.contains(&trigger) {
        return false;
    }

    // Extract the domain (part before the first dot)
    if let Some(domain) = trigger.split('.').next() {
        // Must not use a known domain prefix
        if KNOWN_DOMAINS.contains(&domain) {
            return false;
        }
    }

    true
}

/// Expand a sugar pattern into (base_event, wrapped_statements).
///
/// Returns None if not a sugar pattern.
/// Returns Some((base_event, wrapped_value)) where wrapped_value is the statements
/// wrapped in an if condition.
pub fn expand_sugar_value(
    trigger: &str,
    statements: Value,
) -> Result<Option<(&'static str, Value)>> {
    // Check for {type}.started pattern
    if let Some(task_type) = trigger.strip_suffix(".started") {
        if is_sugar_pattern(trigger) {
            let filter = format!("event.task.type == \"{}\"", task_type);
            let wrapped = wrap_with_if_value(filter, statements);
            return Ok(Some(("task.started", wrapped)));
        }
    }

    // Check for {type}.completed pattern
    if let Some(task_type) = trigger.strip_suffix(".completed") {
        if is_sugar_pattern(trigger) {
            let filter = format!(
                "event.task.type == \"{}\" && event.task.outcome == \"done\"",
                task_type
            );
            let wrapped = wrap_with_if_value(filter, statements);
            return Ok(Some(("task.closed", wrapped)));
        }
    }

    Ok(None)
}

/// Wrap statements with an if condition, producing a YAML Value.
fn wrap_with_if_value(condition: String, statements: Value) -> Value {
    let mut if_mapping = Mapping::new();
    if_mapping.insert(Value::String("if".to_string()), Value::String(condition));
    if_mapping.insert(Value::String("then".to_string()), statements);

    // Return as a sequence with one element (the if statement)
    Value::Sequence(vec![Value::Mapping(if_mapping)])
}

/// Expand sugar patterns in a YAML mapping.
///
/// This function processes the top-level mapping and:
/// 1. Identifies sugar patterns (e.g., "review.started", "feature.completed")
/// 2. Expands them into their base events (task.started, task.closed)
/// 3. Wraps the statements with appropriate if conditions
/// 4. Merges expanded statements into existing base event handlers
pub fn expand_sugar_patterns(map: &mut Mapping) -> Result<()> {
    // Collect sugar patterns to expand
    let mut to_expand: Vec<(String, Value)> = Vec::new();
    let mut to_remove: Vec<Value> = Vec::new();

    for (key, value) in map.iter() {
        if let Value::String(trigger) = key {
            if is_sugar_pattern(trigger) {
                to_expand.push((trigger.clone(), value.clone()));
                to_remove.push(key.clone());
            }
        }
    }

    // Remove sugar pattern keys
    for key in to_remove {
        map.remove(&key);
    }

    // Expand and merge into base events
    for (trigger, statements) in to_expand {
        if let Some((base_event, wrapped)) = expand_sugar_value(&trigger, statements)? {
            let base_key = Value::String(base_event.to_string());

            // Get or create the base event's statement list
            let existing = map.entry(base_key).or_insert(Value::Sequence(vec![]));

            if let (Value::Sequence(ref mut seq), Value::Sequence(wrapped_seq)) =
                (existing, wrapped)
            {
                seq.extend(wrapped_seq);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_sugar_pattern_known_triggers() {
        // Known triggers should NOT be sugar patterns
        assert!(!is_sugar_pattern("session.started"));
        assert!(!is_sugar_pattern("session.resumed"));
        assert!(!is_sugar_pattern("turn.started"));
        assert!(!is_sugar_pattern("turn.completed"));
        assert!(!is_sugar_pattern("task.started"));
        assert!(!is_sugar_pattern("task.closed"));
        assert!(!is_sugar_pattern("change.completed"));
    }

    #[test]
    fn test_is_sugar_pattern_custom_types() {
        // Custom task types should BE sugar patterns
        assert!(is_sugar_pattern("review.started"));
        assert!(is_sugar_pattern("review.completed"));
        assert!(is_sugar_pattern("feature.started"));
        assert!(is_sugar_pattern("feature.completed"));
        assert!(is_sugar_pattern("bugfix.started"));
        assert!(is_sugar_pattern("bugfix.completed"));
    }

    #[test]
    fn test_is_sugar_pattern_non_patterns() {
        // Random strings should NOT be sugar patterns
        assert!(!is_sugar_pattern("review"));
        assert!(!is_sugar_pattern("something.else"));
        assert!(!is_sugar_pattern("foo.bar"));
    }

    #[test]
    fn test_is_sugar_pattern_known_domains() {
        // Known domain prefixes should NOT be sugar patterns even with .started/.completed
        assert!(!is_sugar_pattern("session.completed")); // session is a known domain
        assert!(!is_sugar_pattern("task.completed")); // task is a known domain
    }

    #[test]
    fn test_expand_sugar_value_started() {
        let statements = Value::Sequence(vec![Value::Mapping({
            let mut m = Mapping::new();
            m.insert(
                Value::String("log".to_string()),
                Value::String("Review started".to_string()),
            );
            m
        })]);

        let result = expand_sugar_value("review.started", statements).unwrap();
        assert!(result.is_some());

        let (base_event, wrapped) = result.unwrap();
        assert_eq!(base_event, "task.started");

        // Verify the wrapped structure
        if let Value::Sequence(seq) = wrapped {
            assert_eq!(seq.len(), 1);
            if let Value::Mapping(if_map) = &seq[0] {
                let condition = if_map.get(&Value::String("if".to_string())).unwrap();
                assert_eq!(
                    condition,
                    &Value::String("event.task.type == \"review\"".to_string())
                );
            } else {
                panic!("Expected Mapping");
            }
        } else {
            panic!("Expected Sequence");
        }
    }

    #[test]
    fn test_expand_sugar_value_completed() {
        let statements = Value::Sequence(vec![Value::Mapping({
            let mut m = Mapping::new();
            m.insert(
                Value::String("log".to_string()),
                Value::String("Review done".to_string()),
            );
            m
        })]);

        let result = expand_sugar_value("review.completed", statements).unwrap();
        assert!(result.is_some());

        let (base_event, wrapped) = result.unwrap();
        assert_eq!(base_event, "task.closed");

        // Verify the wrapped structure includes outcome check
        if let Value::Sequence(seq) = wrapped {
            assert_eq!(seq.len(), 1);
            if let Value::Mapping(if_map) = &seq[0] {
                let condition = if_map.get(&Value::String("if".to_string())).unwrap();
                assert_eq!(
                    condition,
                    &Value::String(
                        "event.task.type == \"review\" && event.task.outcome == \"done\""
                            .to_string()
                    )
                );
            } else {
                panic!("Expected Mapping");
            }
        } else {
            panic!("Expected Sequence");
        }
    }

    #[test]
    fn test_expand_sugar_value_non_pattern() {
        let statements = Value::Sequence(vec![]);

        // Known triggers should not expand
        let result = expand_sugar_value("session.started", statements.clone()).unwrap();
        assert!(result.is_none());

        let result = expand_sugar_value("task.started", statements.clone()).unwrap();
        assert!(result.is_none());

        let result = expand_sugar_value("task.closed", statements).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_expand_sugar_patterns_in_mapping() {
        let yaml = r#"
name: Test Hook
version: "1"
review.started:
  - log: "Review started"
review.completed:
  - log: "Review done"
task.started:
  - log: "Existing task handler"
"#;

        let mut value: Value = serde_yaml::from_str(yaml).unwrap();
        if let Value::Mapping(ref mut map) = value {
            expand_sugar_patterns(map).unwrap();

            // Sugar patterns should be removed
            assert!(!map.contains_key(&Value::String("review.started".to_string())));
            assert!(!map.contains_key(&Value::String("review.completed".to_string())));

            // task.started should have both existing and expanded statements
            let task_started = map.get(&Value::String("task.started".to_string())).unwrap();
            if let Value::Sequence(seq) = task_started {
                // Original + expanded
                assert_eq!(seq.len(), 2);
            } else {
                panic!("Expected Sequence for task.started");
            }

            // task.closed should have the expanded review.completed
            let task_closed = map.get(&Value::String("task.closed".to_string())).unwrap();
            if let Value::Sequence(seq) = task_closed {
                assert_eq!(seq.len(), 1);
            } else {
                panic!("Expected Sequence for task.closed");
            }
        }
    }
}
