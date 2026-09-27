//! Undo/redo for the note editor: whole-buffer snapshots. Notes are small
//! markdown files, so cloning the `Vec<String>` per edit is cheap and far
//! simpler than an operation log.
//!
//! Granularity follows vim: every Normal-mode command is one step, and a
//! whole Insert session (from `i`/`a`/`o`… to Esc) is one step, however much
//! was typed in it.

use super::{NoteEditorMode, NoteEditorState};

/// Undo history depth. Oldest snapshots are dropped past this.
const MAX_UNDO: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Snapshot {
    lines: Vec<String>,
    cursor_line: usize,
    cursor_col: usize,
}

impl NoteEditorState {
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            lines: self.lines.clone(),
            cursor_line: self.cursor_line,
            cursor_col: self.cursor_col,
        }
    }

    fn restore(&mut self, snap: Snapshot) {
        self.lines = snap.lines;
        self.cursor_line = snap.cursor_line.min(self.lines.len() - 1);
        self.cursor_col = snap.cursor_col;
        self.clamp_col();
        self.dirty = true;
    }

    /// Record the buffer as it is right now, before an edit, and forget
    /// anything that could have been redone.
    pub(super) fn push_undo(&mut self) {
        self.undo_stack.push(self.snapshot());
        if self.undo_stack.len() > MAX_UNDO {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
    }

    /// Called by every primitive right before it changes the buffer. In
    /// Insert mode only the first change of the session snapshots, so the
    /// whole session undoes at once; anywhere else every call does.
    pub(super) fn begin_edit(&mut self) {
        if self.edit_group {
            return;
        }
        if self.mode == NoteEditorMode::Insert {
            if self.insert_checkpointed {
                return;
            }
            self.insert_checkpointed = true;
        }
        self.push_undo();
    }

    /// `u`: step back one change. Returns `false` when there is nothing to
    /// undo.
    pub fn undo(&mut self) -> bool {
        let Some(snap) = self.undo_stack.pop() else {
            return false;
        };
        self.redo_stack.push(self.snapshot());
        self.restore(snap);
        true
    }

    /// `Ctrl+R`: re-apply the last undone change. Returns `false` when there
    /// is nothing to redo.
    pub fn redo(&mut self) -> bool {
        let Some(snap) = self.redo_stack.pop() else {
            return false;
        };
        self.undo_stack.push(self.snapshot());
        self.restore(snap);
        true
    }
}

#[cfg(test)]
mod tests {
    use crate::app::test_support::test_path;
    use crate::app::{NoteEditorMode, NoteEditorState};

    #[test]
    fn a_whole_insert_session_undoes_as_one_step() {
        let mut editor = NoteEditorState::load(test_path(), NoteEditorMode::Insert);
        for c in "hello".chars() {
            editor.insert_char(c);
        }
        editor.split_line();
        editor.insert_char('x');
        editor.esc_to_normal();

        assert!(editor.undo());
        assert_eq!(editor.lines(), &[""]);
        assert!(!editor.undo(), "nothing older to undo");

        assert!(editor.redo());
        assert_eq!(editor.lines(), &["hello", "x"]);
        assert!(!editor.redo());
    }

    #[test]
    fn separate_insert_sessions_undo_separately() {
        let mut editor = NoteEditorState::load(test_path(), NoteEditorMode::Insert);
        editor.insert_char('a');
        editor.esc_to_normal();
        editor.enter_insert();
        editor.insert_char('b');
        editor.esc_to_normal();

        editor.undo();
        assert_eq!(editor.lines(), &["a"]);
        editor.undo();
        assert_eq!(editor.lines(), &[""]);
    }

    #[test]
    fn a_new_edit_clears_the_redo_stack() {
        let mut editor = NoteEditorState::load(test_path(), NoteEditorMode::Insert);
        editor.insert_char('a');
        editor.esc_to_normal();
        editor.undo();
        editor.enter_insert();
        editor.insert_char('b');
        editor.esc_to_normal();

        assert!(!editor.redo(), "the undone 'a' is gone once 'b' was typed");
        assert_eq!(editor.lines(), &["b"]);
    }
}
