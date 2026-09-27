//! Markdown list awareness for the note editor: Enter continues a list
//! item, Tab / Shift+Tab (and `>>` / `<<`) nest and un-nest lines, and Enter
//! in Normal mode ticks a `- [ ]` checkbox.
//!
//! Recognized items: `-`, `*` and `+` bullets and `1.` / `1)` numbered
//! items, each followed by a space and optionally by a `[ ]` / `[x]`
//! checkbox, at any indentation.

use super::{NoteEditorState, byte_offset};

/// One nesting level. Two spaces is what most markdown formatters emit for
/// bullet lists.
pub(super) const INDENT: &str = "  ";

/// The prefix of a list item line, split into its parts.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ListMarker {
    /// Leading whitespace.
    indent: String,
    /// The bullet or number with its punctuation, e.g. `-`, `12.` or `3)`.
    bullet: String,
    /// The checkbox's state character (`' '`, `'x'`…), if the item has one.
    checkbox: Option<char>,
    /// Length in chars of the whole prefix: indent + bullet + space
    /// (+ checkbox + space).
    prefix_len: usize,
}

impl ListMarker {
    /// The prefix a new item right after this one starts with: same indent,
    /// same bullet (numbers incremented), an unticked checkbox if this item
    /// has one.
    fn continuation(&self) -> String {
        let bullet = match self.bullet.strip_suffix(['.', ')']) {
            Some(num) => {
                let next = num.parse::<u64>().map_or(1, |n| n + 1);
                let punct = &self.bullet[num.len()..];
                format!("{next}{punct}")
            }
            None => self.bullet.clone(),
        };
        let checkbox = if self.checkbox.is_some() { "[ ] " } else { "" };
        format!("{}{bullet} {checkbox}", self.indent)
    }
}

fn parse_marker(line: &str) -> Option<ListMarker> {
    let chars: Vec<char> = line.chars().collect();
    let mut i = chars.iter().take_while(|c| c.is_whitespace()).count();
    let indent: String = chars[..i].iter().collect();
    let bullet_start = i;
    match chars.get(i) {
        Some('-' | '*' | '+') => i += 1,
        Some(c) if c.is_ascii_digit() => {
            while chars.get(i).is_some_and(char::is_ascii_digit) {
                i += 1;
            }
            if !matches!(chars.get(i), Some('.' | ')')) {
                return None;
            }
            i += 1;
        }
        _ => return None,
    }
    let bullet: String = chars[bullet_start..i].iter().collect();
    // A bullet must be followed by a space (or end the line, while it's
    // still being typed): `-foo` and `**bold**` are not list items.
    match chars.get(i) {
        Some(' ') => i += 1,
        None => {}
        Some(_) => return None,
    }
    let mut checkbox = None;
    if chars.get(i) == Some(&'[')
        && chars.get(i + 2) == Some(&']')
        && matches!(chars.get(i + 3), Some(' ') | None)
        && let Some(&state) = chars.get(i + 1)
    {
        checkbox = Some(state);
        i = (i + 4).min(chars.len());
    }
    Some(ListMarker {
        indent,
        bullet,
        checkbox,
        prefix_len: i,
    })
}

/// How far a soft-wrapped continuation of `line` should be indented so it
/// lines up under the item's text instead of under its bullet (vim's
/// `breakindent`): the width of the list marker prefix, or of the plain
/// leading indentation for any other line. Counts chars; list markers and
/// indentation are ASCII in practice.
pub fn wrap_indent(line: &str) -> usize {
    match parse_marker(line) {
        Some(marker) => marker.prefix_len,
        None => line.chars().take_while(|c| *c == ' ').count(),
    }
}

/// What a new line opened below `line` starts with (`o`): the next list
/// marker on a list item, the same indentation on any other line.
pub(super) fn continuation_of(line: &str) -> String {
    match parse_marker(line) {
        Some(marker) => marker.continuation(),
        None => line.chars().take_while(|c| c.is_whitespace()).collect(),
    }
}

