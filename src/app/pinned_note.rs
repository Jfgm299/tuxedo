//! T11+T12: pin notes to the right-docked panel (tmux-pane-style
//! toggle-focus), reusing the existing `show_right`/`RIGHT_PANE_W` docking
//! column in `src/ui/mod.rs::draw()` rather than a new floating overlay (see
//! `odd/tasks/notes-popup.md`'s Round 2 exploration note). `App::pinned_notes`
//! and `App::pinned_focus` are top-level fields (not nested inside
//! `NotesPopupState`/`Mode::Notes`) because they must survive `Mode` changes
//! and keep rendering regardless of the current mode.
//!
//! T12 extends T11's single `Option<NoteEditorState>` into an ordered
//! collection (`Vec<NoteEditorState>` + `active_pin: usize`), following the
//! same "meaningless index on an empty collection" convention already used
//! by `NotesPopupState::cursor`/`selected_index` in `src/app/notes_popup.rs`
//! — `active_pin` is left at `0` (never read as meaningful) whenever
//! `pinned_notes` is empty, and every accessor goes through
//! `active_pinned_note`/`active_pinned_note_mut` (`Vec::get`/`get_mut`, never
//! direct indexing) so the empty case can never panic.
//!
//! `z` (`Action::TogglePinFocus`) and `Z` (`Action::ClosePinnedNote`) are
//! real global `Action`s (see `src/action.rs`), not popup-internal bindings,
//! specifically so they can fire from bare `Mode::Normal` (to enter/toggle
//! focus) and — via the early routing branch in `main.rs::handle_key` — even
//! while `pinned_focus` is `true` and `app.mode` alone can't distinguish
//! "focus on the pinned note" from "focus on the main list". Cycling
//! (`Tab`/`BackTab`) is deliberately NOT a global `Action`: it's only
//! meaningful once already focused on the pinned pane (there's no sensible
//! "cycle" from bare `Mode::Normal`), so it's checked the same
//! popup-internal-style way `main.rs` already checks `n`/`r`/`d`/`u` inside
//! `Mode::Notes` — see the early routing branch in `main.rs::handle_key`.

use super::App;
use super::note_editor::{NoteEditorMode, NoteEditorState};
use super::types::Mode;

impl App {
    /// The pinned tab currently active (rendered, and — while `pinned_focus`
    /// is set — receiving keystrokes), or `None` if nothing is pinned.
    /// Mirrors `NotesPopupState::selected`.
    pub fn active_pinned_note(&self) -> Option<&NoteEditorState> {
        self.pinned_notes.get(self.active_pin)
    }

    /// Mutable counterpart of [`App::active_pinned_note`], used by
    /// `main.rs::handle_pinned_note_key` to drive typing/motions/save on
    /// whichever tab is currently active.
    pub fn active_pinned_note_mut(&mut self) -> Option<&mut NoteEditorState> {
        self.pinned_notes.get_mut(self.active_pin)
    }

    /// `z`: tmux-pane-style toggle-focus, extended by T12 for multiple tabs.
    ///
    /// - A note is open in the floating editor
    ///   (`notes_popup.active_editor.is_some()`, which only happens while
    ///   `Mode::Notes` is active): **always** pins it as a NEW tab appended
    ///   to `pinned_notes` — regardless of whether other notes are already
    ///   pinned (T11 only allowed this when nothing was pinned yet; T12
    ///   changes that to "adds a tab" instead of being blocked/reinterpreted
    ///   as a focus-toggle) — makes it the active tab, closes the popup
    ///   (`mode` reverts to `Mode::Normal`), and gives it focus.
    /// - No active floating editor, and at least one note is already pinned:
    ///   toggle `pinned_focus` on the currently active tab (unchanged T11
    ///   behavior, now operating on `pinned_notes[active_pin]`).
    /// - No active floating editor, nothing pinned: no-op.
    pub fn toggle_pin_focus(&mut self) {
        if let Some(editor) = self.notes_popup.active_editor.take() {
            self.pinned_notes.push(editor);
            self.active_pin = self.pinned_notes.len() - 1;
            self.pinned_focus = true;
            self.mode = Mode::Normal;
            return;
        }
        if self.pinned_notes.is_empty() {
            return;
        }
        self.pinned_focus = !self.pinned_focus;
    }

