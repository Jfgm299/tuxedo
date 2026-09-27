//! Preview mode of the note editor: the note shown rendered (see
//! `src/ui/markdown.rs`) instead of as source, scrolled with vim-ish keys.
//! Read-only — the buffer is untouched, and since it is re-rendered from the
//! buffer every frame, the preview is always current.
//!
//! The scroll position is in rendered rows, which only the renderer can
//! count (they depend on the width). Keys record the intent here; the
//! renderer clamps it and remembers the viewport size in the `Cell`s.

use std::cell::Cell;

use super::vim::{EditorKey, NormalOutcome, Pending};
use super::{NoteEditorMode, NoteEditorState};

/// Preview scroll state, shared between key handling (`&mut`) and the
/// renderer (`&`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PreviewScroll {
    /// First rendered row on screen.
    pub(crate) top: Cell<usize>,
    /// Buffer line to bring into view on the next render (set on entering
    /// the preview, so it opens where the cursor was).
    pub(crate) anchor: Cell<Option<usize>>,
    /// Viewport height at the last render, for page-sized scrolling.
    pub(crate) height: Cell<usize>,
}

impl NoteEditorState {
    /// `M` in Normal mode (or `p` from the notes list): show the note
    /// rendered, opened at the block the cursor is in.
    pub fn enter_preview(&mut self) {
        self.mode = NoteEditorMode::Preview;
        self.preview.anchor.set(Some(self.cursor_line));
        self.pending = Pending::None;
        self.count = None;
    }

    /// Scroll state for the renderer.
    pub fn preview_scroll(&self) -> (&Cell<usize>, &Cell<Option<usize>>, &Cell<usize>) {
        (
            &self.preview.top,
            &self.preview.anchor,
            &self.preview.height,
        )
    }

    /// Scroll the preview by `delta` rows (negative scrolls up). The
    /// renderer clamps the bottom.
    pub fn scroll_preview(&mut self, delta: isize) {
        let top = self.preview.top.get().saturating_add_signed(delta);
        self.preview.top.set(top);
    }

    /// `Ctrl+D` / `Ctrl+U`: half a screen down / up.
    pub fn scroll_preview_half_page(&mut self, down: bool) {
        let half = (self.preview.height.get() / 2).max(1) as isize;
        self.scroll_preview(if down { half } else { -half });
    }

    pub(super) fn preview_key(&mut self, key: EditorKey) -> NormalOutcome {
        use EditorKey::{Char, Down, End, Enter, Esc, Home, Up};
        let page = self.preview.height.get().saturating_sub(2).max(1) as isize;
        let g_pending = self.pending == Pending::G;
        self.pending = Pending::None;
        match key {
            Char('j') | Down | Enter => self.scroll_preview(1),
            Char('k') | Up => self.scroll_preview(-1),
            Char(' ') => self.scroll_preview(page),
            Char('b') => self.scroll_preview(-page),
            Char('g') if g_pending => self.preview.top.set(0),
            Char('g') => self.pending = Pending::G,
            Home => self.preview.top.set(0),
            Char('G') | End => self.preview.top.set(usize::MAX),
            Char('p' | 'M' | 'e') => self.mode = NoteEditorMode::Normal,
            Char('i') => self.enter_insert(),
            Char(':') => self.open_command_prompt(),
            Esc => return NormalOutcome::Esc,
            _ => {}
        }
        NormalOutcome::Handled
    }
}

#[cfg(test)]
mod tests {
    use crate::app::test_support::test_path;
    use crate::app::{EditorKey, NormalOutcome, NoteEditorMode, NoteEditorState};

    #[test]
    fn m_enters_preview_and_p_returns_to_normal() {
        let mut e = NoteEditorState::load(test_path(), NoteEditorMode::Normal);
        e.normal_key(EditorKey::Char('M'));
        assert_eq!(e.mode(), NoteEditorMode::Preview);
        assert!(e.is_idle(), "z / Tab still reach the pin handlers");

        // Editing keys do nothing to the buffer in the preview.
        e.normal_key(EditorKey::Char('x'));
        e.normal_key(EditorKey::Char('d'));
        e.normal_key(EditorKey::Char('d'));
        assert_eq!(e.lines(), &[""]);
        assert!(!e.dirty());

        e.normal_key(EditorKey::Char('p'));
        assert_eq!(e.mode(), NoteEditorMode::Normal);
        e.normal_key(EditorKey::Char('M'));
        e.normal_key(EditorKey::Char('i'));
        assert_eq!(e.mode(), NoteEditorMode::Insert);
    }

    #[test]
    fn preview_scroll_keys_and_esc() {
        let mut e = NoteEditorState::load(test_path(), NoteEditorMode::Preview);
        e.preview.height.set(10);
        e.normal_key(EditorKey::Char('j'));
        e.normal_key(EditorKey::Char('j'));
        assert_eq!(e.preview.top.get(), 2);
        e.normal_key(EditorKey::Char(' '));
        assert_eq!(e.preview.top.get(), 10);
        e.normal_key(EditorKey::Char('g'));
        e.normal_key(EditorKey::Char('g'));
        assert_eq!(e.preview.top.get(), 0);
        e.normal_key(EditorKey::Char('k'));
        assert_eq!(e.preview.top.get(), 0, "no scrolling above the top");
        assert_eq!(e.normal_key(EditorKey::Esc), NormalOutcome::Esc);
    }
}
