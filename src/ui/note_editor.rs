//! Rendering for the embedded note editor
//! (`NotesPopupState::active_editor`). Shown in the same popup area
//! `notes_popup::render` uses for the file list, swapped in by
//! `src/ui/mod.rs`'s `Mode::Notes` arm whenever a file is open for editing.
//! Same bordered-box chrome as `notes_popup::render`/`dialog::render` (both
//! border and title styled from `app.theme()`), title switched to the open
//! file's name (mirrors `dialog::render`'s ADD/EDIT TASK title switch). The
//! cursor is shown as a single highlighted cell rather than a separate
//! cursor widget — simplest correct MVP approach. Normal vs Insert sub-mode
//! is surfaced through `src/ui/status.rs`'s mode label/hint line, the same
//! place `Mode::Insert`'s `DialogInputMode` already shows it — no bespoke
//! mode-indicator widget here.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{App, NoteEditorState};
use crate::theme::Theme;

/// Render the floating popup's embedded editor
/// (`NotesPopupState::active_editor`). Thin wrapper over [`render_editor`]:
/// the floating popup is always the sole keyboard target while open, so it
/// always renders as "focused" (accent border) — there's no unfocused state
/// for it the way there is for T11's pinned/docked note.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let Some(editor) = app.notes_popup.active_editor.as_ref() else {
        return;
    };
    render_editor(frame, area, app.theme(), editor, true);
}

/// Render T11+T12's right-docked pinned panel: `app.pinned_notes` (called
/// only when non-empty — see `src/ui/mod.rs::draw()`), preceded by a
/// one-line tab strip whenever there's more than one pinned note. A tab bar
/// for exactly one pinned note would be unnecessary chrome for what's still
/// the common case (T11's original single-pin usage), so it's only shown
/// once there's actually something to distinguish between — mirrors how
/// `NotesPopupState`'s own list doesn't need selection UI when it holds a
/// single row either. `focused` (border color, from `render_editor`) still
/// tracks `app.pinned_focus` exactly as it did in T11, applied to whichever
/// tab is currently active.
pub fn render_pinned(frame: &mut Frame, area: Rect, theme: &Theme, app: &App) {
    let Some(active) = app.pinned_notes.get(app.active_pin) else {
        return;
    };
    if app.pinned_notes.len() > 1 {
        let [tab_area, editor_area] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
        render_tab_bar(frame, tab_area, theme, app);
        render_editor(frame, editor_area, theme, active, app.pinned_focus);
    } else {
        render_editor(frame, area, theme, active, app.pinned_focus);
    }
}

/// One-line strip of tab labels (filenames), the active tab visually
/// distinguished with the same selection-highlight convention
/// `src/ui/notes_popup.rs`'s list already uses for its selected row
/// (`theme.cursor` background) rather than inventing new visual language.
fn render_tab_bar(frame: &mut Frame, area: Rect, theme: &Theme, app: &App) {
    let spans: Vec<Span> = app
        .pinned_notes
        .iter()
        .enumerate()
        .map(|(i, note)| {
            let name = note
                .path()
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| note.path().display().to_string());
            let is_active = i == app.active_pin;
            let style = if is_active {
                Style::default()
                    .fg(theme.fg)
                    .bg(theme.cursor)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.dim).bg(theme.panel)
            };
            Span::styled(format!(" {name} "), style)
        })
        .collect();
    let line = Line::from(spans).style(Style::default().bg(theme.panel));
    frame.render_widget(
        Paragraph::new(line).style(Style::default().bg(theme.panel)),
        area,
    );
}

/// Render `editor`'s buffer into `area` using this bordered-box chrome,
/// shared by both the floating popup (`render` above, always `focused`) and
/// T11+T12's right-docked pinned tabs (`app.pinned_notes`, `focused` tracks
/// `app.pinned_focus`, applied to whichever tab is active). `focused` picks
/// the border color — an accent border
/// when this editor currently has keyboard focus, a dim default border when
/// it's pinned-but-unfocused — so it's clear at a glance where keystrokes are
/// going.
pub fn render_editor(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    editor: &NoteEditorState,
    focused: bool,
) {
    let file_name = editor
        .path()
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| editor.path().display().to_string());

    let border_color = if focused { theme.accent } else { theme.border };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color).bg(theme.panel))
        .title(Line::from(vec![Span::styled(
            format!(" {file_name} "),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )]))
        .style(Style::default().bg(theme.panel));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let height = inner.height as usize;
    let cursor_line = editor.cursor_line();
    let cursor_col = editor.cursor_col();
    let lines = editor.lines();

    // Viewport-follows-cursor scrolling, recomputed fresh every frame from
    // just the cursor position and the visible height: no persisted scroll
    // offset to keep in sync. Scrolls down only as far as needed to keep the
    // cursor's line on screen; scrolling back up happens for free once the
    // cursor line is inside `0..height` again.
    let top = if height > 0 && cursor_line >= height {
        cursor_line + 1 - height
    } else {
        0
    };
    let end = (top + height).min(lines.len());

    let rendered: Vec<Line> = lines[top..end]
        .iter()
        .enumerate()
        .map(|(i, text)| {
            let abs_line = top + i;
            render_line(theme, text, abs_line == cursor_line, cursor_col)
        })
        .collect();

    frame.render_widget(
        Paragraph::new(rendered).style(Style::default().bg(theme.panel)),
        inner,
    );
}

