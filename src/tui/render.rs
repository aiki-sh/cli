use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::tui::app::{Line, LineStyle, SubtaskStatus};
use crate::tui::theme::{self, Theme};

/// Convert a ratatui `Buffer` to a printable ANSI-escaped string.
///
/// Walks every cell, emitting 24-bit foreground color escapes (`\x1b[38;2;r;g;bm`)
/// and bold (`\x1b[1m`). Resets style between cells when it changes. Trims trailing
/// spaces from each line for cleaner output.
pub fn buffer_to_ansi(buf: &Buffer) -> String {
    let area = buf.area();
    let mut out = String::new();

    for row in area.y..area.y + area.height {
        let last_content_col = (area.x..area.x + area.width)
            .rev()
            .find(|&col| buf[(col, row)].symbol() != " ")
            .unwrap_or(area.x);
        let end_col = last_content_col + 1;

        let mut last_style: Option<(Color, Modifier)> = None;

        for col in area.x..end_col {
            let cell = &buf[(col, row)];
            let style = (cell.fg, cell.modifier);

            if Some(style) != last_style {
                out.push_str("\x1b[0m");
                if let Color::Rgb(r, g, b) = cell.fg {
                    out.push_str(&format!("\x1b[38;2;{r};{g};{b}m"));
                }
                if cell.modifier.contains(Modifier::BOLD) {
                    out.push_str("\x1b[1m");
                }
                last_style = Some(style);
            }

            out.push_str(cell.symbol());
        }

        out.push_str("\x1b[0m");

        if row < area.y + area.height - 1 {
            out.push('\n');
        }
    }

    out
}

