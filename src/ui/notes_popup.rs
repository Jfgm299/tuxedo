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
use crate::ui::dialog::draft_cursor_spans;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme();
    let state = &app.notes_popup;

    // Mirrors `dialog::render`'s ADD TASK / EDIT TASK title switch: the
    // bordered box's own title communicates create-vs-rename instead of an
    // explanatory line inside the box.
    let title = match state.prompt_kind {
        Some(NotePromptKind::Create) => " NEW NOTE ",
        Some(NotePromptKind::Rename { .. }) => " RENAME NOTE ",
        None => " NOTES ",
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border).bg(theme.panel))
        .title(Line::from(vec![Span::styled(
            title,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )]))
        .style(Style::default().bg(theme.panel));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if let Some(input) = &state.prompt {
        // Same `  › ` input-line treatment as the ADD TASK dialog's input
        // row (`dialog::render`), including the cursor glyph — the prompt
        // buffer's cursor is always at the end (append/backspace only, no
        // mid-string editing), so `input.len()` is always the correct
        // cursor position here.
        let mut spans = vec![
            Span::raw("  "),
            Span::styled(
                "› ",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        spans.extend(draft_cursor_spans(
            input,
            input.len(),
            theme.fg,
            theme.panel,
        ));
        let line = Line::from(spans).style(Style::default().bg(theme.panel));
        frame.render_widget(
            Paragraph::new(line).style(Style::default().bg(theme.panel)),
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

    // Round 3 feedback: adopt `task_row.rs::build_line`'s row convention
    // instead of a bare background swap — a leading glyph column (same
    // `"▸ "`/`"  "` glyph, same `theme.accent` coloring) plus the background
    // swap, so the selected row reads unambiguously via both signals, not
    // just one.
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
            let glyph = if is_sel { "▸ " } else { "  " };
            let mut glyph_style = Style::default().fg(theme.accent).bg(bg);
            if is_sel {
                glyph_style = glyph_style.add_modifier(Modifier::BOLD);
            }
            let name_style = Style::default().fg(theme.fg).bg(bg);
            Line::from(vec![
                Span::styled(glyph, glyph_style),
                Span::styled(name, name_style),
            ])
            .style(Style::default().bg(bg))
        })
        .collect();

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme.panel)),
        inner,
    );
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::app::{App, Mode, NotePromptKind, NotesPopupState};
    use crate::config::Config;

    fn build_app() -> App {
        let path = std::env::temp_dir().join(format!(
            "tuxedo-notes-popup-test-{}.txt",
            std::process::id()
        ));
        let body = "(A) Buy milk\n".to_string();
        std::fs::write(&path, &body).unwrap();
        let mut app = App::new(path, body, "2026-05-06".to_string(), Config::default());
        app.mode = Mode::Notes;
        app
    }

    fn render_popup(app: &App) -> String {
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| super::render(f, f.area(), app)).unwrap();
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

    /// Create prompt must mirror `dialog::render`'s ADD/EDIT TASK title
    /// treatment (` NEW NOTE ` here) and drop the old ".md added
    /// automatically" explanatory line — the prompt should read as a plain
    /// name field. Behavior (the actual `.md` auto-append) is unchanged and
    /// untested here; this only checks the rendered text.
    #[test]
    fn create_prompt_uses_new_note_title_with_no_md_explanation() {
        let mut app = build_app();
        app.notes_popup
            .begin_prompt(NotePromptKind::Create, String::new());
        app.notes_popup.prompt_push('r');
        app.notes_popup.prompt_push('e');
        app.notes_popup.prompt_push('a');
        app.notes_popup.prompt_push('d');
        app.notes_popup.prompt_push('m');
        app.notes_popup.prompt_push('e');
        let text = render_popup(&app);
        assert!(
            text.contains("NEW NOTE"),
            "create prompt should title itself NEW NOTE like ADD TASK: {text}"
        );
        assert!(
            !text.to_lowercase().contains(".md"),
            "create prompt should no longer explain .md auto-append: {text}"
        );
        assert!(
            text.contains("› readme") || text.contains("›readme"),
            "create prompt should show the ADD TASK-style › input marker: {text}"
        );
    }

    /// Rename prompt mirrors EDIT TASK's title-switch pattern.
    #[test]
    fn rename_prompt_uses_rename_note_title() {
        let mut app = build_app();
        app.notes_popup
            .begin_prompt(NotePromptKind::Rename { index: 0 }, "old".to_string());
        let text = render_popup(&app);
        assert!(
            text.contains("RENAME NOTE"),
            "rename prompt should title itself RENAME NOTE like EDIT TASK: {text}"
        );
        assert!(
            !text.to_lowercase().contains(".md"),
            "rename prompt should no longer explain .md auto-append: {text}"
        );
    }

    // ---- Round 3 feedback: list rows adopt task_row.rs's glyph convention --

    /// The cursor row must carry the same leading glyph `task_row.rs::build_line`
    /// uses for its own cursor row (`"▸ "`), not just a background swap — the
    /// user's exact complaint was that a plain background swap alone reads as
    /// "muy soso" (bland). Non-cursor rows keep the matching blank two-char
    /// glyph column (`"  "`) so filenames stay aligned.
    #[test]
    fn cursor_row_shows_the_same_glyph_task_row_uses() {
        let mut app = build_app();
        app.notes_popup = NotesPopupState::new(vec![
            std::path::PathBuf::from("/tmp/a.md"),
            std::path::PathBuf::from("/tmp/b.md"),
        ]);
        app.notes_popup.cursor = 1;

        let text = render_popup(&app);

        let cursor_line = text
            .lines()
            .find(|l| l.contains("b.md"))
            .expect("cursor row (b.md) rendered");
        assert!(
            cursor_line.contains('▸'),
            "cursor row must show task_row.rs's cursor glyph: {cursor_line:?}"
        );
        let other_line = text
            .lines()
            .find(|l| l.contains("a.md"))
            .expect("non-cursor row (a.md) rendered");
        assert!(
            !other_line.contains('▸'),
            "non-cursor row must not show the cursor glyph: {other_line:?}"
        );
    }

    /// The cursor row's emphasis must read via BOTH the glyph AND the
    /// background swap — not just one — per the explicit requirement that
    /// selection be unambiguous.
    #[test]
    fn cursor_row_has_both_glyph_color_and_background_emphasis() {
        let mut app = build_app();
        app.notes_popup = NotesPopupState::new(vec![std::path::PathBuf::from("/tmp/only.md")]);
        let theme = app.theme();

        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| super::render(f, f.area(), &app)).unwrap();
        let buf = terminal.backend().buffer();

        // Row 1 (inside the border) is the single file row, cursor at 0.
        let row_has_cursor_bg = (0..buf.area.width).any(|x| buf[(x, 1)].bg == theme.cursor);
        assert!(
            row_has_cursor_bg,
            "selected row must still carry the background swap"
        );
        let glyph_cell = (0..buf.area.width).find(|&x| buf[(x, 1)].symbol() == "▸");
        assert!(
            glyph_cell.is_some(),
            "selected row must also carry the glyph"
        );
        let glyph_x = glyph_cell.unwrap();
        assert_eq!(
            buf[(glyph_x, 1)].fg,
            theme.accent,
            "glyph must be colored via theme.accent, matching task_row.rs"
        );
    }
}
