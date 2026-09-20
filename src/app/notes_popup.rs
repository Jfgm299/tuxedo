//! State for the notes-list popup (`Mode::Notes`): browsing, create, rename,
//! delete (with confirmation) and unlink, all acting on the row currently
//! selected in the list. Selecting a file to open into the embedded editor
//! is wired in a later task (T6+ in `odd/tasks/notes-popup.md`).

use std::path::PathBuf;

use super::App;
use super::types::{Mode, View};
use crate::core::EditOutcome;
use crate::note;

/// What the inline text prompt on `NotesPopupState` is currently for. Paired
/// with the `prompt: Option<String>` buffer (rather than folded into a
/// single `Option<enum-with-buffer>`) so the many existing call sites and
/// tests that only care about the buffer's presence/content don't need to
/// reach through an extra layer — `prompt` and `prompt_kind` are always
/// `Some`/`None` together, kept in sync by `begin_prompt`/`cancel_prompt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotePromptKind {
    /// Prompting for a brand-new note's name (`n`).
    Create,
    /// Prompting for a new name for the file at this index into `files`
    /// (`r`), pre-filled with its current filename.
    Rename { index: usize },
}

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
    /// `Some(buffer)` while an inline text prompt (create or rename) is
    /// open; `None` while just browsing the list. Always `Some`/`None` in
    /// lockstep with `prompt_kind`.
    pub prompt: Option<String>,
    /// What `prompt` is for (create vs. rename-at-index). `None` iff
    /// `prompt` is `None`.
    pub prompt_kind: Option<NotePromptKind>,
    /// Index into `files` of the row awaiting delete confirmation ("Delete
    /// <name>? (y/n)"), or `None` while just browsing the list.
    pub pending_delete: Option<usize>,
}

