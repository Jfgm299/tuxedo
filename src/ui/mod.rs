use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::widgets::{Block, Clear};

use crate::app::{App, Mode, View};

pub mod archive;
pub mod command_palette;
pub mod detail;
pub mod dialog;
pub mod empty;
pub mod filters;
pub mod header;
pub mod help;
pub mod hyperlinks;
pub mod list;
pub mod logo;
pub mod note_editor;
pub mod notes_popup;
pub mod settings;
pub mod share;
pub mod status;
pub mod task_row;
pub mod theme_picker;
pub mod title;
pub mod welcome;

// Pane and overlay sizing. Promoted out of inline literals so the three
// `MIN_BODY_W` references below stay in sync, and so tweaking a sidebar
// width is a one-line change.
const LEFT_PANE_W: u16 = 26;
const RIGHT_PANE_W: u16 = 34;
const MIN_BODY_W: u16 = 40;

const DIALOG_H: u16 = 8;
const DIALOG_MIN_W: u16 = 40;
const DIALOG_MAX_W: u16 = 100;

const HELP_MAX_H: u16 = 29;
const HELP_MIN_W: u16 = 76;
const HELP_MAX_W: u16 = 120;

const PROMPT_H: u16 = 5;
const PROMPT_MAX_W: u16 = 50;

const PALETTE_MAX_H: u16 = 20;
const PALETTE_MIN_W: u16 = 50;
const PALETTE_MAX_W: u16 = 80;

