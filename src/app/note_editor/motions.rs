//! Cursor motions beyond plain h/j/k/l: vim's word motions (`w b e` and
//! their WORD variants `W B E`), line motions (`0 ^ $`), paragraph motions
//! (`{ }`), and buffer start/end. Each motion is a pure function from a
//! position to a new position, so the same code serves plain movement,
//! operators (`dw`, `c$`, `y}`) and Visual mode.

use super::NoteEditorState;

/// A buffer position: (line index, char column).
pub(super) type Pos = (usize, usize);

/// vim's character classes for word motions. `Eol` stands for the virtual
/// newline at the end of each line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Blank,
    Word,
    Punct,
    Eol,
}

fn class_of(c: char, big_word: bool) -> Class {
    if c.is_whitespace() {
        Class::Blank
    } else if big_word || c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

impl NoteEditorState {
    pub(super) fn line_len(&self, line: usize) -> usize {
        self.lines[line].chars().count()
    }

    fn char_at(&self, (line, col): Pos) -> Option<char> {
        self.lines[line].chars().nth(col)
    }

    fn class_at(&self, pos: Pos, big_word: bool) -> Class {
        self.char_at(pos)
            .map_or(Class::Eol, |c| class_of(c, big_word))
    }

    /// An empty line is a word of its own to vim's word motions: they stop
    /// on it instead of skipping it as blank space.
    fn is_empty_line(&self, (line, _): Pos) -> bool {
        self.lines[line].is_empty()
    }

    /// Step forward one position, through each line's virtual newline.
    fn next_pos(&self, (line, col): Pos) -> Option<Pos> {
        if col < self.line_len(line) {
            Some((line, col + 1))
        } else if line + 1 < self.lines.len() {
            Some((line + 1, 0))
        } else {
            None
        }
    }

    /// Step backward one position, through each line's virtual newline.
    fn prev_pos(&self, (line, col): Pos) -> Option<Pos> {
        if col > 0 {
            Some((line, col - 1))
        } else if line > 0 {
            Some((line - 1, self.line_len(line - 1)))
        } else {
            None
        }
    }

    pub(super) fn is_space(&self, pos: Pos, big_word: bool) -> bool {
        matches!(self.class_at(pos, big_word), Class::Blank | Class::Eol)
            && !self.is_empty_line(pos)
    }

    /// `w` / `W`: start of the next word.
    pub(super) fn word_forward(&self, from: Pos, big_word: bool) -> Pos {
        let mut pos = from;
        let start = self.class_at(pos, big_word);
        if !self.is_space(pos, big_word) {
            // Leave the current word (an empty line is a one-position word).
            loop {
                let Some(next) = self.next_pos(pos) else {
                    return self.last_char_pos(pos);
                };
                pos = next;
                if self.is_empty_line(from) || self.class_at(pos, big_word) != start || pos.1 == 0 {
                    break;
                }
            }
        }
        while self.is_space(pos, big_word) {
            let Some(next) = self.next_pos(pos) else {
                return self.last_char_pos(pos);
            };
            pos = next;
        }
        pos
    }

    /// `e` / `E`: end of the current word, or of the next one when already
    /// on a word's last character.
    pub(super) fn word_end(&self, from: Pos, big_word: bool) -> Pos {
        let Some(mut pos) = self.next_pos(from) else {
            return from;
        };
        while self.is_space(pos, big_word) || self.is_empty_line(pos) {
            let Some(next) = self.next_pos(pos) else {
                return self.last_char_pos(pos);
            };
            pos = next;
        }
        let class = self.class_at(pos, big_word);
        while let Some(next) = self.next_pos(pos) {
            if next.0 != pos.0 || self.class_at(next, big_word) != class {
                break;
            }
            pos = next;
        }
        pos
    }

    /// `b` / `B`: start of the current word, or of the previous one when
    /// already at a word's start.
    pub(super) fn word_backward(&self, from: Pos, big_word: bool) -> Pos {
        let Some(mut pos) = self.prev_pos(from) else {
            return from;
        };
        while self.is_space(pos, big_word) {
            let Some(prev) = self.prev_pos(pos) else {
                return pos;
            };
            pos = prev;
        }
        if self.is_empty_line(pos) {
            return pos;
        }
        let class = self.class_at(pos, big_word);
        while let Some(prev) = self.prev_pos(pos) {
            if prev.0 != pos.0 || self.class_at(prev, big_word) != class {
                break;
            }
            pos = prev;
        }
        pos
    }

    /// Where a motion that ran off the end of the buffer stops: just past
    /// the last line's last character, so `dw` on the final word deletes
    /// all of it. Plain cursor movement then clamps back onto the last
    /// character, as Normal mode never rests past a line's end.
    fn last_char_pos(&self, (line, _): Pos) -> Pos {
        (line, self.line_len(line))
    }

    /// `^`: first non-blank character of `line`.
    pub(super) fn first_non_blank(&self, line: usize) -> usize {
        self.lines[line]
            .chars()
            .take_while(|c| c.is_whitespace())
            .count()
            .min(self.line_len(line).saturating_sub(1))
    }

    /// `}`: the next blank line after the cursor's paragraph (or the last
    /// line).
    pub(super) fn paragraph_forward(&self, line: usize) -> usize {
        let blank = |l: usize| self.lines[l].trim().is_empty();
        let mut l = line;
        while l + 1 < self.lines.len() && blank(l) {
            l += 1;
        }
        while l + 1 < self.lines.len() && !blank(l) {
            l += 1;
        }
        l
    }

    /// `{`: the previous blank line before the cursor's paragraph (or the
    /// first line).
    pub(super) fn paragraph_backward(&self, line: usize) -> usize {
        let blank = |l: usize| self.lines[l].trim().is_empty();
        let mut l = line;
        while l > 0 && blank(l) {
            l -= 1;
        }
        while l > 0 && !blank(l) {
            l -= 1;
        }
        l
    }
}

#[cfg(test)]
mod tests {
    use crate::app::test_support::test_path;
    use crate::app::{NoteEditorMode, NoteEditorState};

    fn editor(lines: &[&str]) -> NoteEditorState {
        let path = test_path();
        std::fs::write(&path, lines.join("\n")).expect("write");
        NoteEditorState::load(path, NoteEditorMode::Normal)
    }

    #[test]
    fn w_stops_at_word_and_punctuation_boundaries() {
        let e = editor(&["foo.bar baz", "", "  next"]);
        assert_eq!(e.word_forward((0, 0), false), (0, 3), "foo -> .");
        assert_eq!(e.word_forward((0, 3), false), (0, 4), ". -> bar");
        assert_eq!(e.word_forward((0, 4), false), (0, 8), "bar -> baz");
        assert_eq!(
            e.word_forward((0, 8), false),
            (1, 0),
            "stops on the empty line"
        );
        assert_eq!(e.word_forward((1, 0), false), (2, 2), "skips indentation");
        assert_eq!(e.word_forward((2, 2), false), (2, 6), "stops at buffer end");
    }

    #[test]
    fn big_w_treats_punctuation_as_part_of_the_word() {
        let e = editor(&["foo.bar baz"]);
        assert_eq!(e.word_forward((0, 0), true), (0, 8));
        assert_eq!(e.word_end((0, 0), true), (0, 6));
        assert_eq!(e.word_backward((0, 10), true), (0, 8));
        assert_eq!(e.word_backward((0, 8), true), (0, 0));
    }

    #[test]
    fn e_goes_to_word_ends_across_lines() {
        let e = editor(&["one two", "three"]);
        assert_eq!(e.word_end((0, 0), false), (0, 2));
        assert_eq!(e.word_end((0, 2), false), (0, 6));
        assert_eq!(e.word_end((0, 6), false), (1, 4));
    }

    #[test]
    fn b_goes_back_to_word_starts_across_lines() {
        let e = editor(&["one two", "three"]);
        assert_eq!(e.word_backward((1, 2), false), (1, 0));
        assert_eq!(e.word_backward((1, 0), false), (0, 4));
        assert_eq!(e.word_backward((0, 4), false), (0, 0));
        assert_eq!(e.word_backward((0, 0), false), (0, 0));
    }

    #[test]
    fn motions_are_utf8_safe() {
        let e = editor(&["añadir café ñu"]);
        assert_eq!(e.word_forward((0, 0), false), (0, 7));
        assert_eq!(e.word_end((0, 7), false), (0, 10));
    }

    #[test]
    fn paragraph_motions_jump_between_blank_lines() {
        let e = editor(&["a", "b", "", "c", "d", "", "e"]);
        assert_eq!(e.paragraph_forward(0), 2);
        assert_eq!(e.paragraph_forward(2), 5);
        assert_eq!(e.paragraph_forward(5), 6);
        assert_eq!(e.paragraph_backward(6), 5);
        assert_eq!(e.paragraph_backward(4), 2);
        assert_eq!(e.paragraph_backward(1), 0);
    }

    #[test]
    fn first_non_blank_skips_indentation() {
        let e = editor(&["", "    - item"]);
        assert_eq!(e.first_non_blank(0), 0);
        assert_eq!(e.first_non_blank(1), 4);
    }
}