impl NotesPopupState {
    pub fn new(files: Vec<PathBuf>) -> Self {
        Self {
            files,
            cursor: 0,
            folder: None,
            task_abs: None,
            prompt: None,
            prompt_kind: None,
            pending_delete: None,
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

    /// The cursor's index into `files`, or `None` on an empty list (where
    /// `cursor` is meaningless — mirrors the doc comment on `cursor`
    /// itself). Used to gate rename/delete/unlink as no-ops on an empty
    /// list without each call site re-checking `files.is_empty()`.
    pub fn selected_index(&self) -> Option<usize> {
        if self.files.is_empty() {
            None
        } else {
            Some(self.cursor)
        }
    }

    /// Clamp `cursor` back into bounds after `files` shrinks (delete/unlink
    /// refresh), the same way `move_down`/`move_up` already clamp: never
    /// left pointing past the end of a shrunk list, and reset to `0` if the
    /// list became empty.
    pub fn clamp_cursor(&mut self) {
        if self.files.is_empty() {
            self.cursor = 0;
        } else if self.cursor >= self.files.len() {
            self.cursor = self.files.len() - 1;
        }
    }

    /// Open an inline text prompt for the given purpose, pre-filled with
    /// `buffer` (empty for create, the current filename for rename).
    pub fn begin_prompt(&mut self, kind: NotePromptKind, buffer: String) {
        self.prompt = Some(buffer);
        self.prompt_kind = Some(kind);
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

    /// Close the prompt without creating or renaming anything.
    pub fn cancel_prompt(&mut self) {
        self.prompt = None;
        self.prompt_kind = None;
    }

    /// Open the delete-confirmation sub-state for the selected row. No-op on
    /// an empty list.
    pub fn begin_delete_confirm(&mut self) {
        self.pending_delete = self.selected_index();
    }

    /// Close the delete-confirmation sub-state without deleting anything.
    pub fn cancel_delete_confirm(&mut self) {
        self.pending_delete = None;
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
        self.notes_popup
            .begin_prompt(NotePromptKind::Create, String::new());
    }

    /// Open the inline rename prompt (`r` from the notes list) for the
    /// selected row, pre-filled with its current filename. No-op on an
    /// empty list.
    pub fn begin_rename_prompt(&mut self) {
        let Some(index) = self.notes_popup.selected_index() else {
            return;
        };
        let Some(path) = self.notes_popup.files.get(index) else {
            return;
        };
        let current_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.notes_popup
            .begin_prompt(NotePromptKind::Rename { index }, current_name);
    }

    /// Cancel the inline prompt (create or rename) without writing anything.
    pub fn cancel_note_prompt(&mut self) {
        self.notes_popup.cancel_prompt();
    }

    /// Confirm the inline prompt (Enter), dispatching to create or rename
    /// depending on what the prompt was opened for. A no-op that stays in
    /// the prompt on an empty name, for both purposes.
    pub fn confirm_note_prompt(&mut self) {
        let Some(kind) = self.notes_popup.prompt_kind else {
            return;
        };
        let Some(input) = self.notes_popup.prompt.clone() else {
            return;
        };
        let name = input.trim();
        if name.is_empty() {
            return;
        }

        match kind {
            NotePromptKind::Create => self.confirm_create_note(name),
            NotePromptKind::Rename { index } => self.confirm_rename_note(index, name),
        }
    }

    /// Create-note half of `confirm_note_prompt`: normalize the typed name
    /// into a `.md` filename (appending `.md` unless already present,
    /// case-insensitively), write `note::note_template` into the task's
    /// notes folder, and — for the task's very first note — append a
    /// `notes:<id>/` token to its todo.txt line. Blocked from
    /// `View::Archive` for both a first note and an additional one,
    /// mirroring the old `open_note_for_current_with_create`'s Archive
    /// restriction — creating a brand-new note is a write, unlike opening
    /// an existing one.
    fn confirm_create_note(&mut self, name: &str) {
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

    /// Rename-note half of `confirm_note_prompt`: normalize the typed name
    /// the same way create does, then `std::fs::rename` the file at `index`
    /// to it within the same folder. Renaming to the file's own current
    /// name (after normalization) is treated as a harmless no-op rather
    /// than an error. Refuses — with a flash message, staying in the
    /// prompt — a rename that would silently overwrite a *different*
    /// existing file.
    fn confirm_rename_note(&mut self, index: usize, name: &str) {
        let Some(folder) = self.notes_popup.folder.clone() else {
            self.notes_popup.cancel_prompt();
            return;
        };
        let Some(old_path) = self.notes_popup.files.get(index).cloned() else {
            self.notes_popup.cancel_prompt();
            return;
        };

        let file_name = normalize_note_file_name(name);
        let new_path = folder.dir.join(&file_name);

        if new_path == old_path {
            self.notes_popup.cancel_prompt();
            return;
        }
        if new_path.exists() {
            self.flash(format!("a note named {file_name} already exists"));
            return;
        }

        if let Err(e) = std::fs::rename(&old_path, &new_path) {
            self.flash(format!("note rename failed: {e}"));
            self.notes_popup.cancel_prompt();
            return;
        }

        self.notes_popup.files = note::list_notes(&folder.dir);
        match self.notes_popup.files.iter().position(|p| *p == new_path) {
            Some(pos) => self.notes_popup.cursor = pos,
            None => self.notes_popup.clamp_cursor(),
        }
        self.notes_popup.cancel_prompt();
    }

    /// Open the delete-confirmation sub-state (`d` from the notes list) for
    /// the selected row. No-op on an empty list.
    pub fn begin_delete_note_confirm(&mut self) {
        self.notes_popup.begin_delete_confirm();
    }

    /// Cancel the delete-confirmation sub-state without deleting anything.
    pub fn cancel_delete_note_confirm(&mut self) {
        self.notes_popup.cancel_delete_confirm();
    }

    /// Confirm the pending delete (`y`/Enter while `pending_delete` is
    /// `Some`): `std::fs::remove_file` the selected note, refresh the file
    /// list, and clamp the cursor the same way `move_down`/`move_up` do.
    pub fn confirm_delete_note(&mut self) {
        let Some(index) = self.notes_popup.pending_delete else {
            return;
        };
        let Some(folder) = self.notes_popup.folder.clone() else {
            self.notes_popup.cancel_delete_confirm();
            return;
        };
        let Some(path) = self.notes_popup.files.get(index).cloned() else {
            self.notes_popup.cancel_delete_confirm();
            return;
        };

        if let Err(e) = std::fs::remove_file(&path) {
            self.flash(format!("note delete failed: {e}"));
            self.notes_popup.cancel_delete_confirm();
            return;
        }

        self.notes_popup.files = note::list_notes(&folder.dir);
        self.notes_popup.clamp_cursor();
        self.notes_popup.cancel_delete_confirm();
    }

    /// Unlink the selected note (`u` from the notes list): a task's notes
    /// folder is exclusively that task's own notes, so "leave it orphaned
    /// on disk" means physically moving the file out of it into the shared
    /// `notes_dir/unlinked/` directory (see `note::unlink_note`), then
    /// refreshing the file list and clamping the cursor. No confirmation —
    /// unlike delete, this never touches the file's content. No-op on an
    /// empty list.
    pub fn unlink_selected_note(&mut self) {
        let Some(index) = self.notes_popup.selected_index() else {
            return;
        };
        let Some(folder) = self.notes_popup.folder.clone() else {
            return;
        };
        let Some(path) = self.notes_popup.files.get(index).cloned() else {
            return;
        };
        let notes_dir = self.notes_dir().clone();

        match note::unlink_note(&path, &notes_dir, &folder.id) {
            Ok(_) => {
                self.notes_popup.files = note::list_notes(&folder.dir);
                self.notes_popup.clamp_cursor();
            }
            Err(e) => self.flash(format!("note unlink failed: {e}")),
        }
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
        app.confirm_note_prompt();

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
        app.confirm_note_prompt();

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
        app.cancel_note_prompt();

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
        app.confirm_note_prompt();

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
        app.confirm_note_prompt();

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
        app.confirm_note_prompt();

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
        app.confirm_note_prompt();

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

    // ---- T5: rename ------------------------------------------------------

    fn app_with_two_notes(dir: &std::path::Path) -> App {
        let notes_folder = dir.join("tasks").join("abc123");
        std::fs::create_dir_all(&notes_folder).expect("create notes folder");
        std::fs::write(notes_folder.join("a.md"), "content a").expect("write a.md");
        std::fs::write(notes_folder.join("b.md"), "content b").expect("write b.md");
        let cfg = Config {
            notes_dir: Some(dir.to_string_lossy().into_owned()),
            ..Config::default()
        };
        let raw = "Write PR summary +work notes:abc123/\n";
        let mut app = build_app_with_config(raw, cfg);
        app.open_notes_for_current();
        app
    }

    #[test]
    fn begin_rename_prompt_prefills_current_file_name() {
        let dir = test_path().with_extension("notes");
        let mut app = app_with_two_notes(&dir);

        app.begin_rename_prompt();

        assert_eq!(app.notes_popup.prompt.as_deref(), Some("a.md"));
        assert_eq!(
            app.notes_popup.prompt_kind,
            Some(NotePromptKind::Rename { index: 0 })
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn confirm_rename_note_renames_file_on_disk_and_refreshes_list() {
        let dir = test_path().with_extension("notes");
        let mut app = app_with_two_notes(&dir);
        let notes_folder = app.notes_popup.folder.clone().expect("folder").dir;

        app.begin_rename_prompt();
        app.notes_popup.prompt = Some(String::new());
        for c in "renamed".chars() {
            app.notes_popup.prompt_push(c);
        }
        app.confirm_note_prompt();

        assert!(!notes_folder.join("a.md").exists());
        assert_eq!(
            std::fs::read_to_string(notes_folder.join("renamed.md")).expect("renamed exists"),
            "content a"
        );
        assert_eq!(
            app.notes_popup.files,
            vec![notes_folder.join("b.md"), notes_folder.join("renamed.md")]
        );
        assert_eq!(
            app.notes_popup.selected(),
            Some(&notes_folder.join("renamed.md")),
            "cursor should still point at the renamed file"
        );
        assert!(app.notes_popup.prompt.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn confirm_rename_note_appends_md_extension_when_missing() {
        let dir = test_path().with_extension("notes");
        let mut app = app_with_two_notes(&dir);
        let notes_folder = app.notes_popup.folder.clone().expect("folder").dir;

        app.begin_rename_prompt();
        app.notes_popup.prompt = Some(String::new());
        for c in "renamed".chars() {
            app.notes_popup.prompt_push(c);
        }
        app.confirm_note_prompt();

        assert!(notes_folder.join("renamed.md").exists());
        assert!(!notes_folder.join("renamed.md.md").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn confirm_rename_note_refuses_collision_with_another_existing_file() {
        let dir = test_path().with_extension("notes");
        let mut app = app_with_two_notes(&dir);
        let notes_folder = app.notes_popup.folder.clone().expect("folder").dir;

        app.begin_rename_prompt(); // selects a.md (cursor starts at 0)
        app.notes_popup.prompt = Some(String::new());
        for c in "b".chars() {
            app.notes_popup.prompt_push(c);
        }
        app.confirm_note_prompt();

        assert_eq!(app.flash_active(), Some("a note named b.md already exists"));
        assert!(
            app.notes_popup.prompt.is_some(),
            "must stay in the prompt on a collision"
        );
        assert!(notes_folder.join("a.md").exists(), "original untouched");
        assert_eq!(
            std::fs::read_to_string(notes_folder.join("a.md")).expect("a.md exists"),
            "content a"
        );
        assert_eq!(
            std::fs::read_to_string(notes_folder.join("b.md")).expect("b.md exists"),
            "content b"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn confirm_rename_note_to_its_own_current_name_is_a_harmless_noop() {
        let dir = test_path().with_extension("notes");
        let mut app = app_with_two_notes(&dir);
        let notes_folder = app.notes_popup.folder.clone().expect("folder").dir;

        app.begin_rename_prompt(); // "a.md" pre-filled
        app.confirm_note_prompt();

        assert!(app.flash_active().is_none());
        assert!(notes_folder.join("a.md").exists());
        assert_eq!(
            std::fs::read_to_string(notes_folder.join("a.md")).expect("a.md exists"),
            "content a"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancel_note_prompt_during_rename_changes_nothing_on_disk() {
        let dir = test_path().with_extension("notes");
        let mut app = app_with_two_notes(&dir);
        let notes_folder = app.notes_popup.folder.clone().expect("folder").dir;

        app.begin_rename_prompt();
        app.notes_popup.prompt = Some(String::new());
        for c in "renamed".chars() {
            app.notes_popup.prompt_push(c);
        }
        app.cancel_note_prompt();

        assert!(notes_folder.join("a.md").exists());
        assert!(!notes_folder.join("renamed.md").exists());
        assert!(app.notes_popup.prompt.is_none());
        assert!(app.notes_popup.prompt_kind.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn begin_rename_prompt_on_empty_list_is_noop() {
        let dir = test_path().with_extension("notes");
        let cfg = Config {
            notes_dir: Some(dir.to_string_lossy().into_owned()),
            ..Config::default()
        };
        let mut app = build_app_with_config("Write PR summary +work\n", cfg);
        app.open_notes_for_current();

        app.begin_rename_prompt();

        assert!(app.notes_popup.prompt.is_none());
        assert!(app.notes_popup.prompt_kind.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- T5: delete --------------------------------------------------------

    #[test]
    fn confirm_delete_note_removes_file_and_clamps_cursor_on_last_row() {
        let dir = test_path().with_extension("notes");
        let mut app = app_with_two_notes(&dir);
        let notes_folder = app.notes_popup.folder.clone().expect("folder").dir;
        app.notes_popup.cursor = 1; // b.md, the last row

        app.begin_delete_note_confirm();
        assert_eq!(app.notes_popup.pending_delete, Some(1));

        app.confirm_delete_note();

        assert!(!notes_folder.join("b.md").exists());
        assert_eq!(app.notes_popup.files, vec![notes_folder.join("a.md")]);
        assert_eq!(
            app.notes_popup.cursor, 0,
            "cursor must clamp back onto a.md"
        );
        assert!(app.notes_popup.pending_delete.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_confirm_n_leaves_file_untouched_and_returns_to_browsing() {
        let dir = test_path().with_extension("notes");
        let mut app = app_with_two_notes(&dir);
        let notes_folder = app.notes_popup.folder.clone().expect("folder").dir;

        app.begin_delete_note_confirm();
        app.cancel_delete_note_confirm();

        assert!(notes_folder.join("a.md").exists());
        assert!(app.notes_popup.pending_delete.is_none());
        assert_eq!(app.notes_popup.files.len(), 2, "list untouched");

        // Subsequent navigation still works — not stuck in the confirm state.
        app.notes_popup.move_down();
        assert_eq!(app.notes_popup.cursor, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn begin_delete_note_confirm_on_empty_list_is_noop() {
        let dir = test_path().with_extension("notes");
        let cfg = Config {
            notes_dir: Some(dir.to_string_lossy().into_owned()),
            ..Config::default()
        };
        let mut app = build_app_with_config("Write PR summary +work\n", cfg);
        app.open_notes_for_current();

        app.begin_delete_note_confirm();

        assert!(app.notes_popup.pending_delete.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- T5: unlink --------------------------------------------------------

    #[test]
    fn unlink_selected_note_moves_file_out_of_task_folder_and_clamps_cursor() {
        let dir = test_path().with_extension("notes");
        let mut app = app_with_two_notes(&dir);
        let notes_folder = app.notes_popup.folder.clone().expect("folder").dir;
        app.notes_popup.cursor = 1; // b.md, the last row

        app.unlink_selected_note();

        assert!(!notes_folder.join("b.md").exists());
        let unlinked_path = dir.join("unlinked").join("b.md");
        assert_eq!(
            std::fs::read_to_string(&unlinked_path).expect("unlinked file readable"),
            "content b"
        );
        assert_eq!(
            app.notes_popup.files,
            vec![notes_folder.join("a.md")],
            "no longer listed in the task's notes"
        );
        assert_eq!(app.notes_popup.cursor, 0, "cursor must clamp");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unlink_selected_note_prefixes_with_task_id_on_name_collision_across_tasks() {
        let dir = test_path().with_extension("notes");
        let unlinked_dir = dir.join("unlinked");
        std::fs::create_dir_all(&unlinked_dir).expect("create unlinked dir");
        std::fs::write(unlinked_dir.join("a.md"), "from a different task")
            .expect("pre-existing unlinked file");

        let mut app = app_with_two_notes(&dir);

        app.unlink_selected_note(); // cursor 0 == a.md

        assert_eq!(
            std::fs::read_to_string(unlinked_dir.join("a.md")).expect("original preserved"),
            "from a different task",
            "the pre-existing unlinked file from another task must survive"
        );
        assert_eq!(
            std::fs::read_to_string(unlinked_dir.join("abc123-a.md")).expect("prefixed file"),
            "content a"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unlink_selected_note_on_empty_list_is_noop() {
        let dir = test_path().with_extension("notes");
        let cfg = Config {
            notes_dir: Some(dir.to_string_lossy().into_owned()),
            ..Config::default()
        };
        let mut app = build_app_with_config("Write PR summary +work\n", cfg);
        app.open_notes_for_current();

        app.unlink_selected_note();

        assert!(app.notes_popup.files.is_empty());
        assert!(!dir.join("unlinked").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
