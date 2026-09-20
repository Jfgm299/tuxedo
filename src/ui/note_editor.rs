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
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::App;
use crate::theme::Theme;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme();
    let Some(editor) = app.notes_popup.active_editor.as_ref() else {
        return;
    };

    let file_name = editor
        .path()
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| editor.path().display().to_string());

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border).bg(theme.panel))
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