pub fn draw(frame: &mut Frame, app: &App) {
    let theme = app.theme();
    let area = frame.area();

    // Paint full background.
    frame.render_widget(Block::default().style(Style::default().bg(theme.bg)), area);

    let bottom = if app.prefs.layout.status_bar { 1 } else { 0 };
    let [body_area, bottom_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(bottom)]).areas(area);

    // Determine pane widths. Sidebars apply to every view; navigation +
    // detail pane track the cursor regardless of which view is active.
    let show_left = app.prefs.layout.left;
    // A pinned note (T11, extended to multiple tabs by T12) forces the right
    // column visible even when the user's `show_right` preference is off —
    // pinning doesn't touch that preference at all, it just visually
    // overrides it while active. The column reverts to plain `show_right`
    // automatically once the last tab is closed, since this `||` is
    // recomputed fresh every frame from `pinned_notes.is_empty()`.
    let pinned = !app.pinned_notes.is_empty();
    let show_right = app.prefs.layout.right || pinned;
    let left_w = if show_left { LEFT_PANE_W } else { 0 };
    let right_w = right_pane_width(show_right, pinned, body_area.width, left_w);

    let constraints = match (show_left, show_right) {
        (true, true) => vec![
            Constraint::Length(left_w),
            Constraint::Min(MIN_BODY_W),
            Constraint::Length(right_w),
        ],
        (true, false) => vec![Constraint::Length(left_w), Constraint::Min(MIN_BODY_W)],
        (false, true) => vec![Constraint::Min(MIN_BODY_W), Constraint::Length(right_w)],
        (false, false) => vec![Constraint::Min(1)],
    };
    let chunks = Layout::horizontal(constraints).split(body_area);

    let (left_area, center_area, right_area) = match (show_left, show_right) {
        (true, true) => (Some(chunks[0]), chunks[1], Some(chunks[2])),
        (true, false) => (Some(chunks[0]), chunks[1], None),
        (false, true) => (None, chunks[0], Some(chunks[1])),
        (false, false) => (None, chunks[0], None),
    };

    if let Some(la) = left_area {
        filters::render(frame, la, app);
    }
    match app.view() {
        View::List => list::render(frame, center_area, app),
        View::Archive => archive::render(frame, center_area, app),
    }
    if let Some(ra) = right_area {
        if app.pinned_notes.is_empty() {
            detail::render(frame, ra, app);
        } else {
            note_editor::render_pinned(frame, ra, theme, app);
        }
    }

    if app.prefs.layout.status_bar {
        if app.mode == Mode::Search {
            status::render_command_line(frame, bottom_area, app);
        } else {
            status::render(frame, bottom_area, app);
        }
    }

    // Overlays
    match app.mode {
        Mode::Insert => {
            let dlg_w: u16 = (u32::from(center_area.width) * 4 / 5)
                .clamp(u32::from(DIALOG_MIN_W), u32::from(DIALOG_MAX_W))
                as u16;
            let dlg = centered_in(area, dlg_w, DIALOG_H);
            frame.render_widget(Clear, dlg);
            dialog::render(frame, dlg, app);
            // At most one overlay shows at a time. The autocomplete popup is
            // suppressed while a metadata picker is open so we don't stack
            // two floating panels in the same spot.
            if !dialog::render_overlay(frame, dlg, area, app) {
                dialog::render_autocomplete(frame, dlg, area, app);
            }
        }
        Mode::Help => {
            let h: u16 = area.height.saturating_sub(3).min(HELP_MAX_H);
            let w: u16 = (u32::from(area.width) * 9 / 10)
                .clamp(u32::from(HELP_MIN_W), u32::from(HELP_MAX_W))
                as u16;
            let r = centered_in(area, w, h);
            frame.render_widget(Clear, r);
            help::render(frame, r, app);
        }
        Mode::Settings => {
            frame.render_widget(Clear, body_area);
            settings::render(frame, body_area, app);
        }
        Mode::PromptProject
        | Mode::PromptContext
        | Mode::PromptSaveFilter
        | Mode::PromptRenameProject
        | Mode::PromptRenameContext => {
            let w: u16 = PROMPT_MAX_W.min(area.width.saturating_sub(4));
            let r = centered_in(area, w, PROMPT_H);
            frame.render_widget(Clear, r);
            dialog::render_prompt(frame, r, app);
            if matches!(app.mode, Mode::PromptProject | Mode::PromptContext) {
                dialog::render_autocomplete(frame, r, area, app);
            }
        }
        Mode::CommandPalette => {
            let h: u16 = area.height.saturating_sub(4).min(PALETTE_MAX_H);
            let w: u16 = (u32::from(area.width) * 3 / 5)
                .clamp(u32::from(PALETTE_MIN_W), u32::from(PALETTE_MAX_W))
                as u16;
            let r = centered_in(area, w, h);
            frame.render_widget(Clear, r);
            command_palette::render(frame, r, app);
        }
        Mode::Share => {
            let (w, h) = share::size_for(app);
            let r = centered_in(area, w, h);
            frame.render_widget(Clear, r);
            share::render(frame, r, app);
        }
        Mode::Notes => {
            // Styled like the ADD TASK dialog (same width formula, sized a
            // bit taller to fit a scrollable list).
            let dlg_w: u16 = (u32::from(center_area.width) * 4 / 5)
                .clamp(u32::from(DIALOG_MIN_W), u32::from(DIALOG_MAX_W))
                as u16;
            let dlg_h: u16 = (DIALOG_H + 4).min(area.height.saturating_sub(2));
            let r = centered_in(area, dlg_w, dlg_h);
            frame.render_widget(Clear, r);
            if app.notes_popup.active_editor.is_some() {
                note_editor::render(frame, r, app);
            } else {
                notes_popup::render(frame, r, app);
            }
        }
        Mode::PickTheme => {
            let h: u16 = area.height.saturating_sub(4).min(PALETTE_MAX_H);
            let w: u16 = (u32::from(area.width) * 3 / 5)
                .clamp(u32::from(PALETTE_MIN_W), u32::from(PALETTE_MAX_W))
                as u16;
            let r = centered_in(area, w, h);
            frame.render_widget(Clear, r);
            theme_picker::render(frame, r, app);
        }
        Mode::Welcome => {
            let r = centered_in(area, welcome::WIDTH, welcome::HEIGHT);
            frame.render_widget(Clear, r);
            welcome::render(frame, r, app);
        }
        _ => {}
    }
    // OSC 8 hyperlinks are applied post-draw by the caller (see
    // `hyperlinks::collect` + `emit_overlay`). Doing it inside the buffer
    // breaks ratatui's diff width calculation — keep cell symbols pristine.
}

