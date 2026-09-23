//! The embedded markdown editor's buffer + cursor + Normal/Insert sub-mode
//! (`NotesPopupState::active_editor`), wired into `Mode::Notes` — see
//! `odd/tasks/notes-popup.md` T6+T7. No `$EDITOR` shell-out, no PTY, no real
//! nvim: this *is* the buffer, rendered by `src/ui/note_editor.rs`.
//!
//! MVP scope only, deliberately: up/down/left/right motion (arrows and
//! `hjkl` in Normal), `i` to enter Insert, Esc in Insert returns to Normal
//! (not out of the editor — stepping all the way out to the notes list is
//! the caller's job, see `App::close_note_editor` and
//! `main.rs::handle_notes`), plain character typing/Enter/Backspace in
//! Insert, and a single save key. `w`/`b`/`e`/`dd`/`yy`/`gg`/`G`/visual
//! mode/search are explicitly out of scope — follow-up work if actually
//! missed later, not a gap to quietly patch in here.
//!
//! T13 adds a `folke/noice.nvim`-style `:`-command prompt (see
//! `odd/tasks/notes-popup.md`'s Round 2 exploration note): `:` from the
//! editor's Normal sub-mode opens `command_prompt: Option<String>` on this
//! same struct (deliberately, not on `NotesPopupState` or a per-context type
//! — see the module's doc below on why), Enter parses/executes the MVP
//! `w`/`q`/`wq`/`x` command set via [`NoteEditorState::execute_command_prompt`],
//! Esc cancels. The key *dispatch* for `:`/typing/Backspace/Enter/Esc lives
//! in `main.rs::handle_note_editor_normal` (already shared by the floating
//! popup and every pinned tab — see `NoteEditorSignal`'s doc comment there),
//! and the command *parsing and execution* lives here, so both are written
//! exactly once and work identically in both contexts.

use std::path::PathBuf;

use super::App;

/// Normal vs Insert sub-mode for the embedded editor. Kept as its own small
/// enum rather than reusing `DialogInputMode` (the single-line draft
/// dialog's identically-shaped Normal/Insert enum): the note editor is a
/// categorically different multi-line buffer with its own motions, and
/// coupling the two would tie unrelated widgets together for no benefit
/// beyond a coincidentally matching shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteEditorMode {
    Normal,
    Insert,
}

/// The embedded markdown editor's state for exactly one open file at a time.
///
/// Lines are stored as `Vec<String>`, one `String` per line — the simplest
/// correct representation for a small markdown file; this is not a
/// rope/piece-table document editor. The cursor's column
/// (`cursor_col`) is a **character offset**, not a byte offset, into
/// `lines[cursor_line]`: every read or mutation goes through
/// `chars()`/`char_indices()` (see `byte_offset` below) rather than raw byte
/// indexing, so accented/non-ASCII text (Spanish task names are already a
/// precedent elsewhere in this app, e.g. `note.rs`'s `fold_char`/slugify)
/// can't land the cursor mid-codepoint or panic on insert/backspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteEditorState {
    path: PathBuf,
    lines: Vec<String>,
    cursor_line: usize,
    cursor_col: usize,
    mode: NoteEditorMode,
    dirty: bool,
    /// T13's `:`-command prompt buffer: `None` when closed, `Some(text)`
    /// while open (the `:` keystroke that opened it is the trigger, not
    /// part of `text` — matches real vim). Lives here rather than on
    /// `NotesPopupState` so the floating popup and every pinned tab share
    /// one implementation automatically (see the module doc).
    command_prompt: Option<String>,
}

/// What [`NoteEditorState::execute_command_prompt`] decided for the typed
/// command. The caller (`main.rs::handle_note_editor_normal`, shared by both
/// the floating popup and every pinned tab) maps this onto
/// `NoteEditorSignal` — `Ok`/`Error` correspond to the existing
/// `Handled`/`SaveFailed` signals, `CloseRequested` to the new
/// `NoteEditorSignal::CloseRequested` variant — since only the caller knows
/// what "close" means in its own context (pop back to the list vs. close a
/// pinned tab).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteCommandResult {
    /// The command ran with no further action needed (e.g. `:w` succeeded).
    /// The prompt is already closed.
    Ok,
    /// `:q`, or `:wq`/`:x` after a successful save: the editor asked to be
    /// closed. The prompt is already closed.
    CloseRequested,
    /// An unknown/empty command, or a save failure on `:w`/`:wq`/`:x`. The
    /// message should be flashed; the prompt is already closed, but the
    /// editor itself stays open (a `:wq`/`:x` save failure deliberately does
    /// NOT close — see the module-level rationale on
    /// `execute_command_prompt`).
    Error(String),
}

