//! The vim-style Normal and Visual modes of the note editor: a small state
//! machine that turns keys into motions, operators and commands.
//!
//! Supported, deliberately a subset of vim — the parts that matter for
//! writing notes:
//!
//! - motions: `h j k l` / arrows, `w b e W B E`, `0 ^ $` / Home End,
//!   `gg G`, `{ }`, all with counts (`3w`, `5j`);
//! - operators `d c y > <` combined with any motion (`dw`, `c$`, `y}`,
//!   `>j`), doubled for whole lines (`dd`, `cc`, `yy`, `>>`, `<<`);
//! - commands: `i a I A o O`, `x X s S D C Y J r ~`, `p P`, `u` (redo is
//!   `Ctrl+R`, bound by the caller), Enter ticks a checkbox;
//! - Visual mode: `v` (charwise) and `V` (linewise), extended by any
//!   motion, then `d x y c s > <`, `o` to swap ends;
//! - `M` switches to the rendered markdown preview (`preview.rs`).
//!
//! The binary maps crossterm key events onto [`EditorKey`] and calls
//! [`NoteEditorState::normal_key`]; keeping crossterm out of here keeps this
//! module unit-testable with plain values.

use super::motions::Pos;
use super::{NoteEditorMode, NoteEditorState, Register, byte_offset};

/// A key as the editor sees it. `Char` carries the typed character
/// (already shifted — `'A'`, `'$'`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorKey {
    Char(char),
    Enter,
    Esc,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
}

/// What a Normal/Visual-mode key meant for the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormalOutcome {
    /// Fully handled inside the editor.
    Handled,
    /// Esc with nothing to cancel: the caller steps out one layer.
    Esc,
}

/// The current Visual selection, normalized (`start <= end`, both
/// inclusive). `linewise` selections cover whole lines regardless of
/// column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisualSelection {
    pub start: Pos,
    pub end: Pos,
    pub linewise: bool,
}

impl VisualSelection {
    /// Whether the character at `(line, col)` is inside the selection.
    pub fn contains(&self, line: usize, col: usize) -> bool {
        if line < self.start.0 || line > self.end.0 {
            return false;
        }
        if self.linewise {
            return true;
        }
        (line > self.start.0 || col >= self.start.1) && (line < self.end.0 || col <= self.end.1)
    }
}

/// A half-typed command, waiting for its next key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Pending {
    #[default]
    None,
    /// `g`, waiting for the second `g`.
    G,
    /// `r`, waiting for the replacement character.
    Replace,
    /// An operator waiting for its motion.
    Op(Operator),
    /// An operator followed by `g` (`dgg`).
    OpG(Operator),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Operator {
    Delete,
    Change,
    Yank,
    Indent,
    Outdent,
}

impl Operator {
    fn from_key(c: char) -> Option<Self> {
        Some(match c {
            'd' => Self::Delete,
            'c' => Self::Change,
            'y' => Self::Yank,
            '>' => Self::Indent,
            '<' => Self::Outdent,
            _ => return None,
        })
    }

    fn key(self) -> char {
        match self {
            Self::Delete => 'd',
            Self::Change => 'c',
            Self::Yank => 'y',
            Self::Indent => '>',
            Self::Outdent => '<',
        }
    }
}

/// How an operator treats the span between the cursor and a motion's
/// target — vim's exclusive / inclusive / linewise motion types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Exclusive,
    Inclusive,
    Linewise,
}

impl NoteEditorState {
    /// Handle one key in Normal or Visual mode.
    pub fn normal_key(&mut self, key: EditorKey) -> NormalOutcome {
        if self.mode == NoteEditorMode::Preview {
            return self.preview_key(key);
        }
        let outcome = self.dispatch(key);
        if self.mode != NoteEditorMode::Insert {
            self.clamp_normal_col();
        }
        outcome
    }

    /// The Visual selection, while in Visual mode.
    pub fn visual_selection(&self) -> Option<VisualSelection> {
        if !self.mode.is_visual() {
            return None;
        }
        let cursor = (self.cursor_line, self.cursor_col);
        let (start, end) = ordered(self.visual_anchor, cursor);
        Some(VisualSelection {
            start,
            end,
            linewise: self.mode == NoteEditorMode::VisualLine,
        })
    }

