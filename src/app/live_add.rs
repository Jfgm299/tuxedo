//! Live capture in the add dialog: natural language detected while typing.
//!
//! Every keystroke re-runs [`nl::detect`] over the draft. Recognised phrases
//! are coloured in place (rendering in `ui/dialog.rs`) and fill a row of
//! chips — one per field, empty ones dimmed as a hint. The text itself stays
//! as typed; it is converted to todo.txt only when saved.
//!
//! Keys (wired in `main.rs`): `Tab` moves onto the chips, then `x` rejects a
//! wrong detection (its words become plain text again) and `Enter` opens
//! that field's picker; `Ctrl+Z` rejects the newest detection; one `Enter`
//! saves and keeps the dialog open for the next task, with a confirmation
//! toast — and `Ctrl+Z` on the empty dialog undoes that add.

use chrono::NaiveDate;

use super::App;
use super::draft_overlay::CalendarTarget;
use super::types::AddOutcome;
use crate::core::AddOutcome as CoreAdd;
use crate::nl::{self, DetectedSpan, Detection, FieldKind};

/// The chip row, in display order. `ShowFrom` only appears once detected.
pub const CHIP_ORDER: [FieldKind; 7] = [
    FieldKind::Date,
    FieldKind::Time,
    FieldKind::Repeat,
    FieldKind::Project,
    FieldKind::Context,
    FieldKind::Priority,
    FieldKind::ShowFrom,
];

/// One chip: its field and, when detected, the value to show and the phrase
/// it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    pub kind: FieldKind,
    pub value: Option<String>,
    pub span: Option<DetectedSpan>,
}

impl App {
    fn today_date(&self) -> Option<NaiveDate> {
        NaiveDate::parse_from_str(self.store.today(), "%Y-%m-%d").ok()
    }

    /// Whether the dialog is adding a task (live capture) rather than
    /// editing an existing one.
    pub fn live_add_active(&self) -> bool {
        self.selection.editing().is_none()
    }

    /// The live detection over the current draft.
    pub fn live_detection(&self) -> Detection {
        match self.today_date() {
            Some(today) => nl::detect(self.draft.text(), today, &self.draft.live.rejected),
            None => Detection::default(),
        }
    }

    /// The chip row for the current draft.
    pub fn live_chips(&self) -> Vec<Chip> {
        let det = self.live_detection();
        let p = &det.parsed;
        let span_of = |kind| det.spans.iter().rev().find(|s| s.kind == kind).copied();
        CHIP_ORDER
            .iter()
            .filter_map(|&kind| {
                let value = match kind {
                    FieldKind::Date => p
                        .due
                        .map(|d| d.format("%a %-d %b").to_string().to_lowercase()),
                    FieldKind::Time => p.time.map(|(h, m)| format!("{h:02}:{m:02}")),
                    FieldKind::Repeat => p.rec.as_deref().map(describe_rec),
                    FieldKind::Project => (!p.projects.is_empty()).then(|| p.projects.join(" ")),
                    FieldKind::Context => (!p.contexts.is_empty()).then(|| p.contexts.join(" ")),
                    FieldKind::Priority => p.priority.map(|c| format!("({c})")),
                    FieldKind::ShowFrom => p.threshold.clone(),
                };
                if kind == FieldKind::ShowFrom && value.is_none() {
                    return None;
                }
                Some(Chip {
                    kind,
                    span: value.as_ref().and_then(|_| span_of(kind)),
                    value,
                })
            })
            .collect()
    }

    /// Call after every edit of the draft: keeps the order in which
    /// detections appeared, so `Ctrl+Z` knows which one is newest.
    pub fn live_refresh(&mut self) {
        let det = self.live_detection();
        let text = self.draft.text().to_string();
        let current: Vec<nl::Rejection> = det
            .spans
            .iter()
            .map(|s| (s.kind, text[s.start..s.end].to_lowercase()))
            .collect();
        let live = &mut self.draft.live;
        live.seen.retain(|d| current.contains(d));
        for d in current {
            if !live.seen.contains(&d) {
                live.seen.push(d);
            }
        }
        if let Some(i) = live.chip_focus {
            live.chip_focus = Some(i.min(CHIP_ORDER.len() - 1));
        }
        if !self.draft.text().trim().is_empty() {
            self.draft.live.toast = None;
        }
    }

    /// `Ctrl+Z`: turn the newest detection back into plain text. On an
    /// empty dialog right after an add, undo that add instead. Returns
    /// whether anything happened.
    pub fn live_undo(&mut self) -> bool {
        if let Some(last) = self.draft.live.seen.pop() {
            self.draft.live.rejected.push(last);
            self.live_refresh();
            return true;
        }
        if self.draft.text().trim().is_empty() && self.draft.live.toast.take().is_some() {
            self.undo();
            self.flash("add undone");
            return true;
        }
        false
    }

