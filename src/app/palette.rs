//! Command palette catalog and filter. Maps every `Action` variant to a
//! human-readable label and its current keybinding, then filters by fuzzy
//! subsequence match against the label. Reuses `search::subseq_match_ci` so
//! the matching semantics stay identical to the `/` task search.
use super::types::Mode;
use crate::action::Action;
use crate::search::subseq_match_ci;

/// What a `PaletteEntry` invokes when chosen. Most entries dispatch a global
/// `Action` through the same `apply_action` every keybinding already goes
/// through. `NotesAction` is the escape hatch for the notes-popup-internal
/// operations (create/rename/delete/unlink/open-editor) that were
/// deliberately never promoted to global `Action`s (see
/// `src/app/notes_popup.rs`/`src/app/note_editor.rs`) — there is no
/// command-line to bind them to outside the popup, so instead of going
/// through `Action`/`apply_action` they call the matching `App` method
/// directly (`app.begin_new_note_prompt()` and friends), exactly as
/// `main.rs::handle_notes` already does for the raw keystrokes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteDispatch {
    Global(Action),
    NotesAction(NotesEntryAction),
}

/// The notes-popup-internal operations reachable from the palette only while
/// the captured prior mode is `Mode::Notes` (see `filtered`/`entry_visible`
/// below). Each variant maps 1:1 to an existing `App` method that
/// `main.rs::handle_notes` already calls for its own keystroke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotesEntryAction {
    /// Mirrors `n`: `App::begin_new_note_prompt`.
    Create,
    /// Mirrors `r`: `App::begin_rename_prompt`.
    Rename,
    /// Mirrors `d`: `App::begin_delete_note_confirm` (opens the confirm
    /// sub-state, does not delete immediately).
    Delete,
    /// Mirrors `u`: `App::unlink_selected_note`.
    Unlink,
    /// Mirrors `e`: `App::open_note_editor_normal`.
    OpenEditorNormal,
    /// Mirrors `i`: `App::open_note_editor_insert`.
    OpenEditorInsert,
}

#[derive(Debug, Clone, Copy)]
pub struct PaletteEntry {
    pub label: &'static str,
    pub keys: &'static str,
    pub dispatch: PaletteDispatch,
}