    /// The half-typed command, for the status bar (`d`, `3`, `g`…), or
    /// `None` when nothing is pending.
    pub fn pending_keys(&self) -> Option<String> {
        let count = self.count.map(|n| n.to_string()).unwrap_or_default();
        let pending = match self.pending {
            Pending::None => String::new(),
            Pending::G => "g".into(),
            Pending::Replace => "r".into(),
            Pending::Op(op) => op.key().to_string(),
            Pending::OpG(op) => format!("{}g", op.key()),
        };
        let keys = format!("{count}{pending}");
        (!keys.is_empty()).then_some(keys)
    }

    fn cursor(&self) -> Pos {
        (self.cursor_line, self.cursor_col)
    }

    fn set_cursor(&mut self, (line, col): Pos) {
        self.cursor_line = line.min(self.lines.len() - 1);
        self.cursor_col = col;
        self.clamp_col();
    }

    /// Normal mode never rests past a line's last character.
    fn clamp_normal_col(&mut self) {
        let len = self.current_line_len();
        self.cursor_col = self.cursor_col.min(len.saturating_sub(1));
    }

    fn reset_pending(&mut self) {
        self.pending = Pending::None;
        self.count = None;
    }

    fn dispatch(&mut self, key: EditorKey) -> NormalOutcome {
        if key == EditorKey::Esc {
            if self.mode.is_visual() {
                self.mode = NoteEditorMode::Normal;
            } else if self.pending == Pending::None && self.count.is_none() {
                return NormalOutcome::Esc;
            }
            self.reset_pending();
            return NormalOutcome::Handled;
        }

        if self.pending == Pending::Replace {
            if let EditorKey::Char(c) = key {
                self.replace_chars(c);
            }
            self.reset_pending();
            return NormalOutcome::Handled;
        }

        if let EditorKey::Char(c) = key
            && let Some(digit) = c.to_digit(10)
            && (digit != 0 || self.count.is_some())
        {
            let n = self.count.unwrap_or(0).saturating_mul(10) + digit as usize;
            self.count = Some(n.min(9999));
            return NormalOutcome::Handled;
        }

        let g_prefixed = matches!(self.pending, Pending::G | Pending::OpG(_));
        if key == EditorKey::Char('g') && !g_prefixed {
            self.pending = match self.pending {
                Pending::Op(op) => Pending::OpG(op),
                _ => Pending::G,
            };
            return NormalOutcome::Handled;
        }

        let operator = match self.pending {
            Pending::Op(op) | Pending::OpG(op) => Some(op),
            _ => None,
        };
        if let Some((target, kind)) = self.motion(key, g_prefixed, operator) {
            match operator {
                Some(op) => self.apply_operator(op, self.cursor(), target, kind),
                None => self.set_cursor(target),
            }
            self.reset_pending();
            return NormalOutcome::Handled;
        }
        if g_prefixed {
            self.reset_pending();
            return NormalOutcome::Handled;
        }

        if self.mode.is_visual() {
            self.visual_command(key);
            self.reset_pending();
            return NormalOutcome::Handled;
        }

        if let Some(op) = operator {
            // Doubled operator (`dd`, `yy`, `>>`…): whole lines, `count`
            // of them.
            if key == EditorKey::Char(op.key()) {
                let last = self.cursor_line + self.count.unwrap_or(1) - 1;
                let target = (last.min(self.lines.len() - 1), 0);
                self.apply_operator(op, self.cursor(), target, Kind::Linewise);
            }
            self.reset_pending();
            return NormalOutcome::Handled;
        }

        self.command(key);
        NormalOutcome::Handled
    }

