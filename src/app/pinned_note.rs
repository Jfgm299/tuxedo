//! T11: pin a note to the right-docked panel (tmux-pane-style toggle-focus),
//! reusing the existing `show_right`/`RIGHT_PANE_W` docking column in
//! `src/ui/mod.rs::draw()` rather than a new floating overlay (see
//! `odd/tasks/notes-popup.md`'s Round 2 exploration note). `App::pinned_note`
//! and `App::pinned_focus` are top-level fields (not nested inside
//! `NotesPopupState`/`Mode::Notes`) because they must survive `Mode` changes
//! and keep rendering regardless of the current mode.
//!
//! `z` (`Action::TogglePinFocus`) and `Z` (`Action::ClosePinnedNote`) are
//! real global `Action`s (see `src/action.rs`), not popup-internal bindings,
//! specifically so they can fire from bare `Mode::Normal` (to enter/toggle
//! focus) and — via the early routing branch in `main.rs::handle_key` — even
//! while `pinned_focus` is `true` and `app.mode` alone can't distinguish
//! "focus on the pinned note" from "focus on the main list".

use super::App;
use super::types::Mode;

impl App {
    /// `z`: tmux-pane-style toggle-focus.
    ///
    /// - Nothing pinned, and a note is open in the floating editor
    ///   (`notes_popup.active_editor.is_some()`, which only happens while
    ///   `Mode::Notes` is active): pin it — move the `NoteEditorState` out of
    ///   the popup into `pinned_note`, close the popup (`mode` reverts to
    ///   `Mode::Normal`), and give the pinned note focus.
    /// - Nothing pinned, no active floating editor: no-op (nothing to pin).
    /// - Already pinned: toggle `pinned_focus` — focus moves into the pinned
    ///   note if the main app currently has it, or back to the main app if
    ///   the pinned note currently has it. The pin itself is untouched
    ///   either way.
    pub fn toggle_pin_focus(&mut self) {
        if self.pinned_note.is_none() {
            if let Some(editor) = self.notes_popup.active_editor.take() {
                self.pinned_note = Some(editor);
                self.pinned_focus = true;
                self.mode = Mode::Normal;
            }
            return;
        }
        self.pinned_focus = !self.pinned_focus;
    }

    /// `Z`: close the pinned note entirely, from anywhere — whether it
    /// currently has focus or not, whether `app.mode` is `Mode::Normal` or
    /// something else. Removes the pin (not just unfocuses it); unsaved
    /// edits are discarded, matching this MVP's existing lack of a
    /// discard-confirmation dialog elsewhere (see `odd/tasks/notes-popup.md`
    /// T11). No-op when nothing is pinned.
    pub fn close_pinned_note(&mut self) {
        self.pinned_note = None;
        self.pinned_focus = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::{build_app_with_config, test_path};
    use crate::app::{Mode, NoteEditorMode};
    use crate::config::Config;

    fn app_with_open_editor(dir: &std::path::Path) -> App {
        let notes_folder = dir.join("tasks").join("abc123");
        std::fs::create_dir_all(&notes_folder).expect("create notes folder");
        std::fs::write(notes_folder.join("a.md"), "content a").expect("write a.md");
        let cfg = Config {
            notes_dir: Some(dir.to_string_lossy().into_owned()),
            ..Config::default()
        };
        let raw = "Write PR summary +work notes:abc123/\n";
        let mut app = build_app_with_config(raw, cfg);
        app.open_notes_for_current();
        app.open_note_editor_normal();
        app
    }

    #[test]
    fn toggle_pin_focus_pins_the_active_floating_editor() {
        let dir = test_path().with_extension("notes");
        let mut app = app_with_open_editor(&dir);

        app.toggle_pin_focus();

        assert!(
            app.notes_popup.active_editor.is_none(),
            "moved out of the popup"
        );
        let pinned = app.pinned_note.as_ref().expect("note pinned");
        assert_eq!(pinned.lines(), &["content a"]);
        assert_eq!(app.mode, Mode::Normal, "popup closes back to Mode::Normal");
        assert!(app.pinned_focus, "newly pinned note gets focus");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn toggle_pin_focus_is_noop_with_nothing_pinned_and_no_active_editor() {
        let dir = test_path().with_extension("notes");
        let cfg = Config {
            notes_dir: Some(dir.to_string_lossy().into_owned()),
            ..Config::default()
        };
        let mut app = build_app_with_config("Write PR summary +work\n", cfg);

        app.toggle_pin_focus();

        assert!(app.pinned_note.is_none());
        assert!(!app.pinned_focus);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn toggle_pin_focus_while_pinned_and_unfocused_focuses_it() {
        let dir = test_path().with_extension("notes");
        let mut app = app_with_open_editor(&dir);
        app.toggle_pin_focus(); // pin (also focuses)
        app.pinned_focus = false; // simulate having unfocused it already

        app.toggle_pin_focus();

        assert!(app.pinned_focus);
        assert!(app.pinned_note.is_some(), "still pinned");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn toggle_pin_focus_while_pinned_and_focused_unfocuses_it() {
        let dir = test_path().with_extension("notes");
        let mut app = app_with_open_editor(&dir);
        app.toggle_pin_focus(); // pin (also focuses) -> pinned_focus == true

        app.toggle_pin_focus();

        assert!(!app.pinned_focus, "focus moves back to the main app");
        assert!(
            app.pinned_note.is_some(),
            "note stays pinned, just unfocused"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn close_pinned_note_removes_pin_whether_focused_or_not() {
        let dir = test_path().with_extension("notes");
        let mut app = app_with_open_editor(&dir);
        app.toggle_pin_focus(); // pinned, focused

        app.close_pinned_note();

        assert!(app.pinned_note.is_none());
        assert!(!app.pinned_focus);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn close_pinned_note_removes_pin_when_unfocused() {
        let dir = test_path().with_extension("notes");
        let mut app = app_with_open_editor(&dir);
        app.toggle_pin_focus(); // pinned, focused
        app.toggle_pin_focus(); // unfocused, still pinned

        app.close_pinned_note();

        assert!(app.pinned_note.is_none());
        assert!(!app.pinned_focus);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn close_pinned_note_is_noop_with_nothing_pinned() {
        let dir = test_path().with_extension("notes");
        let cfg = Config {
            notes_dir: Some(dir.to_string_lossy().into_owned()),
            ..Config::default()
        };
        let mut app = build_app_with_config("Write PR summary +work\n", cfg);

        app.close_pinned_note();

        assert!(app.pinned_note.is_none());
        assert!(!app.pinned_focus);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pinned_note_field_is_directly_mutable_for_key_routing() {
        let dir = test_path().with_extension("notes");
        let mut app = app_with_open_editor(&dir);
        app.toggle_pin_focus();

        let editor = app.pinned_note.as_mut().expect("pinned note accessible");
        editor.enter_insert();

        assert_eq!(
            app.pinned_note.as_ref().expect("still pinned").mode(),
            NoteEditorMode::Insert
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
