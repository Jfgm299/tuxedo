//! Floating notes-list popup (`Mode::Notes`). Styled like `dialog::render`'s
//! bordered box: same border/title chrome, colors pulled from `app.theme()`.
//! Lists the current task's `.md` files with the cursor row highlighted;
//! `n`/`r` open an inline name prompt (create/rename) in place of the list,
//! `d` opens an inline "Delete <name>? (y/n)" confirmation in place of the
//! list. `e`/`i` on the selected row open it into the embedded editor
//! instead (`src/ui/note_editor.rs`, swapped in by `src/ui/mod.rs`'s
//! `Mode::Notes` arm whenever `NotesPopupState::active_editor` is `Some`).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{App, NotePromptKind};

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border).bg(theme.panel))
        .title(Line::from(vec![Span::styled(
            " NOTES ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )]))
        .style(Style::default().bg(theme.panel));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let state = &app.notes_popup;
    if let Some(input) = &state.prompt {
        let label = match state.prompt_kind {
            Some(NotePromptKind::Rename { .. }) => "  Rename note (.md added automatically)",
            _ => "  New note name (.md added automatically)",
        };
        let lines = vec![
            Line::from(vec![Span::styled(label, Style::default().fg(theme.dim))]),
            Line::from(vec![Span::styled(
                format!("  > {input}"),
                Style::default().fg(theme.fg),
            )]),
        ];
        frame.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme.panel)),
            inner,
        );
        return;
    }

    if let Some(index) = state.pending_delete {
        let name = state
            .files
            .get(index)
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let line = Line::from(vec![Span::styled(
            format!("  Delete {name}? (y/n)"),
            Style::default()
                .fg(theme.overdue)
                .add_modifier(Modifier::BOLD),
        )]);
        frame.render_widget(
            Paragraph::new(line).style(Style::default().bg(theme.panel)),
            inner,
        );
        return;
    }

    if state.files.is_empty() {
        let line = Line::from(vec![Span::styled(
            "  No notes yet",
            Style::default().fg(theme.dim),
        )])
        .style(Style::default().bg(theme.panel));
        frame.render_widget(
            Paragraph::new(line).style(Style::default().bg(theme.panel)),
            inner,
        );
        return;
    }

    let lines: Vec<Line> = state
        .files
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            let is_sel = i == state.cursor;
            let bg = if is_sel { theme.cursor } else { theme.panel };
            let style = Style::default().fg(theme.fg).bg(bg);
            Line::from(vec![Span::styled(format!("  {name}"), style)])
                .style(Style::default().bg(bg))
        })
        .collect();

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme.panel)),
        inner,
    );
}