    /// Resolve `key` as a motion from the cursor: its target and type.
    /// `operator` is the pending operator, if any (a couple of motions
    /// behave differently under one, as in vim).
    fn motion(
        &self,
        key: EditorKey,
        g_prefixed: bool,
        operator: Option<Operator>,
    ) -> Option<(Pos, Kind)> {
        use EditorKey::{Backspace, Char, Down, End, Home, Left, Right, Up};
        let count = self.count.unwrap_or(1);
        let (line, col) = self.cursor();
        let last_line = self.lines.len() - 1;
        let target = match key {
            Char('g') if g_prefixed => {
                let l = self.count.map_or(0, |n| n - 1).min(last_line);
                ((l, self.first_non_blank(l)), Kind::Linewise)
            }
            _ if g_prefixed => return None,
            Char('G') => {
                let l = self.count.map_or(last_line, |n| n - 1).min(last_line);
                ((l, self.first_non_blank(l)), Kind::Linewise)
            }
            Char('h') | Left | Backspace => ((line, col.saturating_sub(count)), Kind::Exclusive),
            Char('l') | Right | Char(' ') => (
                (line, (col + count).min(self.line_len(line))),
                Kind::Exclusive,
            ),
            Char('j') | Down => (((line + count).min(last_line), col), Kind::Linewise),
            Char('k') | Up => ((line.saturating_sub(count), col), Kind::Linewise),
            Char('0') | Home => ((line, 0), Kind::Exclusive),
            Char('^') => ((line, self.first_non_blank(line)), Kind::Exclusive),
            Char('$') | End => {
                let l = (line + count - 1).min(last_line);
                ((l, self.line_len(l).saturating_sub(1)), Kind::Inclusive)
            }
            Char(c @ ('w' | 'W')) => {
                let big = c == 'W';
                if operator == Some(Operator::Change) && !self.is_space((line, col), big) {
                    // `cw` changes to the end of the word, like `ce`, and
                    // on a word's last char changes just that char.
                    let mut pos = (line, col);
                    for i in 0..count {
                        if i > 0 || !self.at_word_end(pos, big) {
                            pos = self.word_end(pos, big);
                        }
                    }
                    return Some((pos, Kind::Inclusive));
                }
                let mut pos = (line, col);
                for _ in 0..count {
                    pos = self.word_forward(pos, big);
                }
                if operator.is_some() && pos.0 > line {
                    // `dw` on a line's last word stops at the line's end
                    // rather than eating the next line's indentation.
                    let l = pos.0 - 1;
                    pos = (l, self.line_len(l));
                }
                (pos, Kind::Exclusive)
            }
            Char(c @ ('e' | 'E')) => {
                let mut pos = (line, col);
                for _ in 0..count {
                    pos = self.word_end(pos, c == 'E');
                }
                (pos, Kind::Inclusive)
            }
            Char(c @ ('b' | 'B')) => {
                let mut pos = (line, col);
                for _ in 0..count {
                    pos = self.word_backward(pos, c == 'B');
                }
                (pos, Kind::Exclusive)
            }
            Char('}') => {
                let mut l = line;
                for _ in 0..count {
                    l = self.paragraph_forward(l);
                }
                ((l, 0), Kind::Exclusive)
            }
            Char('{') => {
                let mut l = line;
                for _ in 0..count {
                    l = self.paragraph_backward(l);
                }
                ((l, 0), Kind::Exclusive)
            }
            _ => return None,
        };
        Some(target)
    }

    fn at_word_end(&self, (line, col): Pos, big: bool) -> bool {
        let chars: Vec<char> = self.lines[line].chars().collect();
        match (chars.get(col), chars.get(col + 1)) {
            (Some(_), None) => true,
            (Some(&a), Some(&b)) => {
                b.is_whitespace()
                    || (!big
                        && (a.is_alphanumeric() || a == '_') != (b.is_alphanumeric() || b == '_'))
            }
            _ => true,
        }
    }

    /// Apply `op` to the span from `from` to `to` (in either order).
    fn apply_operator(&mut self, op: Operator, from: Pos, to: Pos, kind: Kind) {
        let (start, end) = ordered(from, to);
        if kind == Kind::Linewise || matches!(op, Operator::Indent | Operator::Outdent) {
            self.apply_linewise(op, start.0, end.0);
            return;
        }
        let end = match kind {
            Kind::Inclusive => (end.0, (end.1 + 1).min(self.line_len(end.0))),
            _ => end,
        };
        let text = self.text_between(start, end);
        match op {
            Operator::Yank => {
                self.set_register(text, false);
                self.set_cursor(start);
            }
            Operator::Delete | Operator::Change => {
                if start == end {
                    if op == Operator::Change {
                        self.start_insert(false);
                    }
                    return;
                }
                self.set_register(text, false);
                self.begin_edit();
                self.delete_between(start, end);
                if op == Operator::Change {
                    self.start_insert(true);
                }
            }
            Operator::Indent | Operator::Outdent => unreachable!("handled linewise above"),
        }
    }