/// Every action that's meaningful to invoke from the palette. `ArmF` is
/// omitted: it only exists as the leader of `fp` / `fc`, both of which appear
/// here under their full names. The trailing `NotesAction` entries (T14) are
/// notes-popup-internal — see `entry_visible` — shown only when the palette
/// was opened from `Mode::Notes`.
pub const ENTRIES: &[PaletteEntry] = &[
    PaletteEntry {
        label: "new task",
        keys: "n",
        dispatch: PaletteDispatch::Global(Action::BeginAdd),
    },
    PaletteEntry {
        label: "edit current task (normal mode)",
        keys: "e",
        dispatch: PaletteDispatch::Global(Action::BeginEdit),
    },
    PaletteEntry {
        label: "edit current task (insert mode)",
        keys: "i",
        dispatch: PaletteDispatch::Global(Action::BeginEditInsert),
    },
    PaletteEntry {
        label: "toggle complete",
        keys: "x",
        dispatch: PaletteDispatch::Global(Action::ToggleComplete),
    },
    PaletteEntry {
        label: "delete task",
        keys: "dd",
        dispatch: PaletteDispatch::Global(Action::Delete),
    },
    PaletteEntry {
        label: "cycle priority",
        keys: "p",
        dispatch: PaletteDispatch::Global(Action::CyclePriority),
    },
    PaletteEntry {
        label: "move task down",
        keys: "J",
        dispatch: PaletteDispatch::Global(Action::MoveTaskDown),
    },
    PaletteEntry {
        label: "move task up",
        keys: "K",
        dispatch: PaletteDispatch::Global(Action::MoveTaskUp),
    },
    PaletteEntry {
        label: "add project to current task",
        keys: "+",
        dispatch: PaletteDispatch::Global(Action::BeginPromptProject),
    },
    PaletteEntry {
        label: "add or remove context on current task",
        keys: "c",
        dispatch: PaletteDispatch::Global(Action::BeginPromptContext),
    },
    PaletteEntry {
        label: "copy line to clipboard",
        keys: "yy",
        dispatch: PaletteDispatch::Global(Action::CopyLine),
    },
    PaletteEntry {
        label: "copy body to clipboard",
        keys: "yb",
        dispatch: PaletteDispatch::Global(Action::CopyBody),
    },
    PaletteEntry {
        label: "open task notes",
        keys: "o",
        dispatch: PaletteDispatch::Global(Action::OpenNotes),
    },
    PaletteEntry {
        label: "undo",
        keys: "u",
        dispatch: PaletteDispatch::Global(Action::Undo),
    },
    PaletteEntry {
        label: "cursor down",
        keys: "j / ↓",
        dispatch: PaletteDispatch::Global(Action::CursorDown),
    },
    PaletteEntry {
        label: "cursor up",
        keys: "k / ↑",
        dispatch: PaletteDispatch::Global(Action::CursorUp),
    },
    PaletteEntry {
        label: "jump to first task",
        keys: "gg",
        dispatch: PaletteDispatch::Global(Action::CursorTop),
    },
    PaletteEntry {
        label: "jump to last task",
        keys: "G",
        dispatch: PaletteDispatch::Global(Action::CursorBottom),
    },
    PaletteEntry {
        label: "page down",
        keys: "Ctrl-d",
        dispatch: PaletteDispatch::Global(Action::HalfPageDown),
    },
    PaletteEntry {
        label: "page up",
        keys: "Ctrl-u",
        dispatch: PaletteDispatch::Global(Action::HalfPageUp),
    },
    PaletteEntry {
        label: "fuzzy search",
        keys: "/",
        dispatch: PaletteDispatch::Global(Action::BeginSearch),
    },
    PaletteEntry {
        label: "filter by project",
        keys: "fp",
        dispatch: PaletteDispatch::Global(Action::PickProject),
    },
    PaletteEntry {
        label: "filter by context",
        keys: "fc",
        dispatch: PaletteDispatch::Global(Action::PickContext),
    },
    PaletteEntry {
        label: "pick saved filter",
        keys: "ff",
        dispatch: PaletteDispatch::Global(Action::PickSavedFilter),
    },
    PaletteEntry {
        label: "save search as filter",
        keys: "fs",
        dispatch: PaletteDispatch::Global(Action::SaveCurrentFilter),
    },
    PaletteEntry {
        label: "cycle sort",
        keys: "S",
        dispatch: PaletteDispatch::Global(Action::CycleSort),
    },
    PaletteEntry {
        label: "toggle visual / multi-select",
        keys: "v",
        dispatch: PaletteDispatch::Global(Action::ToggleVisual),
    },
    PaletteEntry {
        label: "toggle selected row",
        keys: "Space",
        dispatch: PaletteDispatch::Global(Action::ToggleSelected),
    },
    PaletteEntry {
        label: "list view",
        keys: "l",
        dispatch: PaletteDispatch::Global(Action::GoList),
    },
    PaletteEntry {
        label: "toggle archive view",
        keys: "a",
        dispatch: PaletteDispatch::Global(Action::ToggleArchiveView),
    },
    PaletteEntry {
        label: "archive completed tasks",
        keys: "A",
        dispatch: PaletteDispatch::Global(Action::ArchiveCompleted),
    },
    PaletteEntry {
        label: "show done in list",
        keys: "H",
        dispatch: PaletteDispatch::Global(Action::ToggleShowDone),
    },
    PaletteEntry {
        label: "show future in list",
        keys: "F",
        dispatch: PaletteDispatch::Global(Action::ToggleShowFuture),
    },
    PaletteEntry {
        label: "toggle filter pane",
        keys: "[",
        dispatch: PaletteDispatch::Global(Action::ToggleLeftPane),
    },
    PaletteEntry {
        label: "toggle detail pane",
        keys: "]",
        dispatch: PaletteDispatch::Global(Action::ToggleRightPane),
    },
    PaletteEntry {
        label: "pick theme",
        keys: "T",
        dispatch: PaletteDispatch::Global(Action::OpenThemePicker),
    },
    PaletteEntry {
        label: "cycle theme",
        keys: "",
        dispatch: PaletteDispatch::Global(Action::CycleTheme),
    },
    PaletteEntry {
        label: "cycle density",
        keys: "D",
        dispatch: PaletteDispatch::Global(Action::CycleDensity),
    },
    PaletteEntry {
        label: "toggle line numbers",
        keys: "L",
        dispatch: PaletteDispatch::Global(Action::ToggleLineNum),
    },
    PaletteEntry {
        label: "open help",
        keys: "?",
        dispatch: PaletteDispatch::Global(Action::OpenHelp),
    },
    PaletteEntry {
        label: "open settings",
        keys: ",",
        dispatch: PaletteDispatch::Global(Action::OpenSettings),
    },
    PaletteEntry {
        label: "open command palette",
        keys: ": / Ctrl-P",
        dispatch: PaletteDispatch::Global(Action::OpenCommandPalette),
    },
    PaletteEntry {
        label: "show capture QR",
        keys: "s",
        dispatch: PaletteDispatch::Global(Action::OpenShare),
    },
    PaletteEntry {
        label: "escape / clear",
        keys: "Esc",
        dispatch: PaletteDispatch::Global(Action::EscapeStack),
    },
    PaletteEntry {
        label: "quit",
        keys: "q",
        dispatch: PaletteDispatch::Global(Action::Quit),
    },
    PaletteEntry {
        label: "reschedule",
        keys: "r",
        dispatch: PaletteDispatch::Global(Action::Reschedule),
    },
    PaletteEntry {
        label: "Change week start",
        keys: "W",
        dispatch: PaletteDispatch::Global(Action::ChangeWeekStart),
    },
    // ---- T14: notes-popup-internal entries, Mode::Notes-only ----------
    PaletteEntry {
        label: "create note",
        keys: "n",
        dispatch: PaletteDispatch::NotesAction(NotesEntryAction::Create),
    },
    PaletteEntry {
        label: "rename note",
        keys: "r",
        dispatch: PaletteDispatch::NotesAction(NotesEntryAction::Rename),
    },
    PaletteEntry {
        label: "delete note",
        keys: "d",
        dispatch: PaletteDispatch::NotesAction(NotesEntryAction::Delete),
    },
    PaletteEntry {
        label: "unlink note",
        keys: "u",
        dispatch: PaletteDispatch::NotesAction(NotesEntryAction::Unlink),
    },
    PaletteEntry {
        label: "open note in editor (normal mode)",
        keys: "e",
        dispatch: PaletteDispatch::NotesAction(NotesEntryAction::OpenEditorNormal),
    },
    PaletteEntry {
        label: "open note in editor (insert mode)",
        keys: "i",
        dispatch: PaletteDispatch::NotesAction(NotesEntryAction::OpenEditorInsert),
    },
];

