//! Reusable view component functions.
//!
//! Each function returns `Vec<Line>` for composition by view builders.

use crate::tui::app::{Line, LineStyle, SubtaskStatus};

// ── Data types ──────────────────────────────────────────────────────

pub struct ChildLine {
    pub text: String,
    pub meta: Option<String>,
    pub style: ChildStyle,
}

pub enum ChildStyle {
    Active,
    Done,
    Error,
    Warning,
    Normal,
    Bold,
}

impl ChildLine {
    pub fn active(text: &str) -> Self {
        Self {
            text: text.to_string(),
            meta: None,
            style: ChildStyle::Active,
        }
    }
    pub fn active_with_elapsed(text: &str, elapsed: Option<String>) -> Self {
        Self {
            text: text.to_string(),
            meta: elapsed,
            style: ChildStyle::Active,
        }
    }
    pub fn done(text: &str, elapsed: Option<String>) -> Self {
        Self {
            text: text.to_string(),
            meta: elapsed,
            style: ChildStyle::Done,
        }
    }
    pub fn error(text: &str, elapsed: Option<String>) -> Self {
        Self {
            text: text.to_string(),
            meta: elapsed,
            style: ChildStyle::Error,
        }
    }
    pub fn warning(text: &str) -> Self {
        Self {
            text: text.to_string(),
            meta: None,
            style: ChildStyle::Warning,
        }
    }
    pub fn normal(text: &str, elapsed: Option<String>) -> Self {
        Self {
            text: text.to_string(),
            meta: elapsed,
            style: ChildStyle::Normal,
        }
    }
    pub fn bold(text: &str) -> Self {
        Self {
            text: text.to_string(),
            meta: None,
            style: ChildStyle::Bold,
        }
    }
}

pub struct SubtaskData {
    pub name: String,
    pub status: SubtaskStatus,
    pub elapsed: Option<String>,
}

pub struct LaneData {
    pub number: usize,
    pub agent: String,
    pub completed: usize,
    pub total: usize,
    pub failed: usize,
    pub heartbeat: Option<String>,
    pub elapsed: Option<String>,
    pub shutdown: bool,
}

// ── Component functions ─────────────────────────────────────────────

/// Phase header + child lines.
///
/// `active` controls the icon: spinner when true, 合 when false.
/// Agent name is rendered inline after the phase name: `合 name (agent)`.
pub fn phase(
    group: u16,
    name: &str,
    agent: Option<&str>,
    active: bool,
    children: Vec<ChildLine>,
) -> Vec<Line> {
    let mut lines = Vec::with_capacity(1 + children.len());

    let text = match agent {
        Some(a) => format!("{} ({})", name, a),
        None => name.to_string(),
    };

    lines.push(Line {
        indent: 0,
        text,
        meta: None,
        style: LineStyle::PhaseHeader { active },
        group,
        dimmed: false,
    });

    for child in children {
        let style = match child.style {
            ChildStyle::Active => LineStyle::ChildActive,
            ChildStyle::Done => LineStyle::ChildDone,
            ChildStyle::Error => LineStyle::ChildError,
            ChildStyle::Warning => LineStyle::ChildWarning,
            ChildStyle::Normal => LineStyle::Child,
            ChildStyle::Bold => LineStyle::ChildBold,
        };
        lines.push(Line {
            indent: 1,
            text: child.text,
            meta: child.meta,
            style,
            group,
            dimmed: false,
        });
    }

    lines
}

/// Subtask table between separators.
pub fn subtask_table(
    group: u16,
    short_id: &str,
    title: &str,
    subtasks: &[SubtaskData],
    loading: bool,
) -> Vec<Line> {
    let mut lines = Vec::new();

    // Opening separator
    lines.push(Line {
        indent: 0,
        text: String::new(),
        meta: None,
        style: LineStyle::Separator,
        group,
        dimmed: false,
    });

    lines.extend(blank());

    // Header line
    lines.push(Line {
        indent: 0,
        text: format!("[{}] {}", short_id, title),
        meta: None,
        style: LineStyle::SubtaskHeader,
        group,
        dimmed: false,
    });

    if subtasks.is_empty() && loading {
        lines.push(Line {
            indent: 1,
            text: "...".to_string(),
            meta: None,
            style: LineStyle::Dim,
            group,
            dimmed: false,
        });
    } else {
        for st in subtasks {
            lines.push(Line {
                indent: 1,
                text: st.name.clone(),
                meta: st.elapsed.clone(),
                style: LineStyle::Subtask { status: st.status },
                group,
                dimmed: false,
            });
        }
    }

    lines.extend(blank());

    // Closing separator
    lines.push(Line {
        indent: 0,
        text: String::new(),
        meta: None,
        style: LineStyle::Separator,
        group,
        dimmed: false,
    });

    lines
}