impl NoteEditorState {
    /// Load `path`'s content into the buffer, starting in `mode`. A missing
    /// or empty file yields a single empty line rather than panicking or
    /// erroring — a freshly created note already has content via
    /// `note::note_template`, but the editor must stay defensive for any
    /// other path it's pointed at (or a file deleted from under it).
    pub fn load(path: PathBuf, mode: NoteEditorMode) -> Self {
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        Self {
            path,
            lines: lines_from_content(&content),
            cursor_line: 0,
            cursor_col: 0,
            mode,
            dirty: false,
            command_prompt: None,
        }
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn cursor_line(&self) -> usize {
        self.cursor_line
    }

    pub fn cursor_col(&self) -> usize {
        self.cursor_col
    }

    pub fn mode(&self) -> NoteEditorMode {
        self.mode
    }

    pub fn dirty(&self) -> bool {
        self.dirty
    }

    /// Write the buffer back to disk: lines joined with `\n`, plus one
    /// trailing `\n` (ordinary POSIX text-file convention). This exact shape
    /// is what `lines_from_content` reverses on the next `load`, so
    /// save-then-load round-trips losslessly regardless of how many trailing
    /// blank lines the buffer has.
    pub fn save(&mut self) -> std::io::Result<()> {
        std::fs::write(&self.path, format!("{}\n", self.lines.join("\n")))?;
        self.dirty = false;
        Ok(())
    }

    fn current_line_len(&self) -> usize {
        self.lines[self.cursor_line].chars().count()
    }

    fn clamp_col(&mut self) {
        self.cursor_col = self.cursor_col.min(self.current_line_len());
    }

    // ---- Normal-mode motions ---------------------------------------------

    /// Move down one line, clamped to the last line. The column is clamped
    /// to the new line's length (never left past its end).
    pub fn move_down(&mut self) {
        if self.cursor_line + 1 < self.lines.len() {
            self.cursor_line += 1;
            self.clamp_col();
        }
    }

    /// Move up one line, clamped at line 0.
    pub fn move_up(&mut self) {
        self.cursor_line = self.cursor_line.saturating_sub(1);
        self.clamp_col();
    }

    /// Move left one character, clamped at column 0. Does not wrap onto the
    /// previous line (MVP scope — see the module doc).
    pub fn move_left(&mut self) {
        self.cursor_col = self.cursor_col.saturating_sub(1);
    }

    /// Move right one character, clamped at the line's length. Does not wrap
    /// onto the next line.
    pub fn move_right(&mut self) {
        self.cursor_col = (self.cursor_col + 1).min(self.current_line_len());
    }

    /// Enter Insert sub-mode (`i`).
    pub fn enter_insert(&mut self) {
        self.mode = NoteEditorMode::Insert;
    }

    /// Esc while in Insert: back to Normal, staying inside the editor. Esc
    /// from Normal itself is handled one layer up by the caller clearing
    /// `NotesPopupState::active_editor` (see `App::close_note_editor`) —
    /// this method never leaves the editor.
    pub fn esc_to_normal(&mut self) {
        self.mode = NoteEditorMode::Normal;
    }

    // ---- T13: `:`-command prompt -------------------------------------------

    /// The prompt's current buffer, or `None` while it's closed. Rendering
    /// (`src/ui/note_editor.rs`) and the key dispatcher
    /// (`main.rs::handle_note_editor_normal`, and its callers deciding
    /// whether `z`/`Z`/`Tab`/`BackTab` should intercept a key ahead of the
    /// editor) both read this to know whether the prompt is on screen and
    /// consuming keys.
    pub fn command_prompt(&self) -> Option<&str> {
        self.command_prompt.as_deref()
    }

    /// `:` from Normal sub-mode: open the prompt with an empty buffer. The
    /// triggering `:` itself is never part of the buffer (matches real vim).
    pub fn open_command_prompt(&mut self) {
        self.command_prompt = Some(String::new());
    }

    /// Append `c` to the buffer. A no-op if the prompt isn't open.
    pub fn command_prompt_push(&mut self, c: char) {
        if let Some(buf) = self.command_prompt.as_mut() {
            buf.push(c);
        }
    }

    /// Remove the last character from the buffer. A no-op on an empty
    /// buffer or if the prompt isn't open (Backspace never closes the
    /// prompt itself — only Esc/Enter do).
    pub fn command_prompt_backspace(&mut self) {
        if let Some(buf) = self.command_prompt.as_mut() {
            buf.pop();
        }
    }

    /// Esc: cancel the prompt with no side effects — no save, no close
    /// signal, buffer discarded.
    pub fn cancel_command_prompt(&mut self) {
        self.command_prompt = None;
    }

    /// Enter: parse and execute the buffered command, closing the prompt
    /// either way (the caller decides what happens next from the returned
    /// [`NoteCommandResult`]). MVP command set only:
    ///
    /// - `w` — save. Success: [`NoteCommandResult::Ok`]. Failure: the error
    ///   is flashed and the editor stays open (same as `Ctrl+S`'s existing
    ///   failure handling) — matches this task's read that a save failure
    ///   should never look like nothing happened, but also should never
    ///   silently discard unsaved work by closing anyway.
    /// - `q` — request a close with no save attempt.
    /// - `wq`/`x` — save, then request a close **only if the save
    ///   succeeded**. On a save failure the editor stays open with the error
    ///   flashed, exactly like plain `:w`: closing anyway on a failed save
    ///   would silently discard the very edit the user just tried to
    ///   persist, which is worse than making them retry.
    /// - anything else, including an empty buffer: an error is flashed
    ///   (`"no command"` for empty, `"unknown command: {input}"` otherwise)
    ///   and nothing else happens — no save, no close, editor stays in
    ///   Normal sub-mode.
    ///
    /// A no-op call (prompt already closed) returns
    /// `NoteCommandResult::Error("no command".into())` for the same reason
    /// an empty buffer does — there is nothing sensible to execute.
    pub fn execute_command_prompt(&mut self) -> NoteCommandResult {
        let input = self.command_prompt.take().unwrap_or_default();
        let cmd = input.trim();
        if cmd.is_empty() {
            return NoteCommandResult::Error("no command".to_string());
        }
        match cmd {
            "w" => match self.save() {
                Ok(()) => NoteCommandResult::Ok,
                Err(e) => NoteCommandResult::Error(format!("note save failed: {e}")),
            },
            "q" => NoteCommandResult::CloseRequested,
            "wq" | "x" => match self.save() {
                Ok(()) => NoteCommandResult::CloseRequested,
                Err(e) => NoteCommandResult::Error(format!("note save failed: {e}")),
            },
            other => NoteCommandResult::Error(format!("unknown command: {other}")),
        }
    }

    // ---- Insert-mode editing ----------------------------------------------

    /// Insert `c` at the cursor and advance past it.
    pub fn insert_char(&mut self, c: char) {
        let byte = byte_offset(&self.lines[self.cursor_line], self.cursor_col);
        self.lines[self.cursor_line].insert(byte, c);
        self.cursor_col += 1;
        self.dirty = true;
    }

    /// Split the current line at the cursor (Enter): the text before the
    /// cursor stays on `cursor_line`, the text after it becomes a new line
    /// right below, and the cursor moves to column 0 of that new line.
    pub fn split_line(&mut self) {
        let byte = byte_offset(&self.lines[self.cursor_line], self.cursor_col);
        let rest = self.lines[self.cursor_line].split_off(byte);
        self.lines.insert(self.cursor_line + 1, rest);
        self.cursor_line += 1;
        self.cursor_col = 0;
        self.dirty = true;
    }

    /// Delete the character before the cursor. At column 0 of a non-first
    /// line, joins the current line onto the end of the previous one instead
    /// (the cursor lands at the join point, i.e. the previous line's old
    /// length). A no-op at the very start of the buffer (line 0, column 0) —
    /// nothing precedes the cursor to delete or join into.
    pub fn backspace(&mut self) {
        if self.cursor_col > 0 {
            let line = &mut self.lines[self.cursor_line];
            let start = byte_offset(line, self.cursor_col - 1);
            let end = byte_offset(line, self.cursor_col);
            line.drain(start..end);
            self.cursor_col -= 1;
            self.dirty = true;
        } else if self.cursor_line > 0 {
            let current = self.lines.remove(self.cursor_line);
            self.cursor_line -= 1;
            let prev_len = self.current_line_len();
            self.lines[self.cursor_line].push_str(&current);
            self.cursor_col = prev_len;
            self.dirty = true;
        }
    }
}

/// Convert a char index into a byte offset within `s`, for indexing/slicing
/// operations that need a byte position (`String::insert`/`drain` etc.).
/// Falls back to `s.len()` past the end, which is always a valid boundary.
fn byte_offset(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}

/// Split file content into buffer lines, undoing exactly the shape `save`
/// writes: `\n`-joined lines plus one trailing `\n`. A single trailing
/// newline is treated as "no extra blank line" (the ordinary POSIX
/// convention), so save-then-load round-trips without growing the file.
/// Empty content yields a single empty line rather than an empty `Vec` —
/// the buffer always has at least one line to put a cursor on.
fn lines_from_content(content: &str) -> Vec<String> {
    if content.is_empty() {
        return vec![String::new()];
    }
    let mut lines: Vec<String> = content.split('\n').map(str::to_string).collect();
    if lines.len() > 1 && lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// App-level delegators mirroring `draft.rs`'s `app.draft_*()` surface: the
/// actual logic lives on `NoteEditorState`, these just route through
/// `NotesPopupState::active_editor` and stay no-ops when it's `None` (the
/// editor isn't open, or was just closed by the same key that reached here).
impl App {
    /// Load the selected note into the embedded editor, starting in Normal
    /// sub-mode (`e` from the notes list, mirrors `draft_set`). No-op on an
    /// empty list — nothing selected to open.
    pub fn open_note_editor_normal(&mut self) {
        self.open_note_editor(NoteEditorMode::Normal);
    }

    /// Same as `open_note_editor_normal`, but starting in Insert sub-mode
    /// (`i` from the notes list, mirrors `draft_set_insert`).
    pub fn open_note_editor_insert(&mut self) {
        self.open_note_editor(NoteEditorMode::Insert);
    }

    fn open_note_editor(&mut self, mode: NoteEditorMode) {
        let Some(path) = self.notes_popup.selected().cloned() else {
            return;
        };
        self.notes_popup.active_editor = Some(NoteEditorState::load(path, mode));
    }

    /// Esc from the editor's Normal sub-mode: step back out to the notes
    /// list. `Mode::Notes` itself is untouched — a second Esc from the bare
    /// list is what closes the whole popup (see `main.rs::handle_notes`).
    pub fn close_note_editor(&mut self) {
        self.notes_popup.active_editor = None;
    }

    pub fn note_editor_enter_insert(&mut self) {
        if let Some(editor) = self.notes_popup.active_editor.as_mut() {
            editor.enter_insert();
        }
    }

    pub fn note_editor_esc_to_normal(&mut self) {
        if let Some(editor) = self.notes_popup.active_editor.as_mut() {
            editor.esc_to_normal();
        }
    }

    pub fn note_editor_move_down(&mut self) {
        if let Some(editor) = self.notes_popup.active_editor.as_mut() {
            editor.move_down();
        }
    }

    pub fn note_editor_move_up(&mut self) {
        if let Some(editor) = self.notes_popup.active_editor.as_mut() {
            editor.move_up();
        }
    }

    pub fn note_editor_move_left(&mut self) {
        if let Some(editor) = self.notes_popup.active_editor.as_mut() {
            editor.move_left();
        }
    }

    pub fn note_editor_move_right(&mut self) {
        if let Some(editor) = self.notes_popup.active_editor.as_mut() {
            editor.move_right();
        }
    }

    pub fn note_editor_insert_char(&mut self, c: char) {
        if let Some(editor) = self.notes_popup.active_editor.as_mut() {
            editor.insert_char(c);
        }
    }

    pub fn note_editor_split_line(&mut self) {
        if let Some(editor) = self.notes_popup.active_editor.as_mut() {
            editor.split_line();
        }
    }

    pub fn note_editor_backspace(&mut self) {
        if let Some(editor) = self.notes_popup.active_editor.as_mut() {
            editor.backspace();
        }
    }

    /// Save the editor's buffer to disk. Bound to `Ctrl+S` in either
    /// sub-mode (see `main.rs::handle_note_editor_normal`/`_insert`) —
    /// chosen because this codebase has no `:`-command-line anywhere to bind
    /// a `:w`-alternative to (see `odd/tasks/notes-popup.md`'s T6+T7 entry).
    /// Reports a flash message on failure; a no-op if the editor isn't open.
    pub fn save_note_editor(&mut self) {
        let Some(editor) = self.notes_popup.active_editor.as_mut() else {
            return;
        };
        if let Err(e) = editor.save() {
            self.flash(format!("note save failed: {e}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::{build_app_with_config, test_path};
    use crate::config::Config;

    // ---- load -------------------------------------------------------------

    #[test]
    fn load_splits_file_content_into_lines() {
        let path = test_path();
        std::fs::write(&path, "first\nsecond\nthird\n").expect("write");

        let editor = NoteEditorState::load(path.clone(), NoteEditorMode::Normal);

        assert_eq!(editor.lines(), &["first", "second", "third"]);
        assert_eq!(editor.cursor_line(), 0);
        assert_eq!(editor.cursor_col(), 0);
        assert!(!editor.dirty());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_missing_file_gives_single_empty_line_without_panicking() {
        let path = test_path(); // never written

        let editor = NoteEditorState::load(path, NoteEditorMode::Normal);

        assert_eq!(editor.lines(), &[String::new()]);
    }

    #[test]
    fn load_empty_file_gives_single_empty_line_without_panicking() {
        let path = test_path();
        std::fs::write(&path, "").expect("write");

        let editor = NoteEditorState::load(path.clone(), NoteEditorMode::Normal);

        assert_eq!(editor.lines(), &[String::new()]);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_starts_in_the_requested_mode() {
        let path = test_path();
        std::fs::write(&path, "hello\n").expect("write");

        let normal = NoteEditorState::load(path.clone(), NoteEditorMode::Normal);
        assert_eq!(normal.mode(), NoteEditorMode::Normal);
        let insert = NoteEditorState::load(path.clone(), NoteEditorMode::Insert);
        assert_eq!(insert.mode(), NoteEditorMode::Insert);

        let _ = std::fs::remove_file(&path);
    }

    // ---- save ---------------------------------------------------------------

    #[test]
    fn save_writes_lines_joined_with_newlines_to_disk() {
        let path = test_path();
        let mut editor = NoteEditorState::load(path.clone(), NoteEditorMode::Normal);
        editor.insert_char('a');
        editor.split_line();
        editor.insert_char('b');

        editor.save().expect("save");

        assert_eq!(std::fs::read_to_string(&path).expect("read back"), "a\nb\n");
        assert!(!editor.dirty());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_then_load_round_trips_the_same_lines() {
        let path = test_path();
        let mut editor = NoteEditorState::load(path.clone(), NoteEditorMode::Normal);
        for c in "hello".chars() {
            editor.insert_char(c);
        }
        editor.split_line();
        for c in "world".chars() {
            editor.insert_char(c);
        }
        editor.save().expect("save");

        let reloaded = NoteEditorState::load(path.clone(), NoteEditorMode::Normal);

        assert_eq!(reloaded.lines(), &["hello", "world"]);

        let _ = std::fs::remove_file(&path);
    }

    // ---- Insert-mode editing ------------------------------------------------

    #[test]
    fn insert_char_inserts_at_cursor_and_advances() {
        let mut editor = NoteEditorState::load(test_path(), NoteEditorMode::Insert);
        editor.insert_char('a');
        editor.insert_char('c');
        // Cursor is now after "ac"; move left once and insert between.
        editor.move_left();
        editor.insert_char('b');

        assert_eq!(editor.lines(), &["abc"]);
        assert_eq!(editor.cursor_col(), 2);
        assert!(editor.dirty());
    }

    #[test]
    fn enter_splits_the_current_line_in_two() {
        let mut editor = NoteEditorState::load(test_path(), NoteEditorMode::Insert);
        for c in "hello world".chars() {
            editor.insert_char(c);
        }
        // Cursor is at the end; walk it back to just after "hello".
        for _ in 0.."world".len() {
            editor.move_left();
        }
        editor.split_line();

        assert_eq!(editor.lines(), &["hello ", "world"]);
        assert_eq!(editor.cursor_line(), 1);
        assert_eq!(editor.cursor_col(), 0);
    }

    #[test]
    fn backspace_deletes_the_character_before_the_cursor() {
        let mut editor = NoteEditorState::load(test_path(), NoteEditorMode::Insert);
        for c in "abc".chars() {
            editor.insert_char(c);
        }
        editor.backspace();

        assert_eq!(editor.lines(), &["ab"]);
        assert_eq!(editor.cursor_col(), 2);
    }

    #[test]
    fn backspace_at_column_zero_joins_with_previous_line() {
        let mut editor = NoteEditorState::load(test_path(), NoteEditorMode::Insert);
        for c in "hello".chars() {
            editor.insert_char(c);
        }
        editor.split_line();
        for c in "world".chars() {
            editor.insert_char(c);
        }
        editor.move_left(); // walk back to column 0 of "world"'s line
        for _ in 0.."world".len() - 1 {
            editor.move_left();
        }
        assert_eq!(editor.cursor_col(), 0);

        editor.backspace();

        assert_eq!(editor.lines(), &["helloworld"]);
        assert_eq!(editor.cursor_line(), 0);
        assert_eq!(editor.cursor_col(), 5, "cursor lands at the join point");
    }

    #[test]
    fn backspace_at_very_start_of_buffer_is_noop_without_panicking() {
        let mut editor = NoteEditorState::load(test_path(), NoteEditorMode::Normal);

        editor.backspace();

        assert_eq!(editor.lines(), &[String::new()]);
        assert_eq!(editor.cursor_line(), 0);
        assert_eq!(editor.cursor_col(), 0);
    }

    // ---- Normal-mode motions --------------------------------------------

    #[test]
    fn move_up_and_down_clamp_at_buffer_bounds() {
        let path = test_path();
        std::fs::write(&path, "a\nb\nc\n").expect("write");
        let mut editor = NoteEditorState::load(path.clone(), NoteEditorMode::Normal);

        editor.move_up();
        assert_eq!(editor.cursor_line(), 0, "must not go negative");

        editor.move_down();
        editor.move_down();
        assert_eq!(editor.cursor_line(), 2);
        editor.move_down();
        assert_eq!(editor.cursor_line(), 2, "must clamp at the last line");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn move_left_and_right_clamp_at_line_bounds() {
        let path = test_path();
        std::fs::write(&path, "ab\n").expect("write");
        let mut editor = NoteEditorState::load(path.clone(), NoteEditorMode::Normal);

        editor.move_left();
        assert_eq!(editor.cursor_col(), 0, "must not go negative");

        editor.move_right();
        editor.move_right();
        assert_eq!(editor.cursor_col(), 2);
        editor.move_right();
        assert_eq!(editor.cursor_col(), 2, "must clamp at the line's length");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn moving_to_a_shorter_line_clamps_the_column() {
        let path = test_path();
        std::fs::write(&path, "hello\nhi\n").expect("write");
        let mut editor = NoteEditorState::load(path.clone(), NoteEditorMode::Normal);
        editor.move_right();
        editor.move_right();
        editor.move_right();
        editor.move_right();
        assert_eq!(editor.cursor_col(), 4);

        editor.move_down();

        assert_eq!(editor.cursor_line(), 1);
        assert_eq!(editor.cursor_col(), 2, "clamped to \"hi\"'s length");

        let _ = std::fs::remove_file(&path);
    }

    // ---- Normal/Insert toggle ---------------------------------------------

    #[test]
    fn enter_insert_switches_mode_and_esc_returns_to_normal() {
        let mut editor = NoteEditorState::load(test_path(), NoteEditorMode::Normal);

        editor.enter_insert();
        assert_eq!(editor.mode(), NoteEditorMode::Insert);

        editor.esc_to_normal();
        assert_eq!(
            editor.mode(),
            NoteEditorMode::Normal,
            "Esc from Insert returns to Normal, not out of the editor"
        );
    }

    // ---- UTF-8 safety -------------------------------------------------------

    #[test]
    fn accented_text_can_be_inserted_moved_through_and_saved_without_panicking() {
        let path = test_path();
        let mut editor = NoteEditorState::load(path.clone(), NoteEditorMode::Insert);
        for c in "café con leche y años".chars() {
            editor.insert_char(c);
        }
        // Walk the cursor all the way back through the accented characters.
        for _ in 0.."café con leche y años".chars().count() {
            editor.move_left();
        }
        assert_eq!(editor.cursor_col(), 0);
        for _ in 0.."café con leche y años".chars().count() {
            editor.move_right();
        }
        editor.insert_char('ñ');
        editor.backspace();

        editor
            .save()
            .expect("save must not panic on multibyte content");
        let saved = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(saved, "café con leche y años\n");

        let reloaded = NoteEditorState::load(path.clone(), NoteEditorMode::Normal);
        assert_eq!(reloaded.lines(), &["café con leche y años"]);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn split_and_backspace_join_are_utf8_safe_around_multibyte_chars() {
        let mut editor = NoteEditorState::load(test_path(), NoteEditorMode::Insert);
        for c in "años".chars() {
            editor.insert_char(c);
        }
        editor.move_left();
        editor.move_left(); // cursor between 'ñ' and 'o', 2 chars from the end
        editor.split_line();
        assert_eq!(editor.lines(), &["añ", "os"]);

        editor.backspace(); // join back together at column 0 of "os"

        assert_eq!(editor.lines(), &["años"]);
        assert_eq!(editor.cursor_col(), 2);
    }

    // ---- App-level wiring (open/close/save) --------------------------------

    fn app_with_one_note(dir: &std::path::Path) -> App {
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
        app
    }

    #[test]
    fn open_note_editor_normal_loads_selected_file_in_normal_mode() {
        let dir = test_path().with_extension("notes");
        let mut app = app_with_one_note(&dir);

        app.open_note_editor_normal();

        let editor = app.notes_popup.active_editor.as_ref().expect("editor open");
        assert_eq!(editor.mode(), NoteEditorMode::Normal);
        assert_eq!(editor.lines(), &["content a"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_note_editor_insert_loads_selected_file_in_insert_mode() {
        let dir = test_path().with_extension("notes");
        let mut app = app_with_one_note(&dir);

        app.open_note_editor_insert();

        let editor = app.notes_popup.active_editor.as_ref().expect("editor open");
        assert_eq!(editor.mode(), NoteEditorMode::Insert);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_note_editor_on_empty_list_is_noop() {
        let dir = test_path().with_extension("notes");
        let cfg = Config {
            notes_dir: Some(dir.to_string_lossy().into_owned()),
            ..Config::default()
        };
        let mut app = build_app_with_config("Write PR summary +work\n", cfg);
        app.open_notes_for_current();

        app.open_note_editor_normal();

        assert!(app.notes_popup.active_editor.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn close_note_editor_clears_active_editor() {
        let dir = test_path().with_extension("notes");
        let mut app = app_with_one_note(&dir);
        app.open_note_editor_normal();

        app.close_note_editor();

        assert!(app.notes_popup.active_editor.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- T13: `:`-command prompt -------------------------------------------

    #[test]
    fn command_prompt_starts_closed_and_open_gives_an_empty_buffer() {
        let editor = NoteEditorState::load(test_path(), NoteEditorMode::Normal);
        assert_eq!(editor.command_prompt(), None);

        let mut editor = editor;
        editor.open_command_prompt();
        assert_eq!(editor.command_prompt(), Some(""));
    }

    #[test]
    fn command_prompt_push_and_backspace_edit_the_buffer() {
        let mut editor = NoteEditorState::load(test_path(), NoteEditorMode::Normal);
        editor.open_command_prompt();

        editor.command_prompt_push('w');
        editor.command_prompt_push('q');
        assert_eq!(editor.command_prompt(), Some("wq"));

        editor.command_prompt_backspace();
        assert_eq!(editor.command_prompt(), Some("w"));
    }

    #[test]
    fn command_prompt_push_and_backspace_are_noops_when_prompt_is_closed() {
        let mut editor = NoteEditorState::load(test_path(), NoteEditorMode::Normal);

        editor.command_prompt_push('w');
        editor.command_prompt_backspace();

        assert_eq!(editor.command_prompt(), None);
    }

    #[test]
    fn cancel_command_prompt_discards_the_buffer_without_side_effects() {
        let path = test_path();
        std::fs::write(&path, "content\n").expect("write");
        let mut editor = NoteEditorState::load(path.clone(), NoteEditorMode::Normal);
        editor.open_command_prompt();
        editor.command_prompt_push('w');

        editor.cancel_command_prompt();

        assert_eq!(editor.command_prompt(), None);
        assert!(!editor.dirty());
        assert_eq!(
            std::fs::read_to_string(&path).expect("read back"),
            "content\n",
            "no save happened"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn execute_command_prompt_w_saves_and_returns_ok() {
        let path = test_path();
        let mut editor = NoteEditorState::load(path.clone(), NoteEditorMode::Insert);
        for c in "hello".chars() {
            editor.insert_char(c);
        }
        editor.esc_to_normal();
        editor.open_command_prompt();
        editor.command_prompt_push('w');

        let result = editor.execute_command_prompt();

        assert_eq!(result, NoteCommandResult::Ok);
        assert_eq!(editor.command_prompt(), None, "prompt closed either way");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read back"),
            "hello\n"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn execute_command_prompt_q_requests_close_without_saving() {
        let path = test_path();
        let mut editor = NoteEditorState::load(path.clone(), NoteEditorMode::Insert);
        editor.insert_char('a');
        editor.esc_to_normal();
        editor.open_command_prompt();
        editor.command_prompt_push('q');

        let result = editor.execute_command_prompt();

        assert_eq!(result, NoteCommandResult::CloseRequested);
        assert!(!path.exists(), "q never saves");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn execute_command_prompt_wq_and_x_save_then_request_close() {
        for cmd in ["wq", "x"] {
            let path = test_path();
            let mut editor = NoteEditorState::load(path.clone(), NoteEditorMode::Insert);
            for c in "saved".chars() {
                editor.insert_char(c);
            }
            editor.esc_to_normal();
            editor.open_command_prompt();
            for c in cmd.chars() {
                editor.command_prompt_push(c);
            }

            let result = editor.execute_command_prompt();

            assert_eq!(result, NoteCommandResult::CloseRequested, "cmd = {cmd}");
            assert_eq!(
                std::fs::read_to_string(&path).expect("read back"),
                "saved\n",
                "cmd = {cmd}"
            );

            let _ = std::fs::remove_file(&path);
        }
    }

    #[test]
    fn execute_command_prompt_wq_save_failure_stays_open_with_flashed_error() {
        // A path that is itself a directory: `std::fs::write` fails on it,
        // forcing `save()` to return `Err` so the "stay open on a failed
        // :wq/:x" branch is genuinely exercised, not just asserted.
        let dir_as_path = test_path().with_extension("dir");
        std::fs::create_dir_all(&dir_as_path).expect("create dir");
        let mut editor = NoteEditorState::load(dir_as_path.clone(), NoteEditorMode::Normal);
        editor.open_command_prompt();
        for c in "wq".chars() {
            editor.command_prompt_push(c);
        }

        let result = editor.execute_command_prompt();

        match result {
            NoteCommandResult::Error(msg) => {
                assert!(msg.contains("note save failed"), "got: {msg}")
            }
            other => panic!("expected Error, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir_as_path);
    }

    #[test]
    fn execute_command_prompt_unknown_command_flashes_error_and_does_not_close() {
        let mut editor = NoteEditorState::load(test_path(), NoteEditorMode::Normal);
        editor.open_command_prompt();
        for c in "zz".chars() {
            editor.command_prompt_push(c);
        }

        let result = editor.execute_command_prompt();

        assert_eq!(
            result,
            NoteCommandResult::Error("unknown command: zz".to_string())
        );
    }

    #[test]
    fn execute_command_prompt_empty_command_flashes_a_sensible_error() {
        let mut editor = NoteEditorState::load(test_path(), NoteEditorMode::Normal);
        editor.open_command_prompt();

        let result = editor.execute_command_prompt();

        assert_eq!(result, NoteCommandResult::Error("no command".to_string()));
    }

    #[test]
    fn save_note_editor_persists_edits_to_disk() {
        let dir = test_path().with_extension("notes");
        let mut app = app_with_one_note(&dir);
        let notes_folder = app.notes_popup.folder.clone().expect("folder").dir;
        app.open_note_editor_insert();
        for _ in 0.."content a".len() {
            app.note_editor_move_right(); // walk to the end of the line
        }
        for c in " appended".chars() {
            app.note_editor_insert_char(c);
        }

        app.save_note_editor();

        assert_eq!(
            std::fs::read_to_string(notes_folder.join("a.md")).expect("read back"),
            "content a appended\n"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