#[derive(Debug, Default, Clone)]
pub struct CommandPaletteState {
    /// Highlighted row in the *filtered* list. Reset to 0 whenever the user
    /// edits the search text so the highlight doesn't get stranded past the
    /// new result count.
    pub cursor: usize,
    /// Mode the user was in when they opened the palette. Restored on close
    /// so that opening the palette from Visual mode (with a selection) and
    /// cancelling — or running a visual-aware action like ToggleComplete —
    /// keeps the selection meaningful instead of silently dropping into
    /// Normal.
    prior_mode: Option<Mode>,
    /// Cached filter inputs and outputs. `refresh` recomputes only when the
    /// needle changes, so multiple call sites (Enter, Up/Down, render) can
    /// read `hits()` per frame without re-running the match each time.
    cached_needle: String,
    cached_hits: Vec<PaletteHit>,
}

impl CommandPaletteState {
    /// Snapshot the mode the palette is being opened from, reset the
    /// highlight, and seed the cache with the unfiltered list so the first
    /// frame doesn't have to fall through `refresh`.
    pub fn open(&mut self, prior: Mode) {
        self.cursor = 0;
        self.prior_mode = Some(prior);
        self.cached_needle.clear();
        self.cached_hits = filtered("", prior);
    }

    /// Consume the snapshot taken in `open`. Defaults to `Normal` if the
    /// palette was somehow closed without a matching open — keeps the close
    /// path total instead of panicking.
    pub fn take_prior(&mut self) -> Mode {
        self.prior_mode.take().unwrap_or(Mode::Normal)
    }

    /// Read the snapshot without consuming it. Renderers use this to keep
    /// the underlying UI looking the same while the palette overlay is open.
    pub fn prior(&self) -> Option<Mode> {
        self.prior_mode
    }

    /// Currently visible hits, in rank order. Computed at most once per
    /// `refresh` call.
    pub fn hits(&self) -> &[PaletteHit] {
        &self.cached_hits
    }

