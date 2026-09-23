use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, DialogInputMode, Mode, NoteEditorMode, View};
use crate::ui::dialog::draft_cursor_spans;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme();
    let mut mode_label: std::borrow::Cow<'static, str> = match app.mode {
        Mode::Normal => "NORMAL".into(),
        Mode::Insert => match app.draft.input_mode() {
            DialogInputMode::Normal => "NORMAL",
            DialogInputMode::Insert => "INSERT",
        }
        .into(),
        Mode::Search => "SEARCH".into(),
        Mode::Visual => "VISUAL".into(),
        Mode::Help => "HELP".into(),
        Mode::Settings => "SETTINGS".into(),
        Mode::PromptProject => "PROJECT".into(),
        Mode::PromptContext => "CONTEXT".into(),
        Mode::PickProject => "PICK +PROJECT".into(),
        Mode::PickContext => "PICK @CONTEXT".into(),
        Mode::PromptRenameProject => "RENAME +PROJECT".into(),
        Mode::PromptRenameContext => "RENAME @CONTEXT".into(),
        Mode::PickSavedFilter => "PICK FILTER".into(),
        Mode::PromptSaveFilter => "SAVE FILTER".into(),
        Mode::CommandPalette => "COMMAND".into(),
        Mode::Share => "SHARE".into(),
        Mode::PickTheme => "PICK THEME".into(),
        Mode::Welcome => "WELCOME".into(),
        Mode::Notes => match app.notes_popup.active_editor.as_ref().map(|e| e.mode()) {
            Some(NoteEditorMode::Normal) => "NORMAL".into(),
            Some(NoteEditorMode::Insert) => "INSERT".into(),
            None => "NOTES".into(),
        },
    };
    if matches!(app.view, View::Archive) {
        mode_label = "ARCHIVE".into();
    }
    if let Some(f) = app.flash_active() {
        mode_label = format!("{mode_label} · {f}").into();
    }

    // T11: while the pinned note has keyboard focus, `app.mode` stays
    // `Mode::Normal` (that's the point — see `src/app/pinned_note.rs`), so
    // it can't drive the hint on its own; check `pinned_focus` ahead of the
    // per-mode match instead of trying to fold it into that match's arms.
    let mut hint: std::borrow::Cow<'static, str> = if app.pinned_focus {
        match app.active_pinned_note().map(|e| e.mode()) {
            Some(NoteEditorMode::Normal) if app.pinned_notes.len() > 1 => {
                "h/j/k/l or arrows move · i insert · Ctrl+S save · Tab/S-Tab switch tab · z unfocus · Z close tab"
                    .into()
            }
            Some(NoteEditorMode::Normal) => {
                "h/j/k/l or arrows move · i insert · Ctrl+S save · z unfocus · Z close pinned"
                    .into()
            }
            Some(NoteEditorMode::Insert) => {
                "type to edit · Enter newline · Ctrl+S save · Esc normal".into()
            }
            None => "z unfocus · Z close pinned".into(),
        }
    } else {
        match app.mode {
            Mode::Insert => match app.draft.input_mode() {
                DialogInputMode::Normal => {
                    "h/l navigate · w/b/e word · i/a insert · Enter save · Esc cancel"
                }
                DialogInputMode::Insert => "Enter save · Esc normal",
            },
            Mode::Visual => "space toggle · x complete · dd delete · Esc cancel",
            Mode::Help => "? close help",
            Mode::Settings => "Esc back",
            Mode::PromptProject => "type +project name · Enter save · Esc cancel",
            Mode::PromptContext => "type @context name · Enter toggle · Esc cancel",
            Mode::PickProject => "j/k or ↑↓ cycle projects · r rename · Enter keep · Esc clear",
            Mode::PickContext => "j/k or ↑↓ cycle contexts · r rename · Enter keep · Esc clear",
            Mode::PickSavedFilter => "j/k or ↑↓ cycle filters · Enter keep · Esc revert",
            Mode::PromptSaveFilter => "type a filter name · Enter save · Esc cancel",
            Mode::CommandPalette => "type to filter · Enter run · Esc cancel",
            Mode::Share => "scan the QR · any key dismisses",
            Mode::Welcome => "c create ./todo.txt · s open sample · q quit",
            Mode::Notes => match app.notes_popup.active_editor.as_ref().map(|e| e.mode()) {
                Some(NoteEditorMode::Normal) => {
                    "h/j/k/l or arrows move · i insert · Ctrl+S save · Esc back to list"
                }
                Some(NoteEditorMode::Insert) => {
                    "type to edit · Enter newline · Ctrl+S save · Esc normal"
                }
                None => {
                    "j/k navigate · e/i edit · n new · r rename · d delete · u unlink · Esc close"
                }
            },
            _ => {
                "j/k · n new · r reschedule · x done · o notes · z pin · / search · ? help · u undo · q quit"
            }
        }
        .into()
    };
    // A note pinned-but-unfocused still needs `Z` surfaced somewhere — it's
    // not covered by any per-mode arm above since the user could be in
    // almost any mode while it sits docked in the background.
    if !app.pinned_focus && !app.pinned_notes.is_empty() {
        hint = format!("{hint} · Z close pinned").into();
    }

    let mut right_parts = Vec::new();
    if matches!(app.view, View::Archive) {
        right_parts.push(format!("{} archived", app.archive().len()));
    } else {
        right_parts.push(format!("{} open", app.visible_indices().len()));
    }
    if !app.selection.is_empty() {
        right_parts.push(format!("{} selected", app.selection.len()));
    }
    right_parts.push(app.today().to_string());
    right_parts.push(concat!(env!("CARGO_PKG_NAME"), " ", env!("CARGO_PKG_VERSION")).to_string());
    // Track where the update suffix would slot in so we can paint it in the
    // accent color (the rest of the right text is dim).
    let update_suffix = app
        .update_available()
        .map(|tag| format!(" · ↑ {tag} (tuxedo update)"));
    let right_text = right_parts.join(" · ");

    // Append a chord indicator (e.g. " g…") so two-key sequences like gg/dd/fp
    // give visible feedback on the first press. Only shown while armed.
    let chord_suffix = app
        .chord
        .active()
        .map(|c| format!(" {c}…"))
        .unwrap_or_default();
    // Layout: mode chip on left, hint in middle, right text right-aligned.
    let chip_text = format!(" {mode_label}{chord_suffix} ");
    let chip_w = chip_text.chars().count() as u16;
    let update_w = update_suffix
        .as_deref()
        .map(|s| s.chars().count() as u16)
        .unwrap_or(0);
    let right_w = right_text.chars().count() as u16 + update_w + 1;
    let middle_w = area.width.saturating_sub(chip_w).saturating_sub(right_w);

    let [chip_area, mid_area, right_area] = Layout::horizontal([
        Constraint::Length(chip_w),
        Constraint::Length(middle_w),
        Constraint::Length(right_w),
    ])
    .areas(area);

    let chip = Paragraph::new(Span::styled(
        chip_text,
        Style::default()
            .bg(theme.mode_bg)
            .fg(theme.mode_fg)
            .add_modifier(Modifier::BOLD),
    ))
    .style(Style::default().bg(theme.statusbar));
    frame.render_widget(chip, chip_area);

    let mid_line = Line::from(vec![
        Span::raw("  "),
        Span::styled(hint, Style::default().fg(theme.status_fg)),
    ])
    .style(Style::default().bg(theme.statusbar));
    frame.render_widget(
        Paragraph::new(mid_line).style(Style::default().bg(theme.statusbar)),
        mid_area,
    );

    let right_line = if let Some(suffix) = update_suffix {
        Line::from(vec![
            Span::styled(right_text, Style::default().fg(theme.dim)),
            Span::styled(
                suffix,
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ", Style::default().fg(theme.dim)),
        ])
        .style(Style::default().bg(theme.statusbar))
    } else {
        Line::from(Span::styled(
            format!("{right_text} "),
            Style::default().fg(theme.dim),
        ))
        .style(Style::default().bg(theme.statusbar))
    };
    frame.render_widget(
        Paragraph::new(right_line)
            .style(Style::default().bg(theme.statusbar))
            .right_aligned(),
        right_area,
    );
}