/// Lane blocks under a loop phase.
pub fn loop_block(group: u16, lanes: &[LaneData]) -> Vec<Line> {
    let mut lines = Vec::new();

    let active = lanes.iter().any(|l| !l.shutdown);
    lines.push(Line {
        indent: 0,
        text: "loop".to_string(),
        meta: None,
        style: LineStyle::PhaseHeader { active },
        group,
        dimmed: false,
    });

    for (i, lane) in lanes.iter().enumerate() {
        // Blank line between lanes (not before the first)
        if i > 0 {
            lines.extend(blank());
        }

        // Lane header: ⎿ Lane N (agent)
        let lane_label = format!("Lane {} ({})", lane.number, lane.agent);
        lines.push(Line {
            indent: 1,
            text: lane_label,
            meta: None,
            style: LineStyle::Child,
            group,
            dimmed: false,
        });

        // Progress line: ⎿ x/y subtasks completed[, z failed]
        let progress = if lane.failed > 0 {
            format!(
                "{}/{} subtasks completed, {} failed",
                lane.completed, lane.total, lane.failed
            )
        } else {
            format!("{}/{} subtasks completed", lane.completed, lane.total)
        };
        lines.push(Line {
            indent: 2,
            text: progress,
            meta: None,
            style: LineStyle::Child,
            group,
            dimmed: false,
        });

        // Heartbeat/status line
        if lane.shutdown {
            lines.push(Line {
                indent: 2,
                text: "Agent shutdown.".to_string(),
                meta: None,
                style: LineStyle::Child,
                group,
                dimmed: false,
            });
        } else if let Some(ref hb) = lane.heartbeat {
            lines.push(Line {
                indent: 2,
                text: hb.clone(),
                meta: lane.elapsed.clone(),
                style: LineStyle::ChildActive,
                group,
                dimmed: false,
            });
        } else {
            lines.push(Line {
                indent: 2,
                text: "starting session...".to_string(),
                meta: lane.elapsed.clone(),
                style: LineStyle::ChildActive,
                group,
                dimmed: false,
            });
        }

        // Error line if failures exist and lane not shutdown
        if lane.failed > 0 && !lane.shutdown {
            lines.push(Line {
                indent: 2,
                text: format!(
                    "Error: {} task{} failed",
                    lane.failed,
                    if lane.failed == 1 { "" } else { "s" }
                ),
                meta: None,
                style: LineStyle::ChildError,
                group,
                dimmed: false,
            });
        }
    }

    lines
}

/// Numbered issue list.
pub fn issues(group: u16, issues: &[String]) -> Vec<Line> {
    issues
        .iter()
        .enumerate()
        .map(|(i, issue)| Line {
            indent: 1,
            text: format!("{}. {}", i + 1, issue),
            meta: None,
            style: LineStyle::Issue,
            group,
            dimmed: false,
        })
        .collect()
}

/// Section header.
pub fn section_header(group: u16, text: &str) -> Vec<Line> {
    vec![Line {
        indent: 0,
        text: text.to_string(),
        meta: None,
        style: LineStyle::SectionHeader,
        group,
        dimmed: false,
    }]
}

/// Standalone separator line (for build summary, etc.)
pub fn separator(group: u16) -> Vec<Line> {
    vec![Line {
        indent: 0,
        text: String::new(),
        meta: None,
        style: LineStyle::Separator,
        group,
        dimmed: false,
    }]
}