/// Braille spinner frames for active phases (~100ms per frame = 1s cycle).
const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Render `&[Line]` into a ratatui `Buffer`.
///
/// `tick` drives spinner animation — incremented each update cycle.
/// Defensive: guards against out-of-bounds by intersecting with buffer area
/// and clamping line count to available rows.
pub fn render_lines(lines: &[Line], buf: &mut Buffer, area: Rect, theme: &Theme, tick: u64) {
    let area = area.intersection(*buf.area());
    if area.is_empty() {
        return;
    }

    for (row_idx, line) in lines.iter().enumerate().take(area.height as usize) {
        let y = area.y + row_idx as u16;
        let x = area.x + line.indent as u16 * 4;

        // If dimmed, override all colors to theme.dim
        let dim_style = Style::default().fg(theme.dim);

        match line.style {
            LineStyle::PhaseHeader { active } => {
                let icon_style = if line.dimmed {
                    dim_style
                } else if active {
                    Style::default().fg(theme.yellow)
                } else {
                    theme.dim_style()
                };
                let name_style = if line.dimmed {
                    dim_style
                } else {
                    Style::default().fg(theme.hi).add_modifier(Modifier::BOLD)
                };

                let mut col = x;
                if active {
                    let frame = SPINNER_FRAMES[tick as usize % SPINNER_FRAMES.len()];
                    buf.set_string(col, y, frame, icon_style);
                    col += 1;
                    // Pad to align with fullwidth 合 (2 cells)
                    buf.set_string(col, y, " ", icon_style);
                    col += 1;
                } else {
                    buf.set_string(col, y, "合", icon_style);
                    col += 2; // fullwidth
                }
                buf.set_string(col, y, " ", name_style);
                col += 1;
                buf.set_string(col, y, &line.text, name_style);
                // meta (elapsed time) handled by fallthrough below
            }
            LineStyle::PhaseHeaderFailed => {
                let icon_style = if line.dimmed {
                    dim_style
                } else {
                    Style::default().fg(theme.red).add_modifier(Modifier::BOLD)
                };
                let text_style = icon_style; // same red+bold for failed

                buf.set_string(x, y, "合", icon_style);
                let col = x + 2; // fullwidth
                buf.set_string(col, y, " ", text_style);
                buf.set_string(col + 1, y, &line.text, text_style);
                // meta handled by fallthrough below
            }
            LineStyle::SubtaskHeader => {
                let style_id = if line.dimmed {
                    dim_style
                } else {
                    theme.dim_style()
                };
                let style_title = if line.dimmed {
                    dim_style
                } else {
                    theme.fg_style()
                };

                if let Some(bracket_end) = line.text.find("] ") {
                    let (id_part, title_part) = line.text.split_at(bracket_end + 2);
                    buf.set_string(x, y, id_part, style_id);
                    buf.set_string(x + id_part.len() as u16, y, title_part, style_title);
                } else {
                    buf.set_string(x, y, &line.text, style_title);
                }
            }
            LineStyle::Child => {
                let tree_style = if line.dimmed {
                    dim_style
                } else {
                    theme.dim_style()
                };
                let text_style = if line.dimmed {
                    dim_style
                } else {
                    theme.dim_style()
                };

                buf.set_string(x, y, "⎿", tree_style);
                buf.set_string(x + 1, y, " ", tree_style);
                buf.set_string(x + 2, y, &line.text, text_style);
            }
            LineStyle::ChildActive => {
                let tree_style = if line.dimmed {
                    dim_style
                } else {
                    theme.dim_style()
                };
                let text_style = if line.dimmed {
                    dim_style
                } else {
                    Style::default().fg(theme.yellow)
                };

                buf.set_string(x, y, "⎿", tree_style);
                buf.set_string(x + 1, y, " ", tree_style);
                buf.set_string(x + 2, y, &line.text, text_style);
            }
            LineStyle::ChildDone => {
                let tree_style = if line.dimmed {
                    dim_style
                } else {
                    theme.dim_style()
                };
                let check_style = if line.dimmed {
                    dim_style
                } else {
                    Style::default().fg(theme.green)
                };
                let text_style = if line.dimmed {
                    dim_style
                } else {
                    theme.dim_style()
                };

                buf.set_string(x, y, "⎿", tree_style);
                buf.set_string(x + 1, y, " ", tree_style);
                buf.set_string(x + 2, y, theme::SYM_CHECK, check_style);
                buf.set_string(x + 3, y, " ", text_style);
                buf.set_string(x + 4, y, &line.text, text_style);
            }
            LineStyle::ChildError => {
                let tree_style = if line.dimmed {
                    dim_style
                } else {
                    theme.dim_style()
                };
                let icon_style = if line.dimmed {
                    dim_style
                } else {
                    Style::default().fg(theme.red)
                };
                let text_style = if line.dimmed {
                    dim_style
                } else {
                    Style::default().fg(theme.red)
                };

                buf.set_string(x, y, "⎿", tree_style);
                buf.set_string(x + 1, y, " ", tree_style);
                buf.set_string(x + 2, y, theme::SYM_FAILED, icon_style);
                buf.set_string(x + 3, y, " ", text_style);
                buf.set_string(x + 4, y, &line.text, text_style);
            }
            LineStyle::ChildWarning => {
                let tree_style = if line.dimmed {
                    dim_style
                } else {
                    theme.dim_style()
                };
                let warn_style = if line.dimmed {
                    dim_style
                } else {
                    Style::default().fg(theme.yellow)
                };
                let text_style = if line.dimmed {
                    dim_style
                } else {
                    Style::default().fg(theme.hi).add_modifier(Modifier::BOLD)
                };

                buf.set_string(x, y, "⎿", tree_style);
                buf.set_string(x + 1, y, " ", tree_style);
                buf.set_string(x + 2, y, "⚠", warn_style);
                buf.set_string(x + 3, y, " ", text_style);
                buf.set_string(x + 4, y, &line.text, text_style);
            }
            LineStyle::ChildBold => {
                let tree_style = if line.dimmed {
                    dim_style
                } else {
                    theme.dim_style()
                };
                let text_style = if line.dimmed {
                    dim_style
                } else {
                    Style::default().fg(theme.hi).add_modifier(Modifier::BOLD)
                };

                buf.set_string(x, y, "⎿", tree_style);
                buf.set_string(x + 1, y, " ", tree_style);
                buf.set_string(x + 2, y, &line.text, text_style);
            }
            LineStyle::Subtask { status } => {
                let (icon, icon_style) = match status {
                    SubtaskStatus::PendingUnassigned => (
                        theme::SYM_PENDING_UNASSIGNED,
                        if line.dimmed {
                            dim_style
                        } else {
                            theme.dim_style()
                        },
                    ),
                    SubtaskStatus::Pending => (
                        theme::SYM_PENDING,
                        if line.dimmed {
                            dim_style
                        } else {
                            theme.dim_style()
                        },
                    ),
                    SubtaskStatus::Assigned => (
                        theme::SYM_STARTING,
                        if line.dimmed {
                            dim_style
                        } else {
                            Style::default().fg(theme.yellow)
                        },
                    ),
                    SubtaskStatus::Active => (
                        theme::SYM_RUNNING,
                        if line.dimmed {
                            dim_style
                        } else {
                            Style::default().fg(theme.yellow)
                        },
                    ),
                    SubtaskStatus::Done => (
                        theme::SYM_CHECK,
                        if line.dimmed {
                            dim_style
                        } else {
                            Style::default().fg(theme.green)
                        },
                    ),
                    SubtaskStatus::Failed => (
                        theme::SYM_FAILED,
                        if line.dimmed {
                            dim_style
                        } else {
                            Style::default().fg(theme.red)
                        },
                    ),
                };

                let text_style = if line.dimmed {
                    dim_style
                } else {
                    match status {
                        SubtaskStatus::Active => Style::default().fg(theme.fg),
                        SubtaskStatus::Failed => Style::default().fg(theme.red),
                        // Done, Pending, Assigned, PendingUnassigned — all dim
                        _ => theme.dim_style(),
                    }
                };

                buf.set_string(x, y, icon, icon_style);
                buf.set_string(x + 1, y, " ", text_style);
                buf.set_string(x + 2, y, &line.text, text_style);

                // Right-align elapsed time (meta) if present
                if let Some(ref meta) = line.meta {
                    let meta_style = if line.dimmed {
                        dim_style
                    } else {
                        theme.dim_style()
                    };
                    let meta_x = area.x + area.width - meta.len() as u16;
                    buf.set_string(meta_x, y, meta, meta_style);
                }
            }
            LineStyle::Separator => {
                let style = if line.dimmed {
                    dim_style
                } else {
                    theme.dim_style()
                };
                buf.set_string(x, y, "---", style);
            }
            LineStyle::SectionHeader => {
                let style = if line.dimmed {
                    dim_style
                } else {
                    Style::default().fg(theme.hi).add_modifier(Modifier::BOLD)
                };
                buf.set_string(x, y, &line.text, style);
            }
            LineStyle::Issue => {
                let style = if line.dimmed {
                    dim_style
                } else {
                    Style::default().fg(theme.fg)
                };
                buf.set_string(x, y, &line.text, style);
            }
            LineStyle::Dim => {
                let style = if line.dimmed {
                    dim_style
                } else {
                    theme.dim_style()
                };
                buf.set_string(x, y, &line.text, style);
            }
            LineStyle::Blank => {
                // Empty line — nothing to render
            }
        }

        // Render right-aligned meta for styles that use it (except Subtask which handles it inline)
        if !matches!(
            line.style,
            LineStyle::Subtask { .. } | LineStyle::SubtaskHeader | LineStyle::Blank
        ) {
            if let Some(ref meta) = line.meta {
                let meta_style = if line.dimmed {
                    dim_style
                } else {
                    theme.dim_style()
                };
                let meta_x = area.x + area.width - meta.len() as u16;
                buf.set_string(meta_x, y, meta, meta_style);
            }
        }
    }
}