    fn apply_linewise(&mut self, op: Operator, first: usize, last: usize) {
        let text = self.lines[first..=last].join("\n");
        match op {
            Operator::Yank => {
                self.set_register(text, true);
                self.cursor_line = first;
            }
            Operator::Delete => {
                self.set_register(text, true);
                self.begin_edit();
                self.lines.drain(first..=last);
                if self.lines.is_empty() {
                    self.lines.push(String::new());
                }
                self.cursor_line = first.min(self.lines.len() - 1);
                self.cursor_col = self.first_non_blank(self.cursor_line);
                self.dirty = true;
            }
            Operator::Change => {
                self.set_register(text, true);
                self.begin_edit();
                let indent: String = self.lines[first]
                    .chars()
                    .take_while(|c| c.is_whitespace())
                    .collect();
                self.lines.splice(first..=last, [indent.clone()]);
                self.cursor_line = first;
                self.cursor_col = indent.chars().count();
                self.dirty = true;
                self.start_insert(true);
            }
            Operator::Indent | Operator::Outdent => {
                self.begin_edit();
                self.grouped(|editor| {
                    for l in first..=last {
                        if op == Operator::Indent {
                            if !editor.lines[l].is_empty() {
                                editor.indent_line(l);
                            }
                        } else {
                            editor.outdent_line(l);
                        }
                    }
                });
                self.cursor_line = first;
                self.cursor_col = self.first_non_blank(first);
            }
        }
    }

    /// Run `f` as part of the undo step the caller already opened with
    /// `begin_edit`, so the primitives `f` calls don't each add their own.
    fn grouped(&mut self, f: impl FnOnce(&mut Self)) {
        let was = self.edit_group;
        self.edit_group = true;
        f(self);
        self.edit_group = was;
    }

    /// Enter Insert mode. `already_checkpointed`: the command that led here
    /// already pushed the undo snapshot this Insert session should share
    /// (`cw`, `o`…), so typing undoes together with it.
    fn start_insert(&mut self, already_checkpointed: bool) {
        self.mode = NoteEditorMode::Insert;
        self.insert_checkpointed = already_checkpointed;
    }

    fn set_register(&mut self, text: String, linewise: bool) {
        self.clipboard_out = Some(if linewise {
            format!("{text}\n")
        } else {
            text.clone()
        });
        self.register = Some(Register { text, linewise });
    }

    /// Text from `start` up to (not including) `end`; lines joined by `\n`.
    fn text_between(&self, start: Pos, end: Pos) -> String {
        let slice = |line: usize, from: usize, to: Option<usize>| -> String {
            let chars = self.lines[line].chars().skip(from);
            match to {
                Some(to) => chars.take(to.saturating_sub(from)).collect(),
                None => chars.collect(),
            }
        };
        if start.0 == end.0 {
            return slice(start.0, start.1, Some(end.1));
        }
        let mut out = slice(start.0, start.1, None);
        for l in start.0 + 1..end.0 {
            out.push('\n');
            out.push_str(&self.lines[l]);
        }
        out.push('\n');
        out.push_str(&slice(end.0, 0, Some(end.1)));
        out
    }

    /// Remove the text from `start` up to (not including) `end`, leaving the
    /// cursor at `start`. The caller opens the undo step.
    fn delete_between(&mut self, start: Pos, end: Pos) {
        let head_end = byte_offset(&self.lines[start.0], start.1);
        let tail_start = byte_offset(&self.lines[end.0], end.1);
        let joined = format!(
            "{}{}",
            &self.lines[start.0][..head_end],
            &self.lines[end.0][tail_start..]
        );
        self.lines.splice(start.0..=end.0, [joined]);
        self.cursor_line = start.0;
        self.cursor_col = start.1;
        self.dirty = true;
    }

    /// `r<c>`: replace `count` characters from the cursor with `c`.
    fn replace_chars(&mut self, c: char) {
        let count = self.count.unwrap_or(1);
        let (line, col) = self.cursor();
        if col + count > self.line_len(line) {
            return;
        }
        self.begin_edit();
        let start = byte_offset(&self.lines[line], col);
        let end = byte_offset(&self.lines[line], col + count);
        let replacement: String = std::iter::repeat_n(c, count).collect();
        self.lines[line].replace_range(start..end, &replacement);
        self.cursor_col = col + count - 1;
        self.dirty = true;
    }

