//! State for the read-only notes-list popup (`Mode::Notes`).
//!
//! Scope for this task is read-only: list the current task's `.md` files
//! (resolved via `note::folder_for_task`/`note::list_notes`) and let the user
//! move a cursor through them. Selecting a file does nothing yet — wiring
//! `e`/`i` to actually open a file into the embedded editor is a later task
//! (T4+ in `odd/tasks/notes-popup.md`).

use std::path::PathBuf;

use super::App;
use super::types::{Mode, View};
use crate::core::EditOutcome;
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
    /// The resolved notes folder for the task the popup is currently open
    /// for, stashed by `open_notes_for_current` (after any legacy-token
    /// migration) so create doesn't need to re-derive it. `None` only in
    /// the zeroed `Default` state before any popup has ever opened.
    pub folder: Option<note::NotesFolder>,
    /// Absolute index into `App::tasks()` for the task the popup is open
    /// for, so create/migrate can rewrite its todo.txt line without
    /// re-deriving the cursor. `None` in `View::Archive`, matching
    /// `App::cur_task_index_in_tasks`.
    pub task_abs: Option<usize>,
    /// `Some(buffer)` while the inline "new note name" prompt (`n` from the
    /// list) is open; `None` while just browsing the list.
    pub prompt: Option<String>,
}

impl NotesPopupState {
    pub fn new(files: Vec<PathBuf>) -> Self {
        Self {
            files,
            cursor: 0,
            folder: None,
            task_abs: None,
            prompt: None,
        }
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

    /// Open the inline "new note name" prompt with an empty buffer.
    pub fn begin_prompt(&mut self) {
        self.prompt = Some(String::new());
    }

    /// Append a character to the prompt buffer. No-op if the prompt isn't
    /// open (defensive — callers gate on `prompt.is_some()` already).
    pub fn prompt_push(&mut self, c: char) {
        if let Some(buf) = self.prompt.as_mut() {
            buf.push(c);
        }
    }

    /// Remove the last character from the prompt buffer. No-op if the
    /// prompt isn't open or is already empty.
    pub fn prompt_backspace(&mut self) {
        if let Some(buf) = self.prompt.as_mut() {
            buf.pop();
        }
    }

    /// Close the prompt without creating anything.
    pub fn cancel_prompt(&mut self) {
        self.prompt = None;
    }
}

impl App {
    /// Open the notes popup for the current task: resolve its notes folder,
    /// migrate a legacy `note:<path>` token into it if one exists (see
    /// `migrate_legacy_note` below), list the `.md` files inside it, and
    /// enter `Mode::Notes`. A task with no `notes:`/`note:` token and/or a
    /// folder that doesn't exist on disk simply opens to an empty list.
    pub fn open_notes_for_current(&mut self) {
        let Some(task) = self.cur_task().cloned() else {
            return;
        };
        let task_abs = self.cur_task_index_in_tasks();
        let notes_dir = self.notes_dir().clone();
        let mut folder = note::folder_for_task(&task, &notes_dir);

        if !folder.existed_in_task
            && let Some(abs) = task_abs
            && let Some(rel) = note::legacy_note_rel_from_raw(&task.raw)
            && let Some(migrated) = self.migrate_legacy_note(abs, &task.raw, &rel, &folder)
        {
            folder = migrated;
        }

        let files = note::list_notes(&folder.dir);
        self.notes_popup = NotesPopupState::new(files);
        self.notes_popup.folder = Some(folder);
        self.notes_popup.task_abs = task_abs;
        self.mode = Mode::Notes;
    }

    /// Migrate a task's legacy `note:<path>` token into the new
    /// `notes:<id>/` folder model: move the file on disk into `candidate`'s
    /// folder, then rewrite the task line in a single edit (drop the old
    /// token, append `notes:<id>/`) so undo/redo sees one step. Only called
    /// with `abs` from `task_abs = Some(_)`, i.e. `View::List` — archived
    /// tasks are never migrated here because the `Store` has no primitive
    /// to rewrite an archived task's persisted `done.txt` line (only
    /// `archive_completed`/`unarchive`/`archive_delete` touch it); adding
    /// that capability is out of scope for this task. Returns the migrated
    /// `NotesFolder` (now `existed_in_task: true`) on success, or `None` if
    /// the file move or line rewrite failed — the caller then falls back to
    /// `candidate` (the pre-migration, not-yet-linked folder).
    fn migrate_legacy_note(
        &mut self,
        abs: usize,
        raw: &str,
        rel: &str,
        candidate: &note::NotesFolder,
    ) -> Option<note::NotesFolder> {
        let old_path = self.notes_dir().join(rel);
        note::migrate_legacy_note(&old_path, &candidate.dir).ok()?;

        let without_legacy_token: Vec<&str> = raw
            .split_whitespace()
            .filter(|token| {
                if token.starts_with("notes:") {
                    return true;
                }
                match token.strip_prefix("note:") {
                    Some(v) => v.trim_matches('"') != rel,
                    None => true,
                }
            })
            .collect();
        let new_raw = format!("{} notes:{}/", without_legacy_token.join(" "), candidate.id);

        match self.store.edit_line(abs, &new_raw) {
            EditOutcome::Saved { abs } => {
                self.after_mutation(abs);
                let mut migrated = candidate.clone();
                migrated.existed_in_task = true;
                Some(migrated)
            }
            EditOutcome::Aborted(r) => {
                self.handle_reconcile_abort(r);
                None
            }
            EditOutcome::Error(e) => {
                self.flash(format!("note migration failed: {e}"));
                None
            }
            EditOutcome::Empty | EditOutcome::OutOfRange | EditOutcome::TermNotFound => None,
        }
    }