    pub fn live_chip_focus(&self) -> Option<usize> {
        self.draft.live.chip_focus
    }

    /// `Tab` from the text: focus the first chip.
    pub fn live_focus_chips(&mut self) {
        self.draft.live.chip_focus = Some(0);
    }

    pub fn live_unfocus_chips(&mut self) {
        self.draft.live.chip_focus = None;
    }

    /// Move the chip focus, wrapping around.
    pub fn live_chip_step(&mut self, forward: bool) {
        let n = self.live_chips().len();
        if n == 0 {
            return;
        }
        let i = self.draft.live.chip_focus.unwrap_or(0).min(n - 1);
        self.draft.live.chip_focus = Some(if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        });
    }

    fn focused_chip(&self) -> Option<Chip> {
        let i = self.draft.live.chip_focus?;
        self.live_chips().get(i).cloned()
    }

    /// `x` on a chip: reject its detection, leaving its words as text.
    pub fn live_reject_focused(&mut self) {
        let Some(chip) = self.focused_chip() else {
            return;
        };
        let Some(span) = chip.span else {
            return;
        };
        let phrase = self.draft.text()[span.start..span.end].to_lowercase();
        self.draft.live.rejected.push((chip.kind, phrase));
        self.live_refresh();
    }

    /// `Enter` on a chip: open that field's picker. A detected phrase is
    /// first replaced by its canonical token, so the picker starts from the
    /// detected value and writes back over it. Project, context and time
    /// have no picker: the matching sigil (or `at `) is typed and focus goes
    /// back to the text, where autocomplete takes over.
    pub fn live_open_focused(&mut self) {
        let Some(chip) = self.focused_chip() else {
            return;
        };
        self.draft.live.chip_focus = None;
        let det = self.live_detection();
        let p = det.parsed.clone();
        if let Some(span) = chip.span {
            let canonical = match chip.kind {
                FieldKind::Date => p.due.map(|d| format!("due:{}", d.format("%Y-%m-%d"))),
                FieldKind::ShowFrom => p.threshold.map(|t| format!("t:{t}")),
                FieldKind::Repeat => p.rec.map(|r| format!("rec:{r}")),
                // The priority picker writes the `(X)` prefix itself.
                FieldKind::Priority => Some(String::new()),
                _ => None,
            };
            if let Some(token) = canonical {
                let text = self.draft.text();
                let is_token = text[span.start..span.end].contains(':')
                    || (chip.kind == FieldKind::Priority && span.start == 0);
                if !is_token {
                    self.draft.replace_token(span.start, span.end, &token);
                }
            }
        }
        match chip.kind {
            FieldKind::Date => self.open_calendar(CalendarTarget::Due),
            FieldKind::ShowFrom => self.open_calendar(CalendarTarget::Threshold),
            FieldKind::Repeat => self.open_recurrence_builder(),
            FieldKind::Priority => self.open_priority_chooser(),
            FieldKind::Project => self.live_append(" +"),
            FieldKind::Context => self.live_append(" @"),
            FieldKind::Time => self.live_append(" at "),
        }
        self.live_refresh();
    }

    /// Append `s` at the end of the draft (dropping a doubled space) and
    /// park the cursor there.
    fn live_append(&mut self, s: &str) {
        self.draft_end();
        let s = if self.draft.text().is_empty() || self.draft.text().ends_with(' ') {
            s.trim_start()
        } else {
            s
        };
        for c in s.chars() {
            self.draft_insert_char(c);
        }
    }

    /// `Enter`: save the draft — converted to todo.txt when anything was
    /// detected — and keep the dialog open, empty, for the next task.
    pub fn live_add(&mut self) -> AddOutcome {
        let text = self.draft.text().trim().to_string();
        if text.is_empty() || text == self.filter.tag_seed().trim_end() {
            return AddOutcome::Empty;
        }
        let det = self.live_detection();
        let (line, title) = if det.is_empty() {
            (text.clone(), text.clone())
        } else {
            if det.parsed.body.trim().is_empty() {
                self.flash("add a title to the task");
                return AddOutcome::Invalid;
            }
            (det.to_todo_txt(), det.parsed.body.clone())
        };
        match self.store.add_finalized(&line) {
            CoreAdd::Added { abs } => {
                self.after_mutation(abs);
                self.flash(format!("added: {title}"));
                let seed = self.filter.tag_seed();
                self.draft_set_insert(seed);
                self.draft.live.toast = Some(title);
                AddOutcome::Saved
            }
            CoreAdd::Empty => AddOutcome::Empty,
            CoreAdd::Aborted(r) => {
                self.handle_reconcile_abort(r);
                AddOutcome::Invalid
            }
            CoreAdd::Error(e) => {
                self.flash(format!("invalid: {e}"));
                AddOutcome::Invalid
            }
        }
    }

    /// The confirmation toast, while the flash that announced it is up.
    pub fn live_toast(&self) -> Option<&str> {
        self.flash_active()?;
        self.draft.live.toast.as_deref()
    }
}