impl NoteEditorState {
    /// Enter in Insert mode. On a list item, the new line starts with the
    /// next item's marker. On an item that is still empty (just its marker),
    /// Enter ends the list instead: a nested item moves out one level, a
    /// top-level one loses its marker — the Obsidian/Notion behavior.
    /// Anywhere else it's a plain line split.
    pub fn newline(&mut self) {
        let line = &self.lines[self.cursor_line];
        let Some(marker) = parse_marker(line) else {
            // Plain line: keep its indentation (vim's `autoindent`).
            let indent: String = line
                .chars()
                .take(self.cursor_col)
                .take_while(|c| c.is_whitespace())
                .collect();
            self.split_line();
            if !indent.is_empty() {
                let rest = std::mem::take(&mut self.lines[self.cursor_line]);
                self.lines[self.cursor_line] = format!("{indent}{}", rest.trim_start());
                self.cursor_col = indent.chars().count();
            }
            return;
        };
        if self.cursor_col < marker.prefix_len {
            self.split_line();
            return;
        }
        let item_is_empty = line
            .chars()
            .skip(marker.prefix_len)
            .all(char::is_whitespace);
        if item_is_empty {
            if marker.indent.is_empty() {
                self.begin_edit();
                self.lines[self.cursor_line].clear();
                self.cursor_col = 0;
                self.dirty = true;
            } else {
                self.outdent_line(self.cursor_line);
            }
            return;
        }
        self.split_line();
        let prefix = marker.continuation();
        let rest = std::mem::take(&mut self.lines[self.cursor_line]);
        self.lines[self.cursor_line] = format!("{prefix}{}", rest.trim_start());
        self.cursor_col = prefix.chars().count();
    }

    /// Tab in Insert mode: nest a list item one level deeper (wherever the
    /// cursor is on it); on any other line insert one indent at the cursor.
    pub fn insert_tab(&mut self) {
        if parse_marker(&self.lines[self.cursor_line]).is_some() {
            self.indent_line(self.cursor_line);
        } else {
            for c in INDENT.chars() {
                self.insert_char(c);
            }
        }
    }

    /// Add one indent level to the start of line `idx`, keeping the cursor
    /// on the same character.
    pub(super) fn indent_line(&mut self, idx: usize) {
        self.begin_edit();
        self.lines[idx].insert_str(0, INDENT);
        if idx == self.cursor_line {
            self.cursor_col += INDENT.chars().count();
        }
        self.dirty = true;
    }

    /// Remove up to one indent level from the start of line `idx` (a single
    /// leading tab counts as a whole level), keeping the cursor on the same
    /// character. A no-op on an unindented line.
    pub(super) fn outdent_line(&mut self, idx: usize) {
        let line = &self.lines[idx];
        let remove = if line.starts_with('\t') {
            1
        } else {
            line.chars()
                .take(INDENT.chars().count())
                .take_while(|&c| c == ' ')
                .count()
        };
        if remove == 0 {
            return;
        }
        self.begin_edit();
        let end = byte_offset(&self.lines[idx], remove);
        self.lines[idx].drain(..end);
        if idx == self.cursor_line {
            self.cursor_col = self.cursor_col.saturating_sub(remove);
        }
        self.dirty = true;
    }

    /// Shift+Tab: un-nest the cursor line by one level.
    pub fn outdent_current_line(&mut self) {
        self.outdent_line(self.cursor_line);
    }