    /// `p` / `P`: put the register after / before the cursor, `count` times.
    fn put(&mut self, after: bool) {
        let Some(reg) = self.register.clone() else {
            return;
        };
        let count = self.count.unwrap_or(1);
        self.begin_edit();
        if reg.linewise {
            let block: Vec<String> = std::iter::repeat_n(reg.text.split('\n'), count)
                .flatten()
                .map(str::to_string)
                .collect();
            let at = if after {
                self.cursor_line + 1
            } else {
                self.cursor_line
            };
            self.lines.splice(at..at, block);
            self.cursor_line = at;
            self.cursor_col = self.first_non_blank(at);
            self.dirty = true;
            return;
        }
        if after && self.current_line_len() > 0 {
            self.cursor_col += 1;
        }
        let text = reg.text.repeat(count);
        self.grouped(|editor| editor.insert_text(&text));
        // vim leaves the cursor on the last put character.
        self.cursor_col = self.cursor_col.saturating_sub(1);
    }

    /// `J`: join `count` (at least 2) lines, separated by single spaces.
    fn join_lines(&mut self) {
        let joins = self.count.unwrap_or(2).max(2) - 1;
        let line = self.cursor_line;
        let joins = joins.min(self.lines.len() - 1 - line);
        if joins == 0 {
            return;
        }
        self.begin_edit();
        for _ in 0..joins {
            let next = self.lines.remove(line + 1);
            let next = next.trim_start();
            let current = self.lines[line].trim_end().to_string();
            self.cursor_col = current.chars().count();
            self.lines[line] = if next.is_empty() {
                current
            } else if current.is_empty() {
                next.to_string()
            } else {
                format!("{current} {next}")
            };
        }
        self.dirty = true;
    }

    /// `~`: flip the case of `count` characters and move past them.
    fn toggle_case(&mut self) {
        let count = self.count.unwrap_or(1);
        let (line, col) = self.cursor();
        let len = self.line_len(line);
        if col >= len {
            return;
        }
        let end = (col + count).min(len);
        self.begin_edit();
        let flipped: String = self.lines[line]
            .chars()
            .skip(col)
            .take(end - col)
            .flat_map(|c| {
                if c.is_uppercase() {
                    c.to_lowercase().collect::<Vec<_>>()
                } else {
                    c.to_uppercase().collect::<Vec<_>>()
                }
            })
            .collect();
        let b0 = byte_offset(&self.lines[line], col);
        let b1 = byte_offset(&self.lines[line], end);
        self.lines[line].replace_range(b0..b1, &flipped);
        self.cursor_col = end;
        self.dirty = true;
    }

    /// Non-motion, non-operator Normal-mode commands.
    fn command(&mut self, key: EditorKey) {
        use EditorKey::{Char, Delete, Enter};
        let count = self.count.unwrap_or(1);
        let (line, col) = self.cursor();
        let len = self.line_len(line);
        match key {
            Char('i') => self.start_insert(false),
            Char('a') => {
                self.cursor_col = (col + 1).min(len);
                self.start_insert(false);
            }
            Char('I') => {
                self.cursor_col = self.first_non_blank(line);
                self.start_insert(false);
            }
            Char('A') => {
                self.cursor_col = len;
                self.start_insert(false);
            }
            Char('o') => {
                self.begin_edit();
                let prefix = super::lists::continuation_of(&self.lines[line]);
                self.cursor_col = prefix.chars().count();
                self.cursor_line = line + 1;
                self.lines.insert(line + 1, prefix);
                self.dirty = true;
                self.start_insert(true);
            }
            Char('O') => {
                self.begin_edit();
                let indent: String = self.lines[line]
                    .chars()
                    .take_while(|c| c.is_whitespace())
                    .collect();
                self.cursor_col = indent.chars().count();
                self.lines.insert(line, indent);
                self.dirty = true;
                self.start_insert(true);
            }
            Char('x') | Delete if len > 0 => {
                self.apply_operator(
                    Operator::Delete,
                    (line, col),
                    (line, (col + count).min(len)),
                    Kind::Exclusive,
                );
            }
            Char('X') if col > 0 => {
                self.apply_operator(
                    Operator::Delete,
                    (line, col.saturating_sub(count)),
                    (line, col),
                    Kind::Exclusive,
                );
            }
            Char('s') => {
                self.apply_operator(
                    Operator::Change,
                    (line, col),
                    (line, (col + count).min(len)),
                    Kind::Exclusive,
                );
            }
            Char('S') => self.apply_linewise(
                Operator::Change,
                line,
                (line + count - 1).min(self.lines.len() - 1),
            ),
            Char('D') | Char('C') => {
                let op = if key == Char('D') {
                    Operator::Delete
                } else {
                    Operator::Change
                };
                let l = (line + count - 1).min(self.lines.len() - 1);
                self.apply_operator(op, (line, col), (l, self.line_len(l)), Kind::Exclusive);
            }
            Char('Y') => self.apply_linewise(
                Operator::Yank,
                line,
                (line + count - 1).min(self.lines.len() - 1),
            ),
            Char('p') => self.put(true),
            Char('P') => self.put(false),
            Char('u') => {
                for _ in 0..count {
                    if !self.undo() {
                        break;
                    }
                }
            }
            Char('J') => self.join_lines(),
            Char('~') => self.toggle_case(),
            Char('r') => {
                self.pending = Pending::Replace;
                return;
            }
            Char('v') => {
                self.visual_anchor = (line, col);
                self.mode = NoteEditorMode::Visual;
            }
            Char('V') => {
                self.visual_anchor = (line, col);
                self.mode = NoteEditorMode::VisualLine;
            }
            Char(':') => self.open_command_prompt(),
            Char('M') => self.enter_preview(),
            Enter => {
                if !self.toggle_checkbox() && line + 1 < self.lines.len() {
                    self.cursor_line = line + 1;
                    self.cursor_col = self.first_non_blank(line + 1);
                }
            }
            Char(c) => {
                if let Some(op) = Operator::from_key(c) {
                    self.pending = Pending::Op(op);
                    return;
                }
            }
            _ => {}
        }
        self.reset_pending();
    }