    /// Recompute `hits` if `needle` differs from the cached needle and snap
    /// the highlight back to the top. Cheap no-op when the needle is the
    /// same (e.g., a cursor-move keystroke that doesn't change the draft).
    pub fn refresh(&mut self, needle: &str) {
        if self.cached_needle == needle {
            return;
        }
        self.cached_needle.clear();
        self.cached_needle.push_str(needle);
        // `open` always sets `prior_mode` before `refresh` can be called (the
        // palette only accepts keystrokes once `Mode::CommandPalette` is
        // active), so this default is a total-function safety net, not a
        // real fallback path.
        let prior = self.prior_mode.unwrap_or(Mode::Normal);
        self.cached_hits = filtered(needle, prior);
        self.cursor = 0;
    }

    /// Move the highlight by `dir` rows with wrap-around. No-op when the
    /// filtered list is empty.
    pub fn step(&mut self, dir: i32) {
        let len = self.cached_hits.len();
        if len == 0 {
            return;
        }
        let cur = self.cursor.min(len - 1) as i32;
        let next = (cur + dir).rem_euclid(len as i32) as usize;
        self.cursor = next;
    }

    /// Dispatch under the highlight, if any. Returns `None` when the filter
    /// produced no matches.
    pub fn current_dispatch(&self) -> Option<PaletteDispatch> {
        let hit = self.cached_hits.get(self.cursor)?;
        ENTRIES.get(hit.entry_idx).map(|e| e.dispatch)
    }
}

/// One filtered hit: the index into `ENTRIES`, plus the matched byte offsets
/// inside that entry's label (for highlighting).
#[derive(Debug, Clone)]
pub struct PaletteHit {
    pub entry_idx: usize,
    pub match_positions: Vec<usize>,
}

/// True when `entry` should be listed at all given the mode the palette was
/// opened from. `Global` entries are always reachable, any prior mode —
/// they mirror a real `Action`/`KeyBindings` entry that's already reachable
/// from wherever the palette was opened. `NotesAction` entries only make
/// sense while actually inside the notes popup (they call `App` methods that
/// read `app.notes_popup`'s current selection), so they're hidden unless
/// `prior == Mode::Notes`.
fn entry_visible(entry: &PaletteEntry, prior: Mode) -> bool {
    match entry.dispatch {
        PaletteDispatch::Global(_) => true,
        PaletteDispatch::NotesAction(_) => prior == Mode::Notes,
    }
}

/// Filter `ENTRIES` against `needle`, first narrowed to the entries visible
/// for `prior` (see `entry_visible`). Empty needle returns every visible
/// entry in declaration order. Non-empty needle returns only visible entries
/// whose label contains the needle as a case-insensitive subsequence, ranked
/// by:
///   1. where the first match sits — byte 0 beats a word-boundary match beats
///      a mid-word match. ("arch" → "archive…" before "toggle archive…"
///      before "fuzzy search".)
///   2. tightness of the run (smaller span first).
///   3. declaration order, via the stable sort.
pub fn filtered(needle: &str, prior: Mode) -> Vec<PaletteHit> {
    if needle.is_empty() {
        return ENTRIES
            .iter()
            .enumerate()
            .filter(|(_, e)| entry_visible(e, prior))
            .map(|(i, _)| PaletteHit {
                entry_idx: i,
                match_positions: Vec::new(),
            })
            .collect();
    }
    let mut hits: Vec<((u8, usize), PaletteHit)> = ENTRIES
        .iter()
        .enumerate()
        .filter(|(_, e)| entry_visible(e, prior))
        .filter_map(|(i, e)| {
            subseq_match_ci(e.label, needle).map(|positions| {
                let rank = rank_hit(e.label, &positions);
                (
                    rank,
                    PaletteHit {
                        entry_idx: i,
                        match_positions: positions,
                    },
                )
            })
        })
        .collect();
    // Stable sort preserves declaration order within a rank tier.
    hits.sort_by_key(|(rank, _)| *rank);
    hits.into_iter().map(|(_, h)| h).collect()
}

fn rank_hit(label: &str, positions: &[usize]) -> (u8, usize) {
    // `positions` is non-empty here: subseq_match_ci returns Some only for
    // a fully-matched needle, and the empty-needle case returns earlier.
    let first = positions[0];
    let last = positions[positions.len() - 1];
    let span = last - first;
    let start_tier = if first == 0 {
        0
    } else if is_word_boundary(label, first) {
        1
    } else {
        2
    };
    (start_tier, span)
}