    /// Round 3 feedback: `z` while just BROWSING the plain notes list (no
    /// floating editor open yet, `notes_popup.active_editor.is_none()`) --
    /// loads the currently selected file straight into a new pinned tab, as
    /// if `e` then `z` had been pressed, skipping the floating-editor step
    /// entirely. Resolves the selected file the exact same way
    /// `App::open_note_editor_normal` does (`notes_popup.selected()`), and
    /// no-ops on an empty list -- nothing selected to pin.
    ///
    /// This is deliberately its own method rather than
    /// `open_note_editor_normal()` followed by `toggle_pin_focus()`: on an
    /// empty list with something already pinned and unfocused, that naive
    /// composition would be a real bug -- `open_note_editor_normal()`
    /// correctly no-ops (nothing selected), but `toggle_pin_focus()`'s
    /// fallback branch has no way to know an open attempt "just failed"
    /// upstream, so it would still toggle focus onto the unrelated
    /// already-pinned note. Here, nothing happens at all unless a file was
    /// actually resolved -- only then do we push a new tab, activate it,
    /// focus it, and close the popup, mirroring exactly what
    /// `toggle_pin_focus()` does for the "pin from the floating editor"
    /// case, minus the intermediate `NotesPopupState::active_editor` layer.
    pub fn pin_selected_note_directly(&mut self) {
        let Some(path) = self.notes_popup.selected().cloned() else {
            return;
        };
        let editor = NoteEditorState::load(path, NoteEditorMode::Normal);
        self.pinned_notes.push(editor);
        self.active_pin = self.pinned_notes.len() - 1;
        self.pinned_focus = true;
        self.mode = Mode::Normal;
    }

    /// `Z`: close only the ACTIVE pinned tab, from anywhere — whether it
    /// currently has focus or not, whether `app.mode` is `Mode::Normal` or
    /// something else. Unsaved edits on the closed tab are discarded,
    /// matching this MVP's existing lack of a discard-confirmation dialog
    /// elsewhere (see `odd/tasks/notes-popup.md` T11). No-op when nothing is
    /// pinned.
    ///
    /// If other tabs remain, `active_pin` is left pointing at the same
    /// index: since `Vec::remove` shifts every later element down by one,
    /// this means the tab that was immediately after the closed one (if any)
    /// becomes active — a "move to the next tab" fallback — except when the
    /// closed tab was the last one, where the same unchanged index now
    /// refers to the new last tab (the previous one). `pinned_focus` is left
    /// untouched in both cases (still focused if it was, still unfocused if
    /// it wasn't). If that was the last tab, `pinned_notes` becomes empty
    /// and `pinned_focus` becomes `false` — the right column then reverts to
    /// `app.prefs.layout.right`'s own state, since `src/ui/mod.rs::draw()`'s
    /// `show_right` visibility is `app.prefs.layout.right ||
    /// !app.pinned_notes.is_empty()`.
    pub fn close_pinned_note(&mut self) {
        if self.pinned_notes.is_empty() {
            return;
        }
        self.pinned_notes.remove(self.active_pin);
        if self.pinned_notes.is_empty() {
            self.active_pin = 0;
            self.pinned_focus = false;
        } else if self.active_pin >= self.pinned_notes.len() {
            self.active_pin = self.pinned_notes.len() - 1;
        }
    }