    /// Commands on the Visual selection.
    fn visual_command(&mut self, key: EditorKey) {
        use EditorKey::{Char, Delete};
        let Some(sel) = self.visual_selection() else {
            return;
        };
        let kind = if sel.linewise {
            Kind::Linewise
        } else {
            Kind::Inclusive
        };
        let op = match key {
            Char('d' | 'x') | Delete => Operator::Delete,
            Char('y') => Operator::Yank,
            Char('c' | 's') => Operator::Change,
            Char('>') => Operator::Indent,
            Char('<') => Operator::Outdent,
            Char('o') => {
                let cursor = self.cursor();
                self.set_cursor(self.visual_anchor);
                self.visual_anchor = cursor;
                return;
            }
            Char('v') | Char('V') => {
                let target = if key == Char('v') {
                    NoteEditorMode::Visual
                } else {
                    NoteEditorMode::VisualLine
                };
                self.mode = if self.mode == target {
                    NoteEditorMode::Normal
                } else {
                    target
                };
                return;
            }
            _ => return,
        };
        self.mode = NoteEditorMode::Normal;
        self.apply_operator(op, sel.start, sel.end, kind);
    }
}

fn ordered(a: Pos, b: Pos) -> (Pos, Pos) {
    if a <= b { (a, b) } else { (b, a) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::test_path;

    fn editor(lines: &[&str]) -> NoteEditorState {
        let path = test_path();
        std::fs::write(&path, lines.join("\n")).expect("write");
        NoteEditorState::load(path, NoteEditorMode::Normal)
    }

    /// Feed a vim key sequence; `<esc>` and `<cr>` spell Esc and Enter.
    fn keys(e: &mut NoteEditorState, seq: &str) {
        let mut rest = seq;
        while !rest.is_empty() {
            if let Some(r) = rest.strip_prefix("<esc>") {
                feed(e, EditorKey::Esc);
                rest = r;
            } else if let Some(r) = rest.strip_prefix("<cr>") {
                feed(e, EditorKey::Enter);
                rest = r;
            } else {
                let c = rest.chars().next().expect("non-empty");
                feed(e, EditorKey::Char(c));
                rest = &rest[c.len_utf8()..];
            }
        }
    }

    /// Route a key the way the binary does: Insert mode types, the rest go
    /// through `normal_key`.
    fn feed(e: &mut NoteEditorState, key: EditorKey) {
        if e.mode() == NoteEditorMode::Insert {
            match key {
                EditorKey::Esc => e.esc_to_normal(),
                EditorKey::Enter => e.newline(),
                EditorKey::Char(c) => e.insert_char(c),
                _ => {}
            }
        } else {
            e.normal_key(key);
        }
    }

    fn pos(e: &NoteEditorState) -> Pos {
        (e.cursor_line(), e.cursor_col())
    }

    #[test]
    fn line_motions_and_counts() {
        let mut e = editor(&["  hello world", "second", "third"]);
        keys(&mut e, "$");
        assert_eq!(pos(&e), (0, 12));
        keys(&mut e, "0");
        assert_eq!(pos(&e), (0, 0));
        keys(&mut e, "^");
        assert_eq!(pos(&e), (0, 2));
        keys(&mut e, "2j");
        assert_eq!(pos(&e), (2, 2));
        keys(&mut e, "gg");
        assert_eq!(pos(&e), (0, 2));
        keys(&mut e, "G");
        assert_eq!(pos(&e), (2, 0));
        keys(&mut e, "2G");
        assert_eq!(pos(&e), (1, 0));
        keys(&mut e, "3l");
        assert_eq!(pos(&e), (1, 3));
        keys(&mut e, "10l");
        assert_eq!(pos(&e), (1, 5), "clamped onto the last char");
    }

    #[test]
    fn word_motions_move_the_cursor() {
        let mut e = editor(&["one two three"]);
        keys(&mut e, "w");
        assert_eq!(pos(&e), (0, 4));
        keys(&mut e, "e");
        assert_eq!(pos(&e), (0, 6));
        keys(&mut e, "b");
        assert_eq!(pos(&e), (0, 4));
        keys(&mut e, "2w");
        assert_eq!(pos(&e), (0, 12), "clamps at the buffer's last char");
    }

    #[test]
    fn delete_operators() {
        let mut e = editor(&["one two three"]);
        keys(&mut e, "dw");
        assert_eq!(e.lines(), &["two three"]);
        keys(&mut e, "de");
        assert_eq!(e.lines(), &[" three"]);
        keys(&mut e, "x");
        assert_eq!(e.lines(), &["three"]);
        keys(&mut e, "ld$");
        assert_eq!(e.lines(), &["t"]);
    }

    #[test]
    fn dw_on_the_last_word_keeps_the_next_line() {
        let mut e = editor(&["one two", "  next"]);
        keys(&mut e, "wdw");
        assert_eq!(e.lines(), &["one ", "  next"]);
    }

    #[test]
    fn dd_counts_and_paste_linewise() {
        let mut e = editor(&["a", "b", "c", "d"]);
        keys(&mut e, "2dd");
        assert_eq!(e.lines(), &["c", "d"]);
        keys(&mut e, "p");
        assert_eq!(e.lines(), &["c", "a", "b", "d"]);
        assert_eq!(pos(&e), (1, 0));
        keys(&mut e, "ggP");
        assert_eq!(e.lines(), &["a", "b", "c", "a", "b", "d"]);
        assert_eq!(e.take_clipboard_out().as_deref(), Some("a\nb\n"));
    }

    #[test]
    fn dd_on_the_only_line_leaves_one_empty_line() {
        let mut e = editor(&["only"]);
        keys(&mut e, "dd");
        assert_eq!(e.lines(), &[""]);
    }

    #[test]
    fn yank_and_put_charwise() {
        let mut e = editor(&["hello world"]);
        keys(&mut e, "yw$p");
        assert_eq!(e.lines(), &["hello worldhello "]);
        assert_eq!(pos(&e), (0, 16));
        keys(&mut e, "0P");
        assert_eq!(e.lines(), &["hello hello worldhello "]);
    }

    #[test]
    fn change_operators_enter_insert_and_undo_as_one_step() {
        let mut e = editor(&["one two"]);
        keys(&mut e, "cwuno<esc>");
        assert_eq!(e.lines(), &["uno two"]);
        assert_eq!(e.mode(), NoteEditorMode::Normal);
        keys(&mut e, "u");
        assert_eq!(e.lines(), &["one two"], "cw + typing undo together");

        keys(&mut e, "wC2<esc>");
        assert_eq!(e.lines(), &["one 2"]);
        keys(&mut e, "ccnew<esc>");
        assert_eq!(e.lines(), &["new"]);
    }

    #[test]
    fn cw_on_a_single_char_word_changes_only_that_char() {
        let mut e = editor(&["a b"]);
        keys(&mut e, "cwX<esc>");
        assert_eq!(e.lines(), &["X b"]);
    }

    #[test]
    fn insert_entry_commands() {
        let mut e = editor(&["  mid"]);
        keys(&mut e, "Ia<esc>");
        assert_eq!(e.lines(), &["  amid"]);
        keys(&mut e, "A!<esc>");
        assert_eq!(e.lines(), &["  amid!"]);
        keys(&mut e, "onext<esc>");
        assert_eq!(e.lines(), &["  amid!", "  next"], "o keeps the indent");
        keys(&mut e, "Oprev<esc>");
        assert_eq!(e.lines(), &["  amid!", "  prev", "  next"]);
    }

    #[test]
    fn o_continues_a_list_item() {
        let mut e = editor(&["- [x] done"]);
        keys(&mut e, "onew<esc>");
        assert_eq!(e.lines(), &["- [x] done", "- [ ] new"]);

        // On an empty item, `o` still opens a new item below it rather
        // than ending the list the way Enter would.
        let mut e = editor(&["1. ", "tail"]);
        keys(&mut e, "ox<esc>");
        assert_eq!(e.lines(), &["1. ", "2. x", "tail"]);
    }

    #[test]
    fn replace_join_and_toggle_case() {
        let mut e = editor(&["abc", "  def"]);
        keys(&mut e, "rX");
        assert_eq!(e.lines(), &["Xbc", "  def"]);
        keys(&mut e, "J");
        assert_eq!(e.lines(), &["Xbc def"]);
        assert_eq!(pos(&e), (0, 3));
        keys(&mut e, "0~~");
        assert_eq!(e.lines(), &["xBc def"]);
    }

    #[test]
    fn indent_operators() {
        let mut e = editor(&["- a", "- b"]);
        keys(&mut e, ">j");
        assert_eq!(e.lines(), &["  - a", "  - b"]);
        keys(&mut e, "j<<");
        assert_eq!(e.lines(), &["  - a", "- b"]);
        keys(&mut e, "u");
        assert_eq!(e.lines(), &["  - a", "  - b"]);
        keys(&mut e, "u");
        assert_eq!(e.lines(), &["- a", "- b"], ">j undoes as one step");
    }

    #[test]
    fn visual_charwise_and_linewise() {
        let mut e = editor(&["one two three", "x"]);
        keys(&mut e, "wvey");
        assert_eq!(e.mode(), NoteEditorMode::Normal);
        assert_eq!(e.take_clipboard_out().as_deref(), Some("two"));
        keys(&mut e, "vld");
        assert_eq!(e.lines(), &["one o three", "x"]);

        keys(&mut e, "Vjd");
        assert_eq!(e.lines(), &[""]);
    }

    #[test]
    fn visual_selection_reports_its_range() {
        let mut e = editor(&["abcdef"]);
        keys(&mut e, "4lvhh");
        let sel = e.visual_selection().expect("in visual");
        assert_eq!((sel.start, sel.end), ((0, 2), (0, 4)));
        assert!(sel.contains(0, 3));
        assert!(!sel.contains(0, 5));
        keys(&mut e, "o");
        assert_eq!(pos(&e), (0, 4), "o jumps to the other end");
        keys(&mut e, "<esc>");
        assert!(e.visual_selection().is_none());
    }

    #[test]
    fn esc_cancels_a_pending_command_before_leaving() {
        let mut e = editor(&["abc"]);
        e.normal_key(EditorKey::Char('d'));
        assert_eq!(e.pending_keys().as_deref(), Some("d"));
        assert!(!e.is_idle());
        assert_eq!(e.normal_key(EditorKey::Esc), NormalOutcome::Handled);
        assert!(e.is_idle());
        assert_eq!(e.normal_key(EditorKey::Esc), NormalOutcome::Esc);
    }

    #[test]
    fn enter_ticks_a_checkbox_or_moves_down() {
        let mut e = editor(&["- [ ] task", "plain", "  last"]);
        keys(&mut e, "<cr>");
        assert_eq!(e.lines()[0], "- [x] task");
        keys(&mut e, "<cr>");
        assert_eq!(e.lines()[0], "- [ ] task");
        keys(&mut e, "j<cr>");
        assert_eq!(pos(&e), (2, 2), "plain line: down to the first non-blank");
    }

    #[test]
    fn undo_redo_normal_commands() {
        let mut e = editor(&["abc"]);
        keys(&mut e, "xx");
        assert_eq!(e.lines(), &["c"]);
        keys(&mut e, "u");
        assert_eq!(e.lines(), &["bc"]);
        assert!(e.redo());
        assert_eq!(e.lines(), &["c"]);
        keys(&mut e, "2u");
        assert_eq!(e.lines(), &["abc"]);
    }

    #[test]
    fn operators_are_utf8_safe() {
        let mut e = editor(&["añadir café"]);
        keys(&mut e, "wdw");
        assert_eq!(e.lines(), &["añadir "]);
        keys(&mut e, "0lrN");
        assert_eq!(e.lines(), &["aNadir "]);
    }
}