/// Returns the number of terminal rows needed to render the lines.
/// Each Line is exactly one terminal row.
pub fn lines_height(lines: &[Line]) -> u16 {
    lines.len() as u16
}

/// One-shot render: convert `&[Line]` to a styled ANSI string for terminal output.
///
/// Creates a fixed-width (80 col) buffer, renders the lines with the given theme,
/// and returns the ANSI-escaped string. Applies progressive dimming before rendering.
pub fn render_to_string(lines: &mut Vec<Line>, theme: &Theme) -> String {
    render_to_string_ex(lines, theme, 80, 0)
}

/// One-shot render with configurable width and tick.
///
/// `tick` drives spinner animation, `width` sets the buffer column count.
pub fn render_to_string_ex(lines: &mut Vec<Line>, theme: &Theme, width: u16, tick: u64) -> String {
    apply_dimming(lines);
    let height = lines_height(lines);
    if height == 0 {
        return String::new();
    }
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    render_lines(lines, &mut buf, area, theme, tick);
    buffer_to_ansi(&buf)
}

/// Progressive dimming: dims all lines in groups before the active group.
///
/// Groups are monotonically increasing. Only one phase can be "active" at a time.
/// If concurrent active phases are ever needed, replace this with explicit PhaseState.
pub fn apply_dimming(lines: &mut [Line]) {
    // Find the highest group number that has a ChildActive line
    let active_group = lines
        .iter()
        .filter(|l| matches!(l.style, LineStyle::ChildActive))
        .map(|l| l.group)
        .max();

    if let Some(active) = active_group {
        for line in lines.iter_mut() {
            // Dim lines in earlier groups, but never dim Issue lines
            if line.group < active && !matches!(line.style, LineStyle::Issue) {
                line.dimmed = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    #[test]
    fn apply_dimming_dims_groups_before_active() {
        let mut lines = vec![
            Line {
                indent: 0,
                text: "old".into(),
                meta: None,
                style: LineStyle::Child,
                group: 0,
                dimmed: false,
            },
            Line {
                indent: 0,
                text: "current".into(),
                meta: None,
                style: LineStyle::ChildActive,
                group: 1,
                dimmed: false,
            },
        ];
        apply_dimming(&mut lines);
        assert!(lines[0].dimmed);
        assert!(!lines[1].dimmed);
    }

    #[test]
    fn apply_dimming_preserves_issue_lines() {
        let mut lines = vec![
            Line {
                indent: 0,
                text: "1. issue".into(),
                meta: None,
                style: LineStyle::Issue,
                group: 0,
                dimmed: false,
            },
            Line {
                indent: 0,
                text: "current".into(),
                meta: None,
                style: LineStyle::ChildActive,
                group: 1,
                dimmed: false,
            },
        ];
        apply_dimming(&mut lines);
        assert!(!lines[0].dimmed); // Issues never dim
    }

    #[test]
    fn apply_dimming_noop_without_active() {
        let mut lines = vec![Line {
            indent: 0,
            text: "child".into(),
            meta: None,
            style: LineStyle::Child,
            group: 0,
            dimmed: false,
        }];
        apply_dimming(&mut lines);
        assert!(!lines[0].dimmed);
    }

    #[test]
    fn render_done_phase_header_shows_gou() {
        let lines = vec![Line {
            indent: 0,
            text: "plan (claude)".into(),
            meta: None,
            style: LineStyle::PhaseHeader { active: false },
            group: 0,
            dimmed: false,
        }];
        let area = Rect::new(0, 0, 40, 5);
        let mut buf = Buffer::empty(area);
        render_lines(&lines, &mut buf, area, &Theme::dark(), 0);
        // First visible char should be 合
        let cell = &buf[(0, 0)];
        assert_eq!(cell.symbol(), "合");
    }

    #[test]
    fn render_active_phase_header_shows_spinner() {
        let lines = vec![Line {
            indent: 0,
            text: "plan (claude)".into(),
            meta: None,
            style: LineStyle::PhaseHeader { active: true },
            group: 0,
            dimmed: false,
        }];
        let area = Rect::new(0, 0, 40, 5);
        let mut buf = Buffer::empty(area);
        render_lines(&lines, &mut buf, area, &Theme::dark(), 0);
        // First frame should be ⠋
        let cell = &buf[(0, 0)];
        assert_eq!(cell.symbol(), "⠋");
    }

    #[test]
    fn render_spinner_advances_with_tick() {
        let lines = vec![Line {
            indent: 0,
            text: "plan".into(),
            meta: None,
            style: LineStyle::PhaseHeader { active: true },
            group: 0,
            dimmed: false,
        }];
        let area = Rect::new(0, 0, 40, 5);
        let mut buf = Buffer::empty(area);
        render_lines(&lines, &mut buf, area, &Theme::dark(), 3);
        // Frame 3 should be ⠸
        let cell = &buf[(0, 0)];
        assert_eq!(cell.symbol(), "⠸");
    }

    #[test]
    fn render_spinner_and_gou_text_align() {
        // Both active (spinner) and done (合) should start the name at the same column.
        let theme = Theme::dark();
        let area = Rect::new(0, 0, 40, 2);

        let active_line = Line {
            indent: 0,
            text: "plan".into(),
            meta: None,
            style: LineStyle::PhaseHeader { active: true },
            group: 0,
            dimmed: false,
        };
        let done_line = Line {
            indent: 0,
            text: "plan".into(),
            meta: None,
            style: LineStyle::PhaseHeader { active: false },
            group: 0,
            dimmed: false,
        };

        let mut buf_active = Buffer::empty(area);
        render_lines(&[active_line], &mut buf_active, area, &theme, 0);

        let mut buf_done = Buffer::empty(area);
        render_lines(&[done_line], &mut buf_done, area, &theme, 0);

        // Find where "plan" starts in each buffer by checking col 3
        // Active: ⠋ (col 0) + space (col 1) + space (col 2) + "p" (col 3)
        // Done:   合 (cols 0-1) + space (col 2) + "p" (col 3)
        assert_eq!(buf_active[(3, 0)].symbol(), "p");
        assert_eq!(buf_done[(3, 0)].symbol(), "p");
    }

    #[test]
    fn render_respects_area_bounds() {
        // 20 lines into 5-row area should not panic
        let lines: Vec<Line> = (0..20)
            .map(|i| Line {
                indent: 0,
                text: format!("line {i}"),
                meta: None,
                style: LineStyle::Child,
                group: 0,
                dimmed: false,
            })
            .collect();
        let area = Rect::new(0, 0, 80, 5);
        let mut buf = Buffer::empty(area);
        render_lines(&lines, &mut buf, area, &Theme::dark(), 0);
    }

    #[test]
    fn lines_height_returns_line_count() {
        let lines = vec![
            Line {
                indent: 0,
                text: "a".into(),
                meta: None,
                style: LineStyle::Blank,
                group: 0,
                dimmed: false,
            },
            Line {
                indent: 0,
                text: "b".into(),
                meta: None,
                style: LineStyle::Blank,
                group: 0,
                dimmed: false,
            },
        ];
        assert_eq!(lines_height(&lines), 2);
    }

    #[test]
    fn render_subtask_failed_text_has_red_fg() {
        let theme = Theme::dark();
        let lines = vec![Line {
            indent: 0,
            text: "broken task".into(),
            meta: None,
            style: LineStyle::Subtask {
                status: SubtaskStatus::Failed,
            },
            group: 0,
            dimmed: false,
        }];
        let area = Rect::new(0, 0, 40, 5);
        let mut buf = Buffer::empty(area);
        render_lines(&lines, &mut buf, area, &theme, 0);
        // Text starts at col 2 (icon + space)
        let cell = &buf[(2, 0)];
        assert_eq!(cell.fg, theme.red);
    }

    #[test]
    fn render_subtask_done_text_has_dim_fg() {
        let theme = Theme::dark();
        let lines = vec![Line {
            indent: 0,
            text: "finished task".into(),
            meta: None,
            style: LineStyle::Subtask {
                status: SubtaskStatus::Done,
            },
            group: 0,
            dimmed: false,
        }];
        let area = Rect::new(0, 0, 40, 5);
        let mut buf = Buffer::empty(area);
        render_lines(&lines, &mut buf, area, &theme, 0);
        let cell = &buf[(2, 0)];
        assert_eq!(cell.fg, theme.dim);
    }

    #[test]
    fn render_subtask_active_text_has_fg_color() {
        let theme = Theme::dark();
        let lines = vec![Line {
            indent: 0,
            text: "running task".into(),
            meta: None,
            style: LineStyle::Subtask {
                status: SubtaskStatus::Active,
            },
            group: 0,
            dimmed: false,
        }];
        let area = Rect::new(0, 0, 40, 5);
        let mut buf = Buffer::empty(area);
        render_lines(&lines, &mut buf, area, &theme, 0);
        let cell = &buf[(2, 0)];
        assert_eq!(cell.fg, theme.fg);
    }

    #[test]
    fn render_subtask_pending_unassigned_shows_circle() {
        let theme = Theme::dark();
        let lines = vec![Line {
            indent: 0,
            text: "unclaimed task".into(),
            meta: None,
            style: LineStyle::Subtask {
                status: SubtaskStatus::PendingUnassigned,
            },
            group: 0,
            dimmed: false,
        }];
        let area = Rect::new(0, 0, 40, 5);
        let mut buf = Buffer::empty(area);
        render_lines(&lines, &mut buf, area, &theme, 0);
        let cell = &buf[(0, 0)];
        assert_eq!(cell.symbol(), "◌");
    }

    #[test]
    fn render_subtask_pending_shows_circle() {
        let theme = Theme::dark();
        let lines = vec![Line {
            indent: 0,
            text: "pending task".into(),
            meta: None,
            style: LineStyle::Subtask {
                status: SubtaskStatus::Pending,
            },
            group: 0,
            dimmed: false,
        }];
        let area = Rect::new(0, 0, 40, 5);
        let mut buf = Buffer::empty(area);
        render_lines(&lines, &mut buf, area, &theme, 0);
        let cell = &buf[(0, 0)];
        assert_eq!(cell.symbol(), "○");
    }

    #[test]
    fn render_subtask_header_shows_bracket_not_gou() {
        let lines = vec![Line {
            indent: 0,
            text: "[lkji3d] Epic: Test".into(),
            meta: None,
            style: LineStyle::SubtaskHeader,
            group: 0,
            dimmed: false,
        }];
        let area = Rect::new(0, 0, 40, 5);
        let mut buf = Buffer::empty(area);
        render_lines(&lines, &mut buf, area, &Theme::dark(), 0);
        // First cell should be `[` (not `合`)
        let cell = &buf[(0, 0)];
        assert_eq!(cell.symbol(), "[");
    }
}