/// `rec:` value in words: `1w` → "every week", `+2m` → "every 2 months".
pub fn describe_rec(rec: &str) -> String {
    let Some(spec) = crate::recurrence::parse_rec_spec(rec) else {
        return rec.to_string();
    };
    use crate::recurrence::RecUnit;
    let unit = match spec.unit {
        RecUnit::Day => "day",
        RecUnit::BusinessDay => "weekday",
        RecUnit::Week => "week",
        RecUnit::Month => "month",
        RecUnit::Year => "year",
    };
    if spec.n == 1 {
        format!("every {unit}")
    } else {
        format!("every {} {unit}s", spec.n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::build_app;

    fn typed(app: &mut App, s: &str) {
        for c in s.chars() {
            app.draft_insert_char(c);
            app.live_refresh();
        }
    }

    fn chip(app: &App, kind: FieldKind) -> Chip {
        app.live_chips()
            .into_iter()
            .find(|c| c.kind == kind)
            .expect("chip")
    }

    #[test]
    fn chips_fill_as_phrases_are_detected() {
        let mut app = build_app("");
        typed(&mut app, "call anna");
        assert!(app.live_chips().iter().all(|c| c.value.is_none()));
        typed(&mut app, " tomorrow at 6pm @calls");
        assert!(chip(&app, FieldKind::Date).value.is_some());
        assert_eq!(chip(&app, FieldKind::Time).value.as_deref(), Some("18:00"));
        assert_eq!(
            chip(&app, FieldKind::Context).value.as_deref(),
            Some("calls")
        );
        assert!(chip(&app, FieldKind::Project).value.is_none());
    }

    #[test]
    fn ctrl_z_rejects_the_newest_detection_first() {
        let mut app = build_app("");
        typed(&mut app, "report friday at 9am");
        assert!(app.live_undo());
        assert!(
            chip(&app, FieldKind::Time).value.is_none(),
            "time undone first"
        );
        assert!(chip(&app, FieldKind::Date).value.is_some());
        assert!(app.live_undo());
        assert!(chip(&app, FieldKind::Date).value.is_none());
    }

    #[test]
    fn x_on_a_focused_chip_rejects_it() {
        let mut app = build_app("");
        typed(&mut app, "notes from friday");
        app.live_focus_chips();
        assert_eq!(app.live_chip_focus(), Some(0), "Date is the first chip");
        app.live_reject_focused();
        assert!(chip(&app, FieldKind::Date).value.is_none());
        assert_eq!(app.draft.text(), "notes from friday", "text untouched");
    }

    #[test]
    fn enter_saves_canonical_and_keeps_the_dialog_ready() {
        let mut app = build_app("");
        typed(&mut app, "call anna at 6pm +work");
        assert_eq!(app.live_add(), AddOutcome::Saved);
        let raw = &app.tasks().last().expect("added").raw;
        assert!(raw.contains("call anna +work at:18:00"), "{raw}");
        assert_eq!(app.draft.text(), "", "ready for the next task");
        assert_eq!(app.live_toast(), Some("call anna"));

        // Ctrl+Z on the empty dialog undoes the add.
        let before = app.tasks().len();
        assert!(app.live_undo());
        assert_eq!(app.tasks().len(), before - 1);
    }

    #[test]
    fn a_rejected_phrase_is_saved_as_text() {
        let mut app = build_app("");
        typed(&mut app, "notes from friday");
        app.live_undo();
        app.live_add();
        let raw = &app.tasks().last().expect("added").raw;
        assert!(raw.ends_with("notes from friday"), "{raw}");
        assert!(!raw.contains("due:"), "{raw}");
    }

    #[test]
    fn detection_only_without_a_title_is_refused() {
        let mut app = build_app("");
        typed(&mut app, "tomorrow");
        assert_eq!(app.live_add(), AddOutcome::Invalid);
        assert_eq!(app.draft.text(), "tomorrow", "nothing lost");
    }

    #[test]
    fn enter_on_the_date_chip_opens_the_calendar_on_the_detected_date() {
        let mut app = build_app("");
        typed(&mut app, "dentist tomorrow");
        app.live_focus_chips();
        app.live_open_focused();
        assert!(app.draft.text().contains("due:"), "{}", app.draft.text());
        assert!(app.calendar_state().is_some());
    }

    #[test]
    fn rec_descriptions() {
        assert_eq!(describe_rec("1w"), "every week");
        assert_eq!(describe_rec("+2m"), "every 2 months");
        assert_eq!(describe_rec("3b"), "every 3 weekdays");
    }
}