/// Single blank line.
pub fn blank() -> Vec<Line> {
    vec![Line {
        indent: 0,
        text: String::new(),
        meta: None,
        style: LineStyle::Blank,
        group: 0,
        dimmed: false,
    }]
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_produces_header_and_children() {
        let lines = phase(
            0,
            "plan",
            Some("claude"),
            false,
            vec![ChildLine {
                text: format!("{} path.md", crate::tui::theme::SYM_CHECK),
                style: ChildStyle::Done,
                meta: None,
            }],
        );
        assert_eq!(lines.len(), 2);
        assert!(matches!(
            lines[0].style,
            LineStyle::PhaseHeader { active: false }
        ));
        assert!(matches!(lines[1].style, LineStyle::ChildDone));
        // Agent should be inline in text, not in meta
        assert!(lines[0].text.contains("plan (claude)"));
        assert!(lines[0].meta.is_none());
    }

    #[test]
    fn phase_active_sets_active_flag() {
        let lines = phase(0, "build", None, true, vec![]);
        assert!(matches!(
            lines[0].style,
            LineStyle::PhaseHeader { active: true }
        ));
        assert_eq!(lines[0].text, "build");
    }

    #[test]
    fn subtask_table_with_loading_shows_placeholder() {
        let lines = subtask_table(0, "lkji3d", "Epic: Test", &[], true);
        // Placeholder is present (index shifted due to blank line after separator)
        assert!(lines.iter().any(|l| l.text.contains("...")));
    }

    #[test]
    fn subtask_table_has_blank_lines_between_separators_and_content() {
        let subtasks = vec![SubtaskData {
            name: "Do thing".into(),
            status: SubtaskStatus::Pending,
            elapsed: None,
        }];
        let lines = subtask_table(0, "abc123", "Epic: Blanks", &subtasks, false);
        // First line: Separator
        assert!(matches!(lines[0].style, LineStyle::Separator));
        // Second line: Blank
        assert!(matches!(lines[1].style, LineStyle::Blank));
        // Second-to-last line: Blank
        assert!(matches!(lines[lines.len() - 2].style, LineStyle::Blank));
        // Last line: Separator
        assert!(matches!(lines[lines.len() - 1].style, LineStyle::Separator));
    }

    #[test]
    fn loop_block_produces_lines_per_lane() {
        let lanes = vec![
            LaneData {
                number: 1,
                agent: "claude".into(),
                completed: 1,
                total: 3,
                failed: 0,
                heartbeat: Some("thinking...".into()),
                elapsed: Some("1m 23s".into()),
                shutdown: false,
            },
            LaneData {
                number: 2,
                agent: "claude".into(),
                completed: 3,
                total: 3,
                failed: 0,
                heartbeat: None,
                elapsed: None,
                shutdown: true,
            },
        ];
        let lines = loop_block(0, &lanes);

        // Header
        assert!(matches!(
            lines[0].style,
            LineStyle::PhaseHeader { active: true }
        ));
        assert_eq!(lines[0].text, "loop");

        // Lane 1 header
        assert!(lines[1].text.starts_with("Lane"));
        assert_eq!(lines[1].text, "Lane 1 (claude)");
        assert!(matches!(lines[1].style, LineStyle::Child));
        assert_eq!(lines[1].indent, 1);

        // Lane 1 progress
        assert_eq!(lines[2].text, "1/3 subtasks completed");
        assert_eq!(lines[2].indent, 2);

        // Lane 1 heartbeat
        assert_eq!(lines[3].text, "thinking...");
        assert!(matches!(lines[3].style, LineStyle::ChildActive));

        // Blank line between lanes
        assert!(matches!(lines[4].style, LineStyle::Blank));

        // Lane 2 header
        assert_eq!(lines[5].text, "Lane 2 (claude)");

        // Lane 2 progress
        assert_eq!(lines[6].text, "3/3 subtasks completed");

        // Lane 2 shutdown status
        assert_eq!(lines[7].text, "Agent shutdown.");
        assert!(matches!(lines[7].style, LineStyle::Child));
    }

    #[test]
    fn issues_produces_numbered_lines() {
        let lines = issues(0, &["first issue".into(), "second issue".into()]);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].text.contains("1."));
        assert!(matches!(lines[0].style, LineStyle::Issue));
    }
}