/// True when `byte` is the start of a word: position 0, or the previous
/// char is non-alphanumeric (whitespace, punctuation, …). `byte` is assumed
/// to fall on a char boundary, which it does for every position returned by
/// `subseq_match_ci`.
fn is_word_boundary(s: &str, byte: usize) -> bool {
    if byte == 0 {
        return true;
    }
    s[..byte]
        .chars()
        .next_back()
        .is_none_or(|c| !c.is_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_needle_in_normal_mode_returns_only_global_entries() {
        let global_count = ENTRIES
            .iter()
            .filter(|e| matches!(e.dispatch, PaletteDispatch::Global(_)))
            .count();
        let hits = filtered("", Mode::Normal);
        assert_eq!(hits.len(), global_count);
        for h in &hits {
            assert!(
                matches!(ENTRIES[h.entry_idx].dispatch, PaletteDispatch::Global(_)),
                "Mode::Normal must never surface a NotesAction entry"
            );
        }
    }

    #[test]
    fn empty_needle_in_notes_mode_returns_every_entry() {
        let hits = filtered("", Mode::Notes);
        assert_eq!(
            hits.len(),
            ENTRIES.len(),
            "Mode::Notes must surface both Global and NotesAction entries"
        );
    }

    #[test]
    fn notes_actions_visible_only_when_prior_mode_is_notes() {
        let visible_in_notes: Vec<&str> = filtered("", Mode::Notes)
            .iter()
            .filter(|h| {
                matches!(
                    ENTRIES[h.entry_idx].dispatch,
                    PaletteDispatch::NotesAction(_)
                )
            })
            .map(|h| ENTRIES[h.entry_idx].label)
            .collect();
        assert!(visible_in_notes.contains(&"create note"));
        assert!(visible_in_notes.contains(&"rename note"));
        assert!(visible_in_notes.contains(&"delete note"));
        assert!(visible_in_notes.contains(&"unlink note"));
        assert!(visible_in_notes.contains(&"open note in editor (normal mode)"));
        assert!(visible_in_notes.contains(&"open note in editor (insert mode)"));

        for prior in [Mode::Normal, Mode::Visual] {
            let hits = filtered("", prior);
            assert!(
                hits.iter().all(|h| !matches!(
                    ENTRIES[h.entry_idx].dispatch,
                    PaletteDispatch::NotesAction(_)
                )),
                "NotesAction entries must be hidden when prior mode is {prior:?}"
            );
        }
    }

    #[test]
    fn global_entries_remain_visible_regardless_of_prior_mode() {
        let normal_count = filtered("", Mode::Normal).len();
        let global_in_notes = filtered("", Mode::Notes)
            .iter()
            .filter(|h| matches!(ENTRIES[h.entry_idx].dispatch, PaletteDispatch::Global(_)))
            .count();
        assert_eq!(
            normal_count, global_in_notes,
            "every Global entry visible from Mode::Normal must also be visible from Mode::Notes"
        );
    }

    #[test]
    fn matches_subsequence_in_label() {
        let hits = filtered("arch", Mode::Normal);
        assert!(!hits.is_empty());
        // Every hit must contain 'a','r','c','h' (case-insensitive) in order.
        for h in &hits {
            let label = ENTRIES[h.entry_idx].label.to_lowercase();
            let mut needle = "arch".chars();
            let mut cur = needle.next();
            for ch in label.chars() {
                if Some(ch) == cur {
                    cur = needle.next();
                }
            }
            assert!(
                cur.is_none(),
                "label {:?} should match 'arch'",
                ENTRIES[h.entry_idx].label
            );
        }
    }

    #[test]
    fn no_match_returns_empty() {
        assert!(filtered("zzzqqq", Mode::Normal).is_empty());
    }

    #[test]
    fn start_of_label_ranks_above_mid_label() {
        // "arch" appears at byte 0 of "archive completed tasks", after a
        // space in "toggle archive view", and mid-word in "fuzzy search".
        // Start-of-label must win.
        let hits = filtered("arch", Mode::Normal);
        let labels: Vec<&str> = hits.iter().map(|h| ENTRIES[h.entry_idx].label).collect();
        assert_eq!(labels.first().copied(), Some("archive completed tasks"));
    }

    #[test]
    fn word_boundary_ranks_above_mid_word() {
        let hits = filtered("arch", Mode::Normal);
        let labels: Vec<&str> = hits.iter().map(|h| ENTRIES[h.entry_idx].label).collect();
        let toggle = labels
            .iter()
            .position(|&l| l == "toggle archive view")
            .expect("toggle archive view in results");
        let fuzzy = labels
            .iter()
            .position(|&l| l == "fuzzy search")
            .expect("fuzzy search in results");
        assert!(
            toggle < fuzzy,
            "word-boundary match (toggle archive view) should rank above mid-word match (fuzzy search)"
        );
    }

    #[test]
    fn tighter_match_ranks_above_gappier_within_tier() {
        // Both labels start mid-word for needle "ye" (matches `y` then `e`).
        // "cycle theme":  y@2  e@5  → span 3
        // "cycle density": y@2 e@10 → span 8
        // Same start tier, so tightness decides.
        let hits = filtered("ye", Mode::Normal);
        let labels: Vec<&str> = hits.iter().map(|h| ENTRIES[h.entry_idx].label).collect();
        let theme = labels.iter().position(|&l| l == "cycle theme");
        let density = labels.iter().position(|&l| l == "cycle density");
        if let (Some(t), Some(d)) = (theme, density) {
            assert!(t < d, "tighter span should rank first within a tier");
        }
    }

    #[test]
    fn case_insensitive_match() {
        let upper = filtered("ARCH", Mode::Normal);
        let lower = filtered("arch", Mode::Normal);
        assert_eq!(upper.len(), lower.len());
        let upper_ids: Vec<usize> = upper.iter().map(|h| h.entry_idx).collect();
        let lower_ids: Vec<usize> = lower.iter().map(|h| h.entry_idx).collect();
        assert_eq!(upper_ids, lower_ids);
    }

    #[test]
    fn entries_cover_every_meaningful_action() {
        // `ArmF` is intentionally omitted (it's only a chord leader, not a
        // user-facing action). Every other Action variant must be reachable
        // from the palette via a `PaletteDispatch::Global` entry — the
        // `NotesAction` entries (T14) are a separate, deliberately
        // non-`Action` dispatch and are checked by
        // `entries_cover_every_notes_action` below, not this guarantee.
        let actions: Vec<Action> = ENTRIES
            .iter()
            .filter_map(|e| match e.dispatch {
                PaletteDispatch::Global(a) => Some(a),
                PaletteDispatch::NotesAction(_) => None,
            })
            .collect();
        let required = [
            Action::Quit,
            Action::CursorDown,
            Action::CursorUp,
            Action::CursorTop,
            Action::CursorBottom,
            Action::HalfPageDown,
            Action::HalfPageUp,
            Action::BeginAdd,
            Action::BeginEdit,
            Action::BeginEditInsert,
            Action::ToggleComplete,
            Action::Delete,
            Action::CyclePriority,
            Action::MoveTaskDown,
            Action::MoveTaskUp,
            Action::BeginSearch,
            Action::OpenHelp,
            Action::OpenSettings,
            Action::OpenCommandPalette,
            Action::Undo,
            Action::ToggleVisual,
            Action::ToggleSelected,
            Action::GoList,
            Action::ToggleArchiveView,
            Action::ArchiveCompleted,
            Action::PickProject,
            Action::PickContext,
            Action::CycleSort,
            Action::BeginPromptProject,
            Action::BeginPromptContext,
            Action::ToggleLeftPane,
            Action::ToggleRightPane,
            Action::CycleTheme,
            Action::CycleDensity,
            Action::ToggleLineNum,
            Action::ToggleShowDone,
            Action::ToggleShowFuture,
            Action::CopyLine,
            Action::CopyBody,
            Action::OpenNotes,
            Action::EscapeStack,
            Action::OpenShare,
            Action::OpenThemePicker,
            Action::Reschedule,
        ];
        for a in required {
            assert!(actions.contains(&a), "missing palette entry for {a:?}");
        }
    }

    #[test]
    fn entries_cover_every_notes_action() {
        // The counterpart guarantee to `entries_cover_every_meaningful_action`
        // above, but for the T14 `NotesAction` dispatch instead of `Action`:
        // every notes-popup-internal operation must be reachable from the
        // palette while the prior mode is `Mode::Notes`.
        let notes_actions: Vec<NotesEntryAction> = ENTRIES
            .iter()
            .filter_map(|e| match e.dispatch {
                PaletteDispatch::NotesAction(a) => Some(a),
                PaletteDispatch::Global(_) => None,
            })
            .collect();
        let required = [
            NotesEntryAction::Create,
            NotesEntryAction::Rename,
            NotesEntryAction::Delete,
            NotesEntryAction::Unlink,
            NotesEntryAction::OpenEditorNormal,
            NotesEntryAction::OpenEditorInsert,
        ];
        for a in required {
            assert!(
                notes_actions.contains(&a),
                "missing palette entry for NotesEntryAction::{a:?}"
            );
        }
    }
}
