# TUI System

Ratatui-based terminal UI for aiki. Uses an Elm architecture (Model → update → view → render) with character-cell buffers and 24-bit color ANSI output.

## Architecture

```
app.rs            Elm core: Model, Msg, update(), run() event loop
components.rs     Reusable view components returning Vec<Line>
render.rs         Line renderer + buffer_to_ansi converter
theme.rs          Color palette (dark/light) + symbols + auto-detection
panic_hook.rs     Terminal cleanup on panic

screens/          Screen-specific view functions (each returns Vec<Line>)
  build.rs        Build pipeline: plan → decompose → loop → review → fix
  epic_show.rs    Epic detail with subtask table
  review.rs       Review progress
  review_show.rs  Review detail with issues
  fix.rs          Fix pipeline progress
  task_run.rs     Single task execution
  helpers.rs      Shared query helpers for screen functions
```

## How it works

1. **Model** — `app.rs` defines `Model` (graph + screen + window state) and `Msg` (events)
2. **Update** — `update()` handles messages: graph refresh, key input, resize, tick
3. **View** — Screen functions in `screens/` convert `Model` into `Vec<Line>`
4. **Render** — `render.rs` renders `Vec<Line>` into a ratatui `Buffer`, then `buffer_to_ansi()` emits ANSI escapes

The Elm loop runs in `app::run()` using a crossterm alternate-screen backend with inline viewport.

## Key types

- `Line` — one terminal row with text, style, indent, group, meta (right-aligned), dimmed flag
- `LineStyle` — rendering variant (PhaseHeader, Child, Subtask, SectionHeader, Issue, etc.)
- `Screen` — which view to render (Build, Review, Fix, TaskRun, EpicShow, ReviewShow)
- `Model` — graph + screen + window state
- `Msg` — graph update, key event, resize, tick

## Adding a new screen

1. Create `screens/my_screen.rs` with `pub fn view(graph: &TaskGraph, ..., window: &WindowState) -> Vec<Line>`
2. Add `pub mod my_screen;` to `screens/mod.rs`
3. Add a `Screen::MyScreen { ... }` variant to `app.rs`
4. Wire the variant in `app.rs`'s view dispatch and exit-condition logic
5. Build `Vec<Line>` using `components::phase()`, `components::subtask_table()`, etc.

## Components

`components.rs` provides reusable builders:
- `phase()` — phase header with child lines (maps to PhaseHeader + Child* styles)
- `subtask_table()` — subtask list with status icons
- `section_header()` — bold section divider
- `separator()` — dim horizontal rule

## Theme

Two modes: dark and light. Auto-detected via `terminal-colorsaurus` (OSC 11), overridable with `AIKI_THEME=light|dark`.

**Semantic colors:** green (done), cyan (info), yellow (active), red (failed), magenta (cursor agent), dim (borders), fg (supporting text), text (primary), hi (headers, bold).

**Symbols:** `✓` check, `▸` running, `○` pending, `✗` failed.

Widgets accept `&Theme` and use `theme.dim_style()`, `theme.text_style()`, etc. Never hard-code colors.

## Testing

Views are tested with insta text snapshots and buffer assertions:

```rust
let theme = Theme::dark();
let window = WindowState::new(80);
let mut lines = screens::build::view(&graph, &epic_id, &plan_path, &window);
let output = render::render_to_string(&mut lines, &theme);
insta::assert_snapshot!(output);
```

Buffer-level tests for render.rs:
```rust
let area = Rect::new(0, 0, 80, 10);
let mut buf = Buffer::empty(area);
render_lines(&lines, &mut buf, area, &theme, 0);
let cell = &buf[(0, 0)];
assert_eq!(cell.symbol(), "✓");
```
