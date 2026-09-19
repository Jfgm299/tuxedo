//! State for the read-only notes-list popup (`Mode::Notes`).
//!
//! Scope for this task is read-only: list the current task's `.md` files
//! (resolved via `note::folder_for_task`/`note::list_notes`) and let the user
//! move a cursor through them. Selecting a file does nothing yet — wiring
//! `e`/`i` to actually open a file into the embedded editor is a later task
//! (T4+ in `odd/tasks/notes-popup.md`).

use std::path::PathBuf;

use super::App;
use super::types::Mode;
use crate::note;

/// Bespoke list-cursor state, following the same hand-rolled idiom used by
/// the rest of the app's pickers (there is no shared list component to
/// extend). Held on `App` as a plain field (mirrors `command_palette`,
/// `saved_pick_idx`, `theme_pick_orig`), not inside the `Mode` variant
/// itself — `Mode` stays a small `Copy` marker enum, matching how
/// `Mode::Insert`'s `DraftState` also lives on `App` rather than in the
/// enum.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotesPopupState {
    /// Absolute paths to the task's `.md` files, sorted (from `list_notes`).
    pub files: Vec<PathBuf>,
    /// Selected row index into `files`. Meaningless (and left at 0) when
    /// `files` is empty — renderers must check for the empty state
    /// separately rather than trusting this index.
    pub cursor: usize,
}

impl NotesPopupState {
    pub fn new(files: Vec<PathBuf>) -> Self {
        Self { files, cursor: 0 }
    }

    /// Move the cursor one row down, clamped to the last row. No-op on an
    /// empty list.
    pub fn move_down(&mut self) {
        if self.files.is_empty() {
            return;
        }
        self.cursor = (self.cursor + 1).min(self.files.len() - 1);
    }

    /// Move the cursor one row up, clamped at 0.
    pub fn move_up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// The path under the cursor, if any.
    pub fn selected(&self) -> Option<&PathBuf> {
        self.files.get(self.cursor)
    }
}

impl App {
    /// Open the notes popup for the current task: resolve its notes folder
    /// (creating no directory — `folder_for_task` only ever returns a
    /// candidate path), list the `.md` files inside it, and enter
    /// `Mode::Notes`. A task with no `notes:<id>/` token yet and/or a folder
    /// that doesn't exist on disk simply opens to an empty list — this task
    /// is read-only, so there is nothing to create or migrate here. (A
    /// legacy `note:<path>` task is likewise left untouched: migrating it
    /// into the new folder model is deferred to a later task, once the
    /// popup can actually rewrite the task line on create.)
    pub fn open_notes_for_current(&mut self) {
        let Some(task) = self.cur_task().cloned() else {
            return;
        };
        let folder = note::folder_for_task(&task, self.notes_dir());
        let files = note::list_notes(&folder.dir);
        self.notes_popup = NotesPopupState::new(files);
        self.mode = Mode::Notes;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Mode;
    use crate::app::test_support::{build_app_with_config, test_path};
    use crate::config::Config;

    #[test]
    fn open_notes_for_current_lists_md_files_from_existing_folder() {
        let dir = test_path().with_extension("notes");
        let notes_folder = dir.join("tasks").join("abc123");
        std::fs::create_dir_all(&notes_folder).expect("create notes folder");
        std::fs::write(notes_folder.join("a.md"), "a").expect("write a.md");
        std::fs::write(notes_folder.join("b.md"), "b").expect("write b.md");
        let cfg = Config {
            notes_dir: Some(dir.to_string_lossy().into_owned()),
            ..Config::default()
        };
        let mut app = build_app_with_config("Write PR summary +work notes:abc123/\n", cfg);

        app.open_notes_for_current();

        assert_eq!(app.mode, Mode::Notes);
        assert_eq!(
            app.notes_popup.files,
            vec![notes_folder.join("a.md"), notes_folder.join("b.md")]
        );
        assert_eq!(app.notes_popup.cursor, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_notes_for_current_with_no_token_and_missing_folder_is_empty_without_panicking() {
        let dir = test_path().with_extension("notes");
        let cfg = Config {
            notes_dir: Some(dir.to_string_lossy().into_owned()),
            ..Config::default()
        };
        let mut app = build_app_with_config("Write PR summary +work @desk\n", cfg);

        app.open_notes_for_current();

        assert_eq!(app.mode, Mode::Notes);
        assert!(app.notes_popup.files.is_empty());
        assert!(!dir.exists(), "resolving the folder must not create it");
    }

    #[test]
    fn move_down_and_up_clamp_at_bounds() {
        let mut state = NotesPopupState::new(vec![
            PathBuf::from("a.md"),
            PathBuf::from("b.md"),
            PathBuf::from("c.md"),
        ]);
        state.move_up();
        assert_eq!(state.cursor, 0, "cursor must not go below 0");

        state.move_down();
        state.move_down();
        assert_eq!(state.cursor, 2);
        state.move_down();
        assert_eq!(state.cursor, 2, "cursor must clamp at the last index");

        state.move_up();
        assert_eq!(state.cursor, 1);
    }

    #[test]
    fn move_down_and_up_on_empty_list_is_noop() {
        let mut state = NotesPopupState::default();
        state.move_down();
        assert_eq!(state.cursor, 0);
        state.move_up();
        assert_eq!(state.cursor, 0);
    }
}