/// Render one buffer line, highlighting the cursor's character cell (or a
/// single blank cell past the end of the line) when `is_cursor_line`.
fn render_line(
    theme: &Theme,
    text: &str,
    is_cursor_line: bool,
    cursor_col: usize,
) -> Line<'static> {
    let base = Style::default().fg(theme.fg).bg(theme.panel);
    if !is_cursor_line {
        return Line::from(Span::styled(text.to_string(), base));
    }

    let chars: Vec<char> = text.chars().collect();
    let mut spans = Vec::new();
    if cursor_col > 0 {
        let before: String = chars[..cursor_col.min(chars.len())].iter().collect();
        spans.push(Span::styled(before, base));
    }
    if cursor_col < chars.len() {
        spans.push(Span::styled(
            chars[cursor_col].to_string(),
            Style::default().fg(theme.panel).bg(theme.cursor),
        ));
        if cursor_col + 1 < chars.len() {
            let after: String = chars[cursor_col + 1..].iter().collect();
            spans.push(Span::styled(after, base));
        }
    } else {
        // Cursor sits past the last character (end of line) — render one
        // highlighted blank cell so the cursor is still visible.
        spans.push(Span::styled(" ", Style::default().bg(theme.cursor)));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::app::test_support::build_app;
    use crate::app::{NoteEditorMode, NoteEditorState};

    fn rendered_text(area_w: u16, area_h: u16, render: impl FnOnce(&mut Frame, Rect)) -> String {
        let backend = TestBackend::new(area_w, area_h);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| render(f, f.area())).expect("draw");
        let buf = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    fn note(name: &str, dir: &std::path::Path) -> NoteEditorState {
        let path = dir.join(name);
        std::fs::write(&path, "body").expect("write note file");
        NoteEditorState::load(path, NoteEditorMode::Normal)
    }

    // ---- T12: tab bar only appears once there's something to distinguish --

    #[test]
    fn render_pinned_shows_no_tab_bar_for_a_single_pinned_note() {
        let dir = std::env::temp_dir().join(format!(
            "tuxedo-note-editor-render-single-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("create dir");
        let mut app = build_app("Buy milk\n");
        app.pinned_notes.push(note("solo.md", &dir));

        let text = rendered_text(40, 8, |f, area| {
            render_pinned(f, area, app.theme(), &app);
        });

        // No tab-bar row: the bordered editor block starts at row 0, so its
        // top-left corner is a border-drawing character, not blank text or a
        // second occurrence of the filename above the title.
        assert_eq!(
            text.matches("solo.md").count(),
            1,
            "filename appears exactly once (the editor title only, no tab strip): {text}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_pinned_shows_a_tab_bar_listing_every_tab_when_more_than_one_is_pinned() {
        let dir = std::env::temp_dir().join(format!(
            "tuxedo-note-editor-render-multi-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("create dir");
        let mut app = build_app("Buy milk\n");
        app.pinned_notes.push(note("first.md", &dir));
        app.pinned_notes.push(note("second.md", &dir));
        app.active_pin = 1;

        let text = rendered_text(40, 8, |f, area| {
            render_pinned(f, area, app.theme(), &app);
        });

        assert!(
            text.contains("first.md"),
            "inactive tab label shown: {text}"
        );
        // "second.md" appears twice: once in the tab strip, once again as
        // the active editor's own title (unchanged from `render_editor`).
        assert_eq!(
            text.matches("second.md").count(),
            2,
            "active tab shown in the strip AND as the editor's title: {text}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_pinned_highlights_the_active_tab_with_the_cursor_theme_color() {
        let dir = std::env::temp_dir().join(format!(
            "tuxedo-note-editor-render-highlight-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("create dir");
        let mut app = build_app("Buy milk\n");
        app.pinned_notes.push(note("first.md", &dir));
        app.pinned_notes.push(note("second.md", &dir));
        app.active_pin = 0;
        let theme = app.theme();

        let backend = TestBackend::new(40, 8);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                let area = f.area();
                render_pinned(f, area, app.theme(), &app);
            })
            .expect("draw");
        let buf = terminal.backend().buffer();

        // Row 0 is the tab strip. Somewhere in it, the active tab's label
        // ("first.md", tab 0) must be painted with the same background the
        // rest of this app uses for a selected row (theme.cursor) --
        // matches src/ui/notes_popup.rs's own selection-highlight
        // convention rather than inventing a new one.
        let active_tab_highlighted = (0..buf.area.width).any(|x| buf[(x, 0)].bg == theme.cursor);
        assert!(
            active_tab_highlighted,
            "expected some cell in the tab strip painted with theme.cursor"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