pub fn render_command_line(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme();
    let visible_count = app.visible_indices().len();
    let suggestion = format!("  {visible_count} matches · Enter accept · Esc cancel");
    let mut spans = vec![
        Span::raw(" "),
        Span::styled(
            "/",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ];
    spans.extend(draft_cursor_spans(
        app.draft.text(),
        app.draft.cursor(),
        theme.fg,
        theme.bg,
    ));
    spans.push(Span::styled(suggestion, Style::default().fg(theme.dim)));
    let line = Line::from(spans).style(Style::default().bg(theme.bg));
    frame.render_widget(
        Paragraph::new(line).style(Style::default().bg(theme.bg)),
        area,
    );
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::app::App;
    use crate::config::Config;

    fn build_app() -> App {
        let path =
            std::env::temp_dir().join(format!("tuxedo-status-test-{}.txt", std::process::id()));
        let body = "(A) Buy milk\n".to_string();
        std::fs::write(&path, &body).unwrap();
        App::new(path, body, "2026-05-06".to_string(), Config::default())
    }

    /// The global Normal/List hint bar (`status::render`'s catch-all arm) is
    /// a hardcoded string with no other test coverage — confirm it actually
    /// advertises the `o` notes action rather than only trusting a code
    /// review of the literal.
    #[test]
    fn normal_mode_hint_advertises_notes_action() {
        let app = build_app();
        // Wide enough that the middle hint segment isn't clipped by the
        // chip/right-text layout math before the assertion below gets to see
        // "o notes" — the real status bar truncates on narrow terminals too,
        // that's expected and not what this test is checking.
        let backend = TestBackend::new(200, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| super::render(f, f.area(), &app)).unwrap();
        let buf = terminal.backend().buffer();
        let mut text = String::new();
        for x in 0..buf.area.width {
            text.push_str(buf[(x, 0)].symbol());
        }
        assert!(
            text.contains("o notes"),
            "Normal-mode hint bar should advertise the notes action ('o notes'): {text}"
        );
    }
}