    /// `Tab`/`BackTab` while focused on the pinned pane: cycle `active_pin`
    /// through `pinned_notes`, wrapping around at both ends. No-op (never
    /// panics) with fewer than two pinned notes — there's nothing to cycle
    /// to. Callers (see `main.rs::handle_key`'s early routing branch) only
    /// invoke this while `pinned_focus` is `true` and the active tab's own
    /// sub-mode is Normal, the same restriction already applied to `z`/`Z`.
    pub fn cycle_pinned_note(&mut self, forward: bool) {
        let len = self.pinned_notes.len();
        if len < 2 {
            return;
        }
        self.active_pin = if forward {
            (self.active_pin + 1) % len
        } else {
            (self.active_pin + len - 1) % len
        };
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

    /// Same folder as `app_with_open_editor` but with a second `.md` file
    /// (`b.md`), so T12 tests can pin a second note without re-deriving a
    /// fresh popup each time.
    fn app_with_open_editor_and_second_file(dir: &std::path::Path) -> App {
        let app = app_with_open_editor(dir);
        let notes_folder = dir.join("tasks").join("abc123");
        std::fs::write(notes_folder.join("b.md"), "content b").expect("write b.md");
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
        let pinned = app.active_pinned_note().expect("note pinned");
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

        assert!(app.pinned_notes.is_empty());
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
        assert_eq!(app.pinned_notes.len(), 1, "still pinned, no new tab added");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn toggle_pin_focus_while_pinned_and_focused_unfocuses_it() {
        let dir = test_path().with_extension("notes");
        let mut app = app_with_open_editor(&dir);
        app.toggle_pin_focus(); // pin (also focuses) -> pinned_focus == true

        app.toggle_pin_focus();

        assert!(!app.pinned_focus, "focus moves back to the main app");
        assert_eq!(
            app.pinned_notes.len(),
            1,
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

        assert!(app.pinned_notes.is_empty());
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

        assert!(app.pinned_notes.is_empty());
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

        assert!(app.pinned_notes.is_empty());
        assert!(!app.pinned_focus);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pinned_note_field_is_directly_mutable_for_key_routing() {
        let dir = test_path().with_extension("notes");
        let mut app = app_with_open_editor(&dir);
        app.toggle_pin_focus();

        let editor = app
            .active_pinned_note_mut()
            .expect("pinned note accessible");
        editor.enter_insert();

        assert_eq!(
            app.active_pinned_note().expect("still pinned").mode(),
            NoteEditorMode::Insert
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- T12: multiple pinned notes as tabs --------------------------------

    #[test]
    fn toggle_pin_focus_with_one_already_pinned_adds_a_second_tab_and_activates_it() {
        let dir = test_path().with_extension("notes");
        let mut app = app_with_open_editor_and_second_file(&dir);
        app.toggle_pin_focus(); // pins a.md as tab 0, focused
        assert_eq!(app.pinned_notes.len(), 1);
        app.pinned_focus = false; // simulate main app regaining focus

        // Re-open the popup and edit the second file, then pin it too.
        app.open_notes_for_current();
        app.notes_popup.move_down(); // select b.md
        app.open_note_editor_normal();
        app.toggle_pin_focus();

        assert_eq!(app.pinned_notes.len(), 2, "second tab added, not replaced");
        assert_eq!(app.active_pin, 1, "the new tab becomes active");
        assert!(app.pinned_focus, "the new tab gets focus");
        assert_eq!(
            app.pinned_notes[0].lines(),
            &["content a"],
            "original pinned note is still in the collection"
        );
        assert_eq!(app.pinned_notes[1].lines(), &["content b"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn toggle_pin_focus_from_mode_normal_toggles_focus_on_the_active_tab_only() {
        let dir = test_path().with_extension("notes");
        let mut app = app_with_open_editor_and_second_file(&dir);
        app.toggle_pin_focus(); // tab 0, focused
        app.open_notes_for_current();
        app.notes_popup.move_down();
        app.open_note_editor_normal();
        app.toggle_pin_focus(); // tab 1 added, active, focused
        assert_eq!(app.active_pin, 1);

        // Simulate reaching Mode::Normal with no active floating editor (the
        // popup is already closed by toggle_pin_focus above).
        assert!(app.notes_popup.active_editor.is_none());
        app.toggle_pin_focus();

        assert!(!app.pinned_focus, "toggled off");
        assert_eq!(app.active_pin, 1, "still the same active tab");
        assert_eq!(app.pinned_notes.len(), 2, "no tab added or removed");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cycle_pinned_note_wraps_forward_and_backward_with_three_tabs() {
        let dir = test_path().with_extension("notes");
        let mut app = app_with_open_editor(&dir);
        let note_a = dir.join("tasks").join("abc123").join("a.md");
        app.pinned_notes.push(NoteEditorState::load(
            note_a.clone(),
            NoteEditorMode::Normal,
        ));
        app.pinned_notes
            .push(NoteEditorState::load(note_a, NoteEditorMode::Normal));
        app.toggle_pin_focus(); // pins the floating editor as a third tab
        assert_eq!(app.pinned_notes.len(), 3);
        assert_eq!(app.active_pin, 2);

        app.cycle_pinned_note(true);
        assert_eq!(app.active_pin, 0, "wraps forward from the last tab");

        app.cycle_pinned_note(true);
        assert_eq!(app.active_pin, 1);

        app.cycle_pinned_note(false);
        assert_eq!(app.active_pin, 0);

        app.cycle_pinned_note(false);
        assert_eq!(app.active_pin, 2, "wraps backward from the first tab");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cycle_pinned_note_is_noop_with_zero_or_one_pinned_notes() {
        let dir = test_path().with_extension("notes");
        let cfg = Config {
            notes_dir: Some(dir.to_string_lossy().into_owned()),
            ..Config::default()
        };
        let mut app = build_app_with_config("Write PR summary +work\n", cfg.clone());
        app.cycle_pinned_note(true);
        app.cycle_pinned_note(false);
        assert_eq!(app.active_pin, 0);
        assert!(app.pinned_notes.is_empty());

        let mut app = app_with_open_editor(&dir);
        app.toggle_pin_focus(); // exactly one pinned
        app.cycle_pinned_note(true);
        assert_eq!(app.active_pin, 0, "nothing to cycle to");
        app.cycle_pinned_note(false);
        assert_eq!(app.active_pin, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn close_pinned_note_with_two_pinned_removes_only_the_active_tab() {
        let dir = test_path().with_extension("notes");
        let mut app = app_with_open_editor_and_second_file(&dir);
        app.toggle_pin_focus(); // tab 0 = a.md
        app.open_notes_for_current();
        app.notes_popup.move_down();
        app.open_note_editor_normal();
        app.toggle_pin_focus(); // tab 1 = b.md, active
        assert_eq!(app.active_pin, 1);

        app.close_pinned_note();

        assert_eq!(app.pinned_notes.len(), 1, "exactly one tab removed");
        assert_eq!(
            app.pinned_notes[0].lines(),
            &["content a"],
            "the other tab survives untouched"
        );
        assert!(
            app.active_pin < app.pinned_notes.len(),
            "active_pin never out of bounds"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn close_pinned_note_with_last_note_clears_focus_and_empties_the_collection() {
        let dir = test_path().with_extension("notes");
        let mut app = app_with_open_editor(&dir);
        app.toggle_pin_focus(); // exactly one pinned

        app.close_pinned_note();

        assert!(app.pinned_notes.is_empty());
        assert!(!app.pinned_focus);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