/// The right column's width for `draw()`'s `Layout::horizontal` split.
/// `show_right` is the *effective* visibility (`prefs.layout.right ||
/// pinned`, computed by the caller) — this function only decides *how wide*,
/// given it's already visible. A pinned note (T11) gets roughly half the
/// body width instead of the fixed `RIGHT_PANE_W`, clamped so the center
/// list never gets pushed below its own `MIN_BODY_W` floor. All saturating
/// arithmetic: a very narrow terminal degrades to a 0-width (invisible)
/// pinned column instead of panicking on a u16 underflow.
pub(crate) fn right_pane_width(
    show_right: bool,
    pinned: bool,
    body_width: u16,
    left_w: u16,
) -> u16 {
    if !show_right {
        return 0;
    }
    if !pinned {
        return RIGHT_PANE_W;
    }
    let ceiling = body_width.saturating_sub(left_w).saturating_sub(MIN_BODY_W);
    (body_width / 2).min(ceiling)
}

pub(crate) fn centered_in(parent: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(parent.width);
    let h = h.min(parent.height);
    let x = parent.x + (parent.width - w) / 2;
    let y = parent.y + (parent.height - h) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

pub(crate) fn fill_bg(frame: &mut Frame, area: Rect, style: Style) {
    frame.render_widget(Block::default().style(style), area);
}

pub(crate) fn density_blank_lines(d: crate::app::Density) -> usize {
    match d {
        crate::app::Density::Compact => 0,
        crate::app::Density::Comfortable => 1,
        crate::app::Density::Cozy => 2,
    }
}

/// Compute the new vertical scroll offset for a paragraph-backed list so the
/// cursor row stays inside the viewport. `prev` is the previous frame's offset,
/// `cursor_line` is the line index of the cursor (or `None` if there's no
/// cursor row in the current build, e.g. when the list is empty). `height` is
/// the viewport height in rows; `total` is the total line count.
pub(crate) fn keep_cursor_visible(
    prev: u16,
    cursor_line: Option<usize>,
    height: u16,
    total: usize,
) -> u16 {
    let h = usize::from(height);
    if h == 0 || total == 0 {
        return 0;
    }
    let max_offset = total.saturating_sub(h);
    let prev = usize::from(prev).min(max_offset);
    let new = match cursor_line {
        Some(cl) if cl < prev => cl,
        Some(cl) if cl >= prev + h => cl + 1 - h,
        _ => prev,
    };
    new.min(max_offset).min(usize::from(u16::MAX)) as u16
}

#[cfg(test)]
mod tests {
    use super::{keep_cursor_visible, right_pane_width};

    // ---- T11: right-pane width for a pinned note --------------------------

    #[test]
    fn right_pane_width_is_zero_when_not_shown_regardless_of_pinned() {
        assert_eq!(right_pane_width(false, false, 200, 0), 0);
        assert_eq!(right_pane_width(false, true, 200, 0), 0);
    }

    #[test]
    fn right_pane_width_is_fixed_when_shown_and_not_pinned() {
        assert_eq!(right_pane_width(true, false, 200, 0), super::RIGHT_PANE_W);
        assert_eq!(right_pane_width(true, false, 200, 26), super::RIGHT_PANE_W);
    }

    #[test]
    fn right_pane_width_is_roughly_half_body_when_pinned() {
        assert_eq!(right_pane_width(true, true, 100, 0), 50);
        assert_eq!(right_pane_width(true, true, 200, 0), 100);
    }

    #[test]
    fn right_pane_width_when_pinned_never_pushes_center_below_min_body_w() {
        // body 100, left 26: half of 100 is 50, but 100 - 26 - MIN_BODY_W(40)
        // = 34 is the ceiling, so the clamp (not the naive half) must win.
        let w = right_pane_width(true, true, 100, 26);
        assert_eq!(w, 34);
        assert!(
            26 + super::MIN_BODY_W + w <= 100,
            "left + center floor + right must fit: {w}"
        );
    }

    #[test]
    fn right_pane_width_when_pinned_degrades_to_zero_on_a_very_narrow_terminal_without_panicking() {
        // body narrower than left_w + MIN_BODY_W: saturating arithmetic must
        // clamp to 0, never underflow/panic.
        let w = right_pane_width(true, true, 30, 26);
        assert_eq!(w, 0);
    }

    // ---- T11: pinned note actually renders in the right column -----------

    #[test]
    fn pinned_note_renders_in_right_column_even_when_show_right_pref_is_off_and_reverts_after_unpin()
     {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        use crate::app::{App, NoteEditorMode, NoteEditorState};
        use crate::config::Config;

        let dir = std::env::temp_dir().join(format!(
            "tuxedo-ui-pin-render-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("create dir");
        let todo_path = dir.join("todo.txt");
        let body = "(A) Buy milk\n".to_string();
        std::fs::write(&todo_path, &body).expect("write todo.txt");
        let note_path = dir.join("pinned-note.md");
        std::fs::write(&note_path, "pinned note body").expect("write note");

        let mut app = App::new(todo_path, body, "2026-05-06".to_string(), Config::default());
        app.prefs.toggle_right(); // show_right preference off
        assert!(!app.prefs.layout.right, "precondition: pref is off");

        let render_text = |app: &App| -> String {
            let backend = TestBackend::new(120, 20);
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal.draw(|f| super::draw(f, app)).expect("draw");
            let buf = terminal.backend().buffer();
            let mut text = String::new();
            for y in 0..buf.area.height {
                for x in 0..buf.area.width {
                    text.push_str(buf[(x, y)].symbol());
                }
            }
            text
        };

        let before = render_text(&app);
        assert!(
            !before.contains("pinned-note.md"),
            "nothing pinned yet, and show_right is off: file name must not appear"
        );

        app.pinned_notes.push(NoteEditorState::load(
            note_path.clone(),
            NoteEditorMode::Normal,
        ));
        let while_pinned = render_text(&app);
        assert!(
            while_pinned.contains("pinned-note.md"),
            "right column must render the pinned note even though show_right is off: {while_pinned}"
        );

        app.close_pinned_note();
        let after = render_text(&app);
        assert!(
            !after.contains("pinned-note.md"),
            "right column must revert to show_right's own (off) state after unpinning: {after}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_scroll_when_content_fits() {
        assert_eq!(keep_cursor_visible(0, Some(5), 10, 8), 0);
        assert_eq!(keep_cursor_visible(0, Some(7), 10, 8), 0);
    }

    #[test]
    fn scrolls_down_when_cursor_below_viewport() {
        // viewport rows 0..5, cursor at line 7 -> offset = 7 - 5 + 1 = 3
        assert_eq!(keep_cursor_visible(0, Some(7), 5, 20), 3);
    }

    #[test]
    fn scrolls_up_when_cursor_above_viewport() {
        // prev offset 10, cursor at line 3 -> offset = 3
        assert_eq!(keep_cursor_visible(10, Some(3), 5, 20), 3);
    }

    #[test]
    fn keeps_previous_offset_when_cursor_in_viewport() {
        // prev 5, cursor at line 7, height 5 -> 7 in [5, 10), stays 5
        assert_eq!(keep_cursor_visible(5, Some(7), 5, 20), 5);
    }

    #[test]
    fn clamps_to_max_offset_when_previous_exceeds_it() {
        // total shrank since last frame; previous offset 50 is now too large.
        assert_eq!(keep_cursor_visible(50, None, 5, 8), 3);
    }

    #[test]
    fn handles_degenerate_inputs() {
        assert_eq!(keep_cursor_visible(0, None, 0, 100), 0);
        assert_eq!(keep_cursor_visible(0, Some(0), 5, 0), 0);
    }
}
