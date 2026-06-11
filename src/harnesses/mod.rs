mod claude_code;
mod codex;
mod cursor;
mod gemini;

pub mod definition;
mod detect;
pub mod runtime;

pub use detect::detect_harness_from_process_tree;

use crate::agents::AgentType;
use definition::HarnessDefinition;

pub fn iter() -> impl Iterator<Item = &'static HarnessDefinition> {
    inventory::iter::<HarnessDefinition>.into_iter()
}

pub fn lookup(id_or_alias: &str) -> Option<&'static HarnessDefinition> {
    let needle = id_or_alias.to_lowercase();
    iter().find(|d| {
        d.identity.id == needle
            || d.identity.aliases.iter().any(|a| *a == needle)
    })
}

pub fn lookup_by_agent_type(agent_type: &AgentType) -> Option<&'static HarnessDefinition> {
    iter().find(|d| d.identity.agent_type == *agent_type)
}

pub fn all_sorted() -> Vec<&'static HarnessDefinition> {
    let mut v: Vec<_> = iter().collect();
    v.sort_by_key(|d| d.identity.id);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_agent_type_has_exactly_one_harness() {
        for at in [AgentType::ClaudeCode, AgentType::Codex, AgentType::Cursor, AgentType::Gemini] {
            let matches: Vec<_> = iter().filter(|d| d.identity.agent_type == at).collect();
            assert_eq!(
                matches.len(),
                1,
                "AgentType::{:?} should have exactly 1 harness, found {}",
                at,
                matches.len()
            );
        }
    }

    #[test]
    fn no_duplicate_harness_ids_or_aliases() {
        let all = all_sorted();
        let mut seen = std::collections::HashSet::new();
        for d in &all {
            assert!(seen.insert(d.identity.id), "duplicate harness id: {}", d.identity.id);
            for alias in d.identity.aliases {
                assert!(seen.insert(alias), "duplicate alias: {}", alias);
            }
        }
    }

    #[test]
    fn aliases_do_not_conflict_with_canonical_ids() {
        let ids: std::collections::HashSet<_> = iter().map(|d| d.identity.id).collect();
        for d in iter() {
            for alias in d.identity.aliases {
                assert!(
                    !ids.contains(alias) || *alias == d.identity.id,
                    "alias '{}' of harness '{}' conflicts with canonical id",
                    alias,
                    d.identity.id
                );
            }
        }
    }

    #[test]
    fn lookup_is_case_insensitive() {
        assert!(lookup("CLAUDE-CODE").is_some());
        assert!(lookup("Claude").is_some());
        assert!(lookup("CODEX").is_some());
    }

    #[test]
    fn all_sorted_returns_stable_order() {
        let sorted = all_sorted();
        let ids: Vec<_> = sorted.iter().map(|d| d.identity.id).collect();
        let mut expected = ids.clone();
        expected.sort();
        assert_eq!(ids, expected);
    }
}