    /// Open the inline "new note name" prompt (`n` from the notes list).
    pub fn begin_new_note_prompt(&mut self) {
        self.notes_popup.begin_prompt();
    }

    /// Cancel the inline "new note name" prompt without creating anything.
    pub fn cancel_new_note_prompt(&mut self) {
        self.notes_popup.cancel_prompt();
    }

    /// Confirm the inline "new note name" prompt (Enter): normalize the
    /// typed name into a `.md` filename (appending `.md` unless already
    /// present, case-insensitively), write `note::note_template` into the
    /// task's notes folder, and — for the task's very first note — append a
    /// `notes:<id>/` token to its todo.txt line. A no-op that stays in the
    /// prompt on an empty name. Blocked from `View::Archive` for both a
    /// first note and an additional one, mirroring the old
    /// `open_note_for_current_with_create`'s Archive restriction — creating
    /// a brand-new note is a write, unlike opening an existing one.
    pub fn confirm_new_note_prompt(&mut self) {
        let Some(input) = self.notes_popup.prompt.clone() else {
            return;
        };
        let name = input.trim();
        if name.is_empty() {
            return;
        }

        if matches!(self.view(), View::Archive) {
            self.flash("archived task has no note");
            self.notes_popup.cancel_prompt();
            return;
        }

        let Some(folder) = self.notes_popup.folder.clone() else {
            self.notes_popup.cancel_prompt();
            return;
        };
        let Some(task) = self.cur_task().cloned() else {
            self.notes_popup.cancel_prompt();
            return;
        };

        let file_name = normalize_note_file_name(name);
        let path = folder.dir.join(&file_name);

        if let Err(e) = std::fs::create_dir_all(&folder.dir) {
            self.flash(format!("note mkdir failed: {e}"));
            self.notes_popup.cancel_prompt();
            return;
        }
        if let Err(e) = std::fs::write(&path, note::note_template(&task)) {
            self.flash(format!("note write failed: {e}"));
            self.notes_popup.cancel_prompt();
            return;
        }

        if !folder.existed_in_task {
            if let Some(abs) = self.notes_popup.task_abs {
                match self.store.append_at(abs, &format!("notes:{}/", folder.id)) {
                    EditOutcome::Saved { abs } => self.after_mutation(abs),
                    EditOutcome::Aborted(r) => self.handle_reconcile_abort(r),
                    EditOutcome::Error(e) => self.flash(format!("note link failed: {e}")),
                    EditOutcome::Empty | EditOutcome::OutOfRange | EditOutcome::TermNotFound => {}
                }
            }
            let mut linked = folder.clone();
            linked.existed_in_task = true;
            self.notes_popup.folder = Some(linked);
        }

        self.notes_popup.files = note::list_notes(&folder.dir);
        self.notes_popup.cancel_prompt();
    }
}

/// Normalize a user-typed note name into a `.md` filename: append `.md`
/// unless the name already ends in it, case-insensitively (`foo.md` stays
/// `foo.md`, it isn't doubled up into `foo.md.md`).
fn normalize_note_file_name(name: &str) -> String {
    if name.to_ascii_lowercase().ends_with(".md") {
        name.to_string()
    } else {
        format!("{name}.md")
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

    // ---- T4: create-note flow -------------------------------------------

    #[test]
    fn confirm_new_note_prompt_appends_md_extension_when_missing() {
        let dir = test_path().with_extension("notes");
        let cfg = Config {
            notes_dir: Some(dir.to_string_lossy().into_owned()),
            ..Config::default()
        };
        let mut app = build_app_with_config("Write PR summary +work\n", cfg);
        app.open_notes_for_current();
        let folder_dir = app.notes_popup.folder.clone().expect("folder resolved").dir;

        app.begin_new_note_prompt();
        for c in "foo".chars() {
            app.notes_popup.prompt_push(c);
        }
        app.confirm_new_note_prompt();

        assert!(folder_dir.join("foo.md").exists());
        assert!(!folder_dir.join("foo.md.md").exists());
        assert!(app.notes_popup.prompt.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn confirm_new_note_prompt_does_not_double_extension_when_already_present() {
        let dir = test_path().with_extension("notes");
        let cfg = Config {
            notes_dir: Some(dir.to_string_lossy().into_owned()),
            ..Config::default()
        };
        let mut app = build_app_with_config("Write PR summary +work\n", cfg);
        app.open_notes_for_current();
        let folder_dir = app.notes_popup.folder.clone().expect("folder resolved").dir;

        app.begin_new_note_prompt();
        for c in "foo.md".chars() {
            app.notes_popup.prompt_push(c);
        }
        app.confirm_new_note_prompt();

        assert!(folder_dir.join("foo.md").exists());
        assert!(!folder_dir.join("foo.md.md").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancel_new_note_prompt_creates_nothing() {
        let dir = test_path().with_extension("notes");
        let cfg = Config {
            notes_dir: Some(dir.to_string_lossy().into_owned()),
            ..Config::default()
        };
        let mut app = build_app_with_config("Write PR summary +work\n", cfg);
        app.open_notes_for_current();

        app.begin_new_note_prompt();
        for c in "foo".chars() {
            app.notes_popup.prompt_push(c);
        }
        app.cancel_new_note_prompt();

        assert!(app.notes_popup.prompt.is_none());
        assert!(!dir.exists(), "cancelling must not create the notes dir");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn confirm_new_note_prompt_on_empty_input_is_noop_and_stays_in_prompt() {
        let dir = test_path().with_extension("notes");
        let cfg = Config {
            notes_dir: Some(dir.to_string_lossy().into_owned()),
            ..Config::default()
        };
        let mut app = build_app_with_config("Write PR summary +work\n", cfg);
        app.open_notes_for_current();

        app.begin_new_note_prompt();
        app.confirm_new_note_prompt();

        assert_eq!(app.notes_popup.prompt, Some(String::new()));
        assert!(!dir.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn confirm_new_note_prompt_first_note_creates_folder_writes_template_and_appends_token() {
        let dir = test_path().with_extension("notes");
        let cfg = Config {
            notes_dir: Some(dir.to_string_lossy().into_owned()),
            ..Config::default()
        };
        let mut app = build_app_with_config("Write PR summary +work\n", cfg);
        app.open_notes_for_current();
        assert!(
            !app.notes_popup
                .folder
                .as_ref()
                .expect("folder resolved")
                .existed_in_task
        );

        app.begin_new_note_prompt();
        for c in "foo".chars() {
            app.notes_popup.prompt_push(c);
        }
        app.confirm_new_note_prompt();

        let folder = app.notes_popup.folder.clone().expect("folder resolved");
        assert!(folder.existed_in_task);
        let content = std::fs::read_to_string(folder.dir.join("foo.md")).expect("note written");
        assert!(content.starts_with("# Write PR summary\n"));
        assert!(content.contains("## My notes\n\n"));

        let raw = &app.tasks()[0].raw;
        assert!(
            raw.contains(&format!("notes:{}/", folder.id)),
            "task line should gain the notes:<id>/ token: {raw}"
        );
        assert_eq!(
            app.notes_popup.files,
            vec![folder.dir.join("foo.md")],
            "popup file list must refresh after create"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn confirm_new_note_prompt_second_note_reuses_existing_folder_without_rewriting_line() {
        let dir = test_path().with_extension("notes");
        let notes_folder = dir.join("tasks").join("abc123");
        std::fs::create_dir_all(&notes_folder).expect("create notes folder");
        std::fs::write(notes_folder.join("a.md"), "a").expect("write a.md");
        let cfg = Config {
            notes_dir: Some(dir.to_string_lossy().into_owned()),
            ..Config::default()
        };
        let raw = "Write PR summary +work notes:abc123/\n";
        let mut app = build_app_with_config(raw, cfg);
        app.open_notes_for_current();

        app.begin_new_note_prompt();
        for c in "b".chars() {
            app.notes_popup.prompt_push(c);
        }
        app.confirm_new_note_prompt();

        assert!(notes_folder.join("b.md").exists());
        assert_eq!(app.tasks()[0].raw, raw.trim(), "line must not be rewritten");
        assert_eq!(
            app.notes_popup.files,
            vec![notes_folder.join("a.md"), notes_folder.join("b.md")]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn confirm_new_note_prompt_blocked_in_archive_view() {
        let dir = test_path().with_extension("notes");
        let cfg = Config {
            notes_dir: Some(dir.to_string_lossy().into_owned()),
            ..Config::default()
        };
        let mut app = build_app_with_config("a\n", cfg);
        let path = app.archive().path().to_path_buf();
        app.store.archive = crate::app::Archive::for_test(
            crate::todo::parse_file("x 2026-05-01 2026-04-01 archived task\n"),
            String::new(),
            path,
        );
        app.set_view(crate::app::View::Archive);
        app.open_notes_for_current();

        app.begin_new_note_prompt();
        for c in "foo".chars() {
            app.notes_popup.prompt_push(c);
        }
        app.confirm_new_note_prompt();

        assert_eq!(app.flash_active(), Some("archived task has no note"));
        assert!(!dir.exists(), "nothing should be written to disk");
        assert!(app.notes_popup.prompt.is_none(), "prompt still closes");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- T4: legacy `note:<path>` migration on popup open ----------------

    #[test]
    fn open_notes_for_current_migrates_legacy_note_token() {
        let dir = test_path().with_extension("notes");
        let old_path = dir.join("projects/example.md");
        std::fs::create_dir_all(old_path.parent().expect("old_path has a parent"))
            .expect("create old parent");
        std::fs::write(&old_path, "# Existing\n\nlegacy content\n").expect("write legacy note");
        let cfg = Config {
            notes_dir: Some(dir.to_string_lossy().into_owned()),
            ..Config::default()
        };
        let raw = "Write PR summary +work note:projects/example.md\n";
        let mut app = build_app_with_config(raw, cfg);

        app.open_notes_for_current();

        assert!(!old_path.exists(), "legacy file must be moved, not copied");
        let folder = app.notes_popup.folder.clone().expect("folder resolved");
        assert!(folder.existed_in_task);
        let migrated_path = folder.dir.join("example.md");
        assert_eq!(
            std::fs::read_to_string(&migrated_path).expect("migrated file exists"),
            "# Existing\n\nlegacy content\n"
        );
        assert_eq!(app.notes_popup.files, vec![migrated_path]);

        let task_raw = &app.tasks()[0].raw;
        assert!(
            !task_raw.contains("note:projects/example.md"),
            "legacy token must be removed: {task_raw}"
        );
        assert!(
            task_raw.contains(&format!("notes:{}/", folder.id)),
            "new token must be present: {task_raw}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_notes_for_current_does_not_migrate_a_task_that_already_has_a_notes_token() {
        let dir = test_path().with_extension("notes");
        let notes_folder = dir.join("tasks").join("abc123");
        std::fs::create_dir_all(&notes_folder).expect("create notes folder");
        std::fs::write(notes_folder.join("a.md"), "a").expect("write a.md");
        let cfg = Config {
            notes_dir: Some(dir.to_string_lossy().into_owned()),
            ..Config::default()
        };
        let raw = "Write PR summary +work notes:abc123/\n";
        let mut app = build_app_with_config(raw, cfg);

        app.open_notes_for_current();

        assert_eq!(app.tasks()[0].raw, raw.trim());
        assert_eq!(app.notes_popup.files, vec![notes_folder.join("a.md")]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_notes_for_current_does_not_migrate_from_archive_view() {
        // No Store primitive rewrites an archived task's persisted done.txt
        // line (only archive_completed/unarchive/archive_delete touch it),
        // so migration is deliberately skipped from View::Archive — see the
        // task report for the full reasoning. The legacy file is left in
        // place untouched.
        let dir = test_path().with_extension("notes");
        let old_path = dir.join("projects/example.md");
        std::fs::create_dir_all(old_path.parent().expect("old_path has a parent"))
            .expect("create old parent");
        std::fs::write(&old_path, "legacy content").expect("write legacy note");
        let cfg = Config {
            notes_dir: Some(dir.to_string_lossy().into_owned()),
            ..Config::default()
        };
        let mut app = build_app_with_config("a\n", cfg);
        let path = app.archive().path().to_path_buf();
        let archived_raw = "x 2026-05-01 2026-04-01 archived task note:projects/example.md";
        app.store.archive = crate::app::Archive::for_test(
            crate::todo::parse_file(&format!("{archived_raw}\n")),
            String::new(),
            path,
        );
        app.set_view(crate::app::View::Archive);

        app.open_notes_for_current();

        assert!(old_path.exists(), "legacy file must be left in place");
        assert_eq!(app.archive().tasks()[0].raw, archived_raw);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