    /// Flip the cursor line's `[ ]` checkbox to `[x]` and back (any ticked
    /// state — `x`, `X`, `-`… — counts as done and flips back to `[ ]`).
    /// Returns `false` when the line has no checkbox.
    pub fn toggle_checkbox(&mut self) -> bool {
        let Some(marker) = parse_marker(&self.lines[self.cursor_line]) else {
            return false;
        };
        let Some(state) = marker.checkbox else {
            return false;
        };
        let new_state = if state == ' ' { 'x' } else { ' ' };
        // The state char sits two chars before the end of `[?] `, or one
        // before the end when the line ends right after the `]`.
        let line = &self.lines[self.cursor_line];
        let bracket = line
            .chars()
            .take(marker.prefix_len)
            .collect::<Vec<_>>()
            .iter()
            .rposition(|&c| c == ']')
            .unwrap_or(0);
        let state_idx = bracket.saturating_sub(1);
        self.begin_edit();
        let line = &mut self.lines[self.cursor_line];
        let start = byte_offset(line, state_idx);
        let end = byte_offset(line, state_idx + 1);
        line.replace_range(start..end, &new_state.to_string());
        self.dirty = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::NoteEditorMode;
    use crate::app::test_support::test_path;

    fn editor_with(
        lines: &[&str],
        line: usize,
        col: usize,
        mode: NoteEditorMode,
    ) -> NoteEditorState {
        let path = test_path();
        std::fs::write(&path, lines.join("\n")).expect("write");
        let mut editor = NoteEditorState::load(path, mode);
        editor.cursor_line = line;
        editor.cursor_col = col;
        editor
    }

    fn type_str(editor: &mut NoteEditorState, s: &str) {
        for c in s.chars() {
            editor.insert_char(c);
        }
    }

    #[test]
    fn parses_bullets_numbers_and_checkboxes() {
        let m = parse_marker("  - [ ] buy milk").expect("marker");
        assert_eq!(m.indent, "  ");
        assert_eq!(m.bullet, "-");
        assert_eq!(m.checkbox, Some(' '));
        assert_eq!(m.prefix_len, 8);

        let m = parse_marker("12. step").expect("marker");
        assert_eq!(m.bullet, "12.");
        assert_eq!(m.checkbox, None);

        assert!(parse_marker("plain text").is_none());
        assert!(parse_marker("-no space").is_none());
        assert!(parse_marker("**bold**").is_none());
        assert!(parse_marker("3 apples").is_none());
    }

    #[test]
    fn enter_continues_bullets_checkboxes_and_numbers() {
        for (line, expected) in [
            ("- one", "- "),
            ("* one", "* "),
            ("  + one", "  + "),
            ("- [ ] one", "- [ ] "),
            ("- [x] one", "- [ ] "),
            ("1. one", "2. "),
            ("9) one", "10) "),
        ] {
            let len = line.chars().count();
            let mut editor = editor_with(&[line], 0, len, NoteEditorMode::Insert);
            editor.newline();
            assert_eq!(editor.lines()[1], expected, "after {line:?}");
            assert_eq!(editor.cursor_col(), expected.chars().count());
        }
    }

    #[test]
    fn enter_mid_item_carries_the_rest_onto_the_new_item() {
        let mut editor = editor_with(&["- buy milk"], 0, 6, NoteEditorMode::Insert);
        editor.newline();
        assert_eq!(editor.lines(), &["- buy ", "- milk"]);
        assert_eq!((editor.cursor_line(), editor.cursor_col()), (1, 2));
    }

    #[test]
    fn enter_on_an_empty_item_ends_or_outdents_the_list() {
        let mut editor = editor_with(&["- one", "- "], 1, 2, NoteEditorMode::Insert);
        editor.newline();
        assert_eq!(editor.lines(), &["- one", ""]);

        let mut editor = editor_with(&["- one", "  - [ ] "], 1, 8, NoteEditorMode::Insert);
        editor.newline();
        assert_eq!(editor.lines(), &["- one", "- [ ] "]);
        assert_eq!(editor.cursor_col(), 6);
    }

    #[test]
    fn enter_on_a_plain_line_or_before_the_marker_just_splits() {
        let mut editor = editor_with(&["hello"], 0, 5, NoteEditorMode::Insert);
        editor.newline();
        assert_eq!(editor.lines(), &["hello", ""]);

        let mut editor = editor_with(&["- one"], 0, 0, NoteEditorMode::Insert);
        editor.newline();
        assert_eq!(editor.lines(), &["", "- one"]);
    }

    #[test]
    fn enter_on_an_indented_plain_line_keeps_the_indent() {
        let mut editor = editor_with(&["  code"], 0, 6, NoteEditorMode::Insert);
        editor.newline();
        assert_eq!(editor.lines(), &["  code", "  "]);
        assert_eq!(editor.cursor_col(), 2);
    }

    #[test]
    fn typing_a_list_then_enter_twice_leaves_the_list() {
        let mut editor = editor_with(&[""], 0, 0, NoteEditorMode::Insert);
        type_str(&mut editor, "- [ ] a");
        editor.newline();
        type_str(&mut editor, "b");
        editor.newline();
        editor.newline();
        type_str(&mut editor, "done");
        assert_eq!(editor.lines(), &["- [ ] a", "- [ ] b", "done"]);
    }

    #[test]
    fn tab_nests_list_items_and_indents_plain_text() {
        let mut editor = editor_with(&["- one"], 0, 3, NoteEditorMode::Insert);
        editor.insert_tab();
        assert_eq!(editor.lines(), &["  - one"]);
        assert_eq!(editor.cursor_col(), 5, "cursor stays on the same char");
        editor.outdent_current_line();
        assert_eq!(editor.lines(), &["- one"]);
        assert_eq!(editor.cursor_col(), 3);
        editor.outdent_current_line();
        assert_eq!(editor.lines(), &["- one"], "no-op at the top level");

        let mut editor = editor_with(&["ab"], 0, 1, NoteEditorMode::Insert);
        editor.insert_tab();
        assert_eq!(editor.lines(), &["a  b"]);
    }

    #[test]
    fn toggle_checkbox_flips_between_open_and_done() {
        let mut editor = editor_with(&["  - [ ] task"], 0, 0, NoteEditorMode::Normal);
        assert!(editor.toggle_checkbox());
        assert_eq!(editor.lines(), &["  - [x] task"]);
        assert!(editor.toggle_checkbox());
        assert_eq!(editor.lines(), &["  - [ ] task"]);

        let mut editor = editor_with(&["- [X] shouting"], 0, 0, NoteEditorMode::Normal);
        editor.toggle_checkbox();
        assert_eq!(editor.lines(), &["- [ ] shouting"]);

        let mut editor = editor_with(&["- plain"], 0, 0, NoteEditorMode::Normal);
        assert!(!editor.toggle_checkbox());
        assert_eq!(editor.lines(), &["- plain"]);
    }
}
