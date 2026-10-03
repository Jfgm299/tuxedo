//! Natural-language parser for the add-todo draft.
//!
//! When the user types prose into the add buffer ("Pay rent monthly on the
//! first, show 3 days before due, project home"), this module extracts the
//! structured todo.txt metadata so the caller can rewrite the buffer into
//! canonical form for the user to review.
//!
//! Pure logic — no I/O, no app state. The crate-level wiring lives in
//! `app::mutations::add_from_draft`.
//!
//! Detection (`looks_like_natural_language`) is intentionally conservative:
//! it returns `false` whenever the buffer already contains a `due:` / `rec:`
//! / `t:` token, which gives the rewrite pipeline trivial idempotency — a
//! second Enter on the canonical output falls through to the existing save
//! path.

use chrono::{Datelike, Days, Months, NaiveDate, Weekday};

use crate::todo;

/// Structured fields extracted from a prose draft. Each `Option` field is
/// `None` when the user didn't say anything about that aspect; `Vec` fields
/// are empty for the same reason. `body` is the input with all recognized
/// phrases stripped and whitespace collapsed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ParsedNl {
    pub body: String,
    /// The deadline: "by friday", "due friday".
    pub due: Option<NaiveDate>,
    /// When you plan to do it: "on friday", "tomorrow".
    pub planned: Option<NaiveDate>,
    /// How long it takes, in minutes: "for 1h".
    pub duration: Option<u32>,
    /// Reminders, in minutes before its time: "remind me 15 min before".
    pub reminders: Vec<u32>,
    pub rec: Option<String>,
    pub threshold: Option<String>,
    pub projects: Vec<String>,
    pub contexts: Vec<String>,
    pub priority: Option<char>,
    /// Time of day, `(hour, minute)` in 24h — "at 6pm" → `(18, 0)`. Written
    /// as an `at:HH:MM` tag.
    pub time: Option<(u32, u32)>,
}

/// What a recognised phrase sets — one per chip in the add dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldKind {
    /// When it's planned (`plan:`).
    Date,
    /// The deadline (`due:`).
    Deadline,
    /// How long it takes (`dur:`).
    Duration,
    /// Reminders before its time (`remind:`).
    Reminder,
    /// Time of day (`at:`).
    Time,
    /// Recurrence (`rec:`).
    Repeat,
    /// When the task starts showing (`t:`).
    ShowFrom,
    Project,
    Context,
    Priority,
}

/// One recognised phrase: the byte range it covers in the typed text and
/// the field it sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetectedSpan {
    pub start: usize,
    pub end: usize,
    pub kind: FieldKind,
}

/// Result of [`detect`]: the structured fields plus where each one came
/// from in the text, for live highlighting.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Detection {
    pub parsed: ParsedNl,
    pub spans: Vec<DetectedSpan>,
}

impl Detection {
    /// Whether anything at all was recognised.
    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// The canonical todo.txt line this text saves as.
    pub fn to_todo_txt(&self) -> String {
        format_as_todo_txt(&self.parsed)
    }
}

/// A detection the user rejected (`x` on its chip, `Ctrl+Z`): the field and
/// the lower-cased phrase. A matching phrase is left as plain text.
pub type Rejection = (FieldKind, String);

/// Live parse for the add dialog: like [`try_parse`], but runs on any text
/// (also text that already contains `due:` / `rec:` / `t:` / `at:` tokens,
/// which count as detected fields too), reports the byte span of every
/// recognised phrase, and leaves `rejected` phrases as plain words.
pub fn detect(text: &str, today: NaiveDate, rejected: &[Rejection]) -> Detection {
    let mut blocked = vec![false; text.len()];
    // A rejected phrase can, once blocked, let a shorter phrase inside it be
    // detected instead; a few rounds settle that.
    for _ in 0..4 {
        let detection = detect_once(text, today, &blocked);
        let mut changed = false;
        for span in &detection.spans {
            let phrase = text[span.start..span.end].to_lowercase();
            if rejected
                .iter()
                .any(|(kind, p)| *kind == span.kind && *p == phrase)
            {
                for b in &mut blocked[span.start..span.end] {
                    *b = true;
                }
                changed = true;
            }
        }
        if !changed {
            return detection;
        }
    }
    detect_once(text, today, &blocked)
}

fn detect_once(text: &str, today: NaiveDate, blocked: &[bool]) -> Detection {
    let mut scratch = Scratch::new(text);
    scratch.blocked = blocked.to_vec();
    let mut parsed = ParsedNl::default();

    pass_leading_priority(&mut scratch, &mut parsed);
    pass_canonical(&mut scratch, &mut parsed);
    pass_sigiled(&mut scratch, &mut parsed);
    pass_time(&mut scratch, &mut parsed);
    pass_reminder(&mut scratch, &mut parsed);
    pass_duration(&mut scratch, &mut parsed);
    pass_threshold(&mut scratch, &mut parsed);
    let weekday_hint = pass_recurrence(&mut scratch, &mut parsed);
    pass_date(&mut scratch, &mut parsed, today, weekday_hint);
    pass_project_context(&mut scratch, &mut parsed);
    pass_priority(&mut scratch, &mut parsed);

    parsed.body = scratch.remaining_cleaned();
    let spans = merge_spans(text, std::mem::take(&mut scratch.spans));
    Detection { parsed, spans }
}

/// Sort spans and join neighbours of the same kind separated only by
/// whitespace, so "every other friday" is one highlighted phrase.
fn merge_spans(text: &str, mut spans: Vec<DetectedSpan>) -> Vec<DetectedSpan> {
    spans.sort_by_key(|s| s.start);
    let mut out: Vec<DetectedSpan> = Vec::new();
    for s in spans {
        if let Some(last) = out.last_mut()
            && last.kind == s.kind
            && s.start >= last.end
            && text[last.end..s.start].trim().is_empty()
        {
            last.end = s.end;
            continue;
        }
        out.push(s);
    }
    out
}

/// Cheap heuristic gating the full parse. Returns `true` when the buffer
/// looks like prose worth interpreting. Two rules:
///
/// 1. The buffer must not already contain a `due:` / `rec:` / `t:` token —
///    that means the user (or a previous NL rewrite) already produced
///    canonical form, so leave it alone.
/// 2. The buffer must contain at least one trigger word (date words,
///    weekdays, months, recurrence vocabulary, `before`, `project`,
///    `context`, …).
pub fn looks_like_natural_language(text: &str) -> bool {
    if ["due", "plan", "rec", "t"]
        .iter()
        .any(|k| has_kv_token(text, k))
    {
        return false;
    }
    contains_trigger(text)
}

/// Main entry point. `today` resolves relative dates ("tomorrow", "the first
/// of the month"). Returns `None` when the parser couldn't extract anything
/// structured — the caller then falls through to the plain save path.
pub fn try_parse(text: &str, today: NaiveDate) -> Option<ParsedNl> {
    let mut scratch = Scratch::new(text);
    let mut parsed = ParsedNl::default();

    pass_leading_priority(&mut scratch, &mut parsed);
    pass_sigiled(&mut scratch, &mut parsed);
    pass_time(&mut scratch, &mut parsed);
    pass_reminder(&mut scratch, &mut parsed);
    pass_duration(&mut scratch, &mut parsed);
    pass_threshold(&mut scratch, &mut parsed);
    let weekday_hint = pass_recurrence(&mut scratch, &mut parsed);
    pass_date(&mut scratch, &mut parsed, today, weekday_hint);
    pass_project_context(&mut scratch, &mut parsed);
    pass_priority(&mut scratch, &mut parsed);

    parsed.body = scratch.remaining_cleaned();

    let extracted = parsed.due.is_some()
        || parsed.planned.is_some()
        || parsed.duration.is_some()
        || !parsed.reminders.is_empty()
        || parsed.rec.is_some()
        || parsed.threshold.is_some()
        || !parsed.projects.is_empty()
        || !parsed.contexts.is_empty()
        || parsed.priority.is_some()
        || parsed.time.is_some();
    if extracted { Some(parsed) } else { None }
}

/// Serialize a parsed result back to a canonical todo.txt line. Token order
/// is fixed: `(P) body +proj… @ctx… due:… rec:… t:…`. An empty body falls
/// back to `"todo"` so the result is always a well-formed task — the caller
/// is expected to flash a hint so the user knows to fix the body.
pub fn format_as_todo_txt(p: &ParsedNl) -> String {
    let mut out = String::new();
    if let Some(prio) = p.priority {
        out.push('(');
        out.push(prio);
        out.push(')');
        out.push(' ');
    }
    let body = p.body.trim();
    if body.is_empty() {
        out.push_str("todo");
    } else {
        out.push_str(body);
    }
    for proj in &p.projects {
        out.push_str(" +");
        out.push_str(proj);
    }
    for ctx in &p.contexts {
        out.push_str(" @");
        out.push_str(ctx);
    }
    if let Some(d) = p.planned {
        out.push_str(" plan:");
        out.push_str(&d.format("%Y-%m-%d").to_string());
    }
    if let Some(d) = p.due {
        out.push_str(" due:");
        out.push_str(&d.format("%Y-%m-%d").to_string());
    }
    if let Some(r) = &p.rec {
        out.push_str(" rec:");
        out.push_str(r);
    }
    if let Some(t) = &p.threshold {
        out.push_str(" t:");
        out.push_str(t);
    }
    if let Some((h, m)) = p.time {
        out.push_str(&format!(" at:{h:02}:{m:02}"));
    }
    if let Some(m) = p.duration {
        out.push_str(" dur:");
        out.push_str(&crate::duration::format_minutes(m));
    }
    if !p.reminders.is_empty() {
        let list: Vec<String> = p
            .reminders
            .iter()
            .map(|m| crate::duration::format_minutes(*m))
            .collect();
        out.push_str(" remind:");
        out.push_str(&list.join(","));
    }
    out
}

// ---------------------------------------------------------------------------
// Trigger detection
// ---------------------------------------------------------------------------

fn has_kv_token(text: &str, key: &str) -> bool {
    for tok in text.split_whitespace() {
        if let Some((k, v)) = tok.split_once(':')
            && k == key
            && !v.is_empty()
        {
            return true;
        }
    }
    false
}

fn contains_trigger(text: &str) -> bool {
    let lower = ascii_lower(text);
    let words: Vec<&str> = lower
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| matches!(c, ',' | '.' | ';' | ':' | '!' | '?')))
        .collect();

    const SINGLE_TRIGGERS: &[&str] = &[
        // date words
        "today",
        "tonight",
        "tomorrow",
        "yesterday",
        // weekdays
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
        "sunday",
        "mon",
        "tue",
        "tues",
        "wed",
        "thu",
        "thurs",
        "fri",
        "sat",
        "sun",
        // recurrence
        "every",
        "each",
        "daily",
        "weekly",
        "biweekly",
        "monthly",
        "yearly",
        "annually",
        // prose markers
        "project",
        "proj",
        "context",
        "ctx",
        "priority",
        "before",
        "starting",
        "due",
        "by",
    ];

    for w in &words {
        if SINGLE_TRIGGERS.contains(w) {
            return true;
        }
        if parse_month(w).is_some() {
            return true;
        }
    }

    // Multi-word: "in N (day|week|month|year)s?"
    for i in 0..words.len() {
        if words[i] == "in" && i + 2 < words.len() {
            let n = words[i + 1]
                .parse::<u32>()
                .ok()
                .or_else(|| word_number(words[i + 1]));
            let unit = unit_char(words[i + 2]);
            if n.is_some() && unit.is_some() {
                return true;
            }
        }
    }

    false
}

// ---------------------------------------------------------------------------
// Scratch buffer: tracks consumed byte ranges across all passes.
// ---------------------------------------------------------------------------

struct Scratch<'a> {
    text: &'a str,
    /// ASCII-lowercased copy of `text`. Lowercasing only ASCII letters keeps
    /// byte indices aligned between `text` and `lower`, so a range valid in
    /// one is valid in the other.
    lower: String,
    consumed: Vec<bool>,
    /// Bytes the user rejected as a detection: never matched, but kept in
    /// the body (unlike `consumed`).
    blocked: Vec<bool>,
    /// The field the running pass sets; `mark` records a span of this kind.
    kind: Option<FieldKind>,
    /// Every marked range with its kind, for live highlighting.
    spans: Vec<DetectedSpan>,
    /// Cached word ranges over the original text. Recomputed via
    /// `live_words()` each pass — cheap since inputs are short.
    word_cache: Vec<(usize, usize)>,
}

impl<'a> Scratch<'a> {
    fn new(text: &'a str) -> Self {
        let lower = ascii_lower(text);
        let consumed = vec![false; text.len()];
        let word_cache = compute_words(text);
        Self {
            text,
            lower,
            consumed,
            blocked: vec![false; text.len()],
            kind: None,
            spans: Vec::new(),
            word_cache,
        }
    }

    /// Returns `true` if every byte in `[start, end)` is unconsumed. A
    /// fully-consumed word counts as gone for subsequent passes.
    fn is_live(&self, start: usize, end: usize) -> bool {
        (start..end).all(|i| {
            !self.consumed.get(i).copied().unwrap_or(true)
                && !self.blocked.get(i).copied().unwrap_or(false)
        })
    }

    fn mark(&mut self, start: usize, end: usize) {
        let end = end.min(self.consumed.len());
        if let Some(kind) = self.kind
            && start < end
        {
            self.spans.push(DetectedSpan { start, end, kind });
        }
        for slot in self.consumed[start..end].iter_mut() {
            *slot = true;
        }
    }

    /// Lower-case slice with trailing punctuation stripped — what most pattern
    /// matchers want to compare against.
    fn word_lc(&self, range: (usize, usize)) -> &str {
        self.lower[range.0..range.1].trim_end_matches([',', '.', ';', ':', '!', '?'])
    }

    /// Original-case slice with trailing punctuation stripped — used when the
    /// extracted value needs to round-trip (e.g. tag names).
    fn word_orig(&self, range: (usize, usize)) -> &str {
        self.text[range.0..range.1].trim_end_matches([',', '.', ';', ':', '!', '?'])
    }

    /// Remaining body text after stripping consumed bytes and collapsing
    /// whitespace. Leading/trailing connector words ("and", "it's", …) are
    /// also dropped so the body reads cleanly.
    fn remaining_cleaned(&self) -> String {
        let mut buf = String::new();
        let mut prev_space = true;
        for (i, c) in self.text.char_indices() {
            let is_consumed = self.consumed.get(i).copied().unwrap_or(false);
            if is_consumed || c.is_whitespace() {
                if !prev_space {
                    buf.push(' ');
                    prev_space = true;
                }
            } else {
                buf.push(c);
                prev_space = false;
            }
        }
        let mut tokens: Vec<&str> = buf.split_whitespace().collect();
        let is_connector = |t: &str| {
            let cleaned = t
                .trim_matches(|c: char| matches!(c, ',' | '.' | ';' | ':' | '!' | '?'))
                .to_ascii_lowercase();
            matches!(
                cleaned.as_str(),
                "and" | "or" | "but" | "it's" | "its" | "that" | "which" | ""
            )
        };
        while tokens.first().is_some_and(|t| is_connector(t)) {
            tokens.remove(0);
        }
        while tokens.last().is_some_and(|t| is_connector(t)) {
            tokens.pop();
        }
        let joined = tokens.join(" ");
        joined
            .trim_matches(|c: char| matches!(c, ',' | '.' | ';' | ':') || c.is_whitespace())
            .to_string()
    }
}

fn ascii_lower(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii() {
                c.to_ascii_lowercase()
            } else {
                c
            }
        })
        .collect()
}

fn compute_words(s: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let mut last_end = 0;
    for (i, c) in s.char_indices() {
        if c.is_whitespace() {
            if let Some(st) = start.take() {
                out.push((st, i));
            }
        } else if start.is_none() {
            start = Some(i);
        }
        last_end = i + c.len_utf8();
    }
    if let Some(st) = start {
        out.push((st, last_end));
    }
    out
}

// ---------------------------------------------------------------------------
// Pass 0: leading "(X) " priority prefix
// ---------------------------------------------------------------------------

/// Strip a leading `(X) ` priority token if the user typed canonical priority
/// syntax inside an otherwise prose buffer (e.g. `"(A) Buy milk tomorrow"`).
/// Without this, the `(A)` survives into the body — saving still works because
/// `todo::parse_line` strips it on re-parse, but `format_as_todo_txt` would
/// emit `"(A) Buy milk ..."` with the priority living *in the body*, and any
/// subsequent priority word in the prose would double up the prefix.
fn pass_leading_priority(scratch: &mut Scratch, p: &mut ParsedNl) {
    scratch.kind = Some(FieldKind::Priority);
    let bytes = scratch.text.as_bytes();
    if bytes.len() >= 4
        && scratch.is_live(0, 4)
        && bytes[0] == b'('
        && bytes[1].is_ascii_uppercase()
        && bytes[2] == b')'
        && bytes[3] == b' '
    {
        p.priority = Some(bytes[1] as char);
        scratch.mark(0, 4);
    }
}

// ---------------------------------------------------------------------------
// Canonical tokens (`due:` `t:` `rec:` `at:`), live detection only
// ---------------------------------------------------------------------------

/// Tokens already in todo.txt form — typed by hand, or written by a picker —
/// count as detected fields too, so their chips fill in. Only values that
/// parse are taken; anything else stays body text.
fn pass_canonical(scratch: &mut Scratch, p: &mut ParsedNl) {
    let words = scratch.word_cache.clone();
    for (s, e) in words {
        if !scratch.is_live(s, e) {
            continue;
        }
        let tok = scratch.word_orig((s, e)).to_string();
        let Some((key, value)) = tok.split_once(':') else {
            continue;
        };
        let kind = match key {
            "due" => match NaiveDate::parse_from_str(value, "%Y-%m-%d") {
                Ok(d) => {
                    p.due = Some(d);
                    FieldKind::Deadline
                }
                Err(_) => continue,
            },
            "plan" => match NaiveDate::parse_from_str(value, "%Y-%m-%d") {
                Ok(d) => {
                    p.planned = Some(d);
                    FieldKind::Date
                }
                Err(_) => continue,
            },
            "dur" => match crate::duration::parse_minutes(value) {
                Some(m) => {
                    p.duration = Some(m);
                    FieldKind::Duration
                }
                None => continue,
            },
            "remind" => match crate::duration::parse_reminders(value) {
                Some(r) => {
                    p.reminders = r;
                    FieldKind::Reminder
                }
                None => continue,
            },
            "t" if crate::threshold::parse_threshold(value).is_some() => {
                p.threshold = Some(value.to_string());
                FieldKind::ShowFrom
            }
            "rec" if crate::recurrence::parse_rec_spec(value).is_some() => {
                p.rec = Some(value.to_string());
                FieldKind::Repeat
            }
            "at" => match parse_clock(value) {
                Some(t) => {
                    p.time = Some(t);
                    FieldKind::Time
                }
                None => continue,
            },
            _ => continue,
        };
        scratch.kind = Some(kind);
        scratch.mark(s, e);
    }
}

// ---------------------------------------------------------------------------
// Time of day ("at 6pm", "at 18:30", "7am")
// ---------------------------------------------------------------------------

/// `at 6pm`, `at 6 pm`, `at 18:30`, `6:30pm`, `7am`, `noon`, and a bare
/// hour after `at`: `at 6` reads like a calendar app would — 1–6 is the
/// afternoon (18:00), 7–11 the morning, 12 noon, 13–23 as written. The chip
/// shows the guess, so a wrong one is visible and easy to fix.
fn pass_time(scratch: &mut Scratch, p: &mut ParsedNl) {
    scratch.kind = Some(FieldKind::Time);
    if p.time.is_some() {
        return;
    }
    let words = scratch.word_cache.clone();
    let live = |s: &Scratch, i: usize| words.get(i).is_some_and(|w| s.is_live(w.0, w.1));
    for i in 0..words.len() {
        if !live(scratch, i) {
            continue;
        }
        let at = scratch.word_lc(words[i]) == "at";
        let first = if at { i + 1 } else { i };
        if !live(scratch, first) {
            continue;
        }
        let w = scratch.word_lc(words[first]).to_string();
        let next = (first + 1 < words.len() && live(scratch, first + 1))
            .then(|| scratch.word_lc(words[first + 1]).to_string());
        let (time, count) = if w == "noon" || w == "midday" {
            ((12, 0), 1)
        } else if w == "midnight" {
            ((0, 0), 1)
        } else if let Some(t) = parse_clock_word(&w) {
            (t, 1)
        } else if let (Some(n), Some(suffix)) = (w.parse::<u32>().ok(), next.as_deref())
            && let Some(t) = apply_meridiem(n, 0, suffix)
        {
            (t, 2)
        } else if at && let Some(h) = w.parse::<u32>().ok().and_then(guess_hour) {
            ((h, 0), 1)
        } else {
            continue;
        };
        // Without a leading `at`, only an explicit am/pm (or noon) counts:
        // "18:30" alone could be a score or a ratio.
        let explicit =
            at || w.ends_with("am") || w.ends_with("pm") || count == 2 || !w.contains(':');
        if !explicit {
            continue;
        }
        let start = words[i].0;
        let end = words[first + count - 1].1;
        scratch.mark(start, end);
        p.time = Some(time);
        return;
    }
}

/// `6pm`, `6:30pm`, `18:30`, `7am` (one word). Bare numbers don't count.
fn parse_clock_word(w: &str) -> Option<(u32, u32)> {
    for suffix in ["am", "pm"] {
        if let Some(num) = w.strip_suffix(suffix) {
            let (h, m) = split_clock(num)?;
            return apply_meridiem(h, m, suffix);
        }
    }
    if w.contains(':') {
        let (h, m) = split_clock(w)?;
        return (h < 24).then_some((h, m));
    }
    None
}

/// The value of an `at:` tag: `18:00`, `6pm`, `6:30pm`.
fn parse_clock(v: &str) -> Option<(u32, u32)> {
    parse_clock_word(&v.to_ascii_lowercase())
}

fn split_clock(s: &str) -> Option<(u32, u32)> {
    let (h, m) = match s.split_once(':') {
        Some((h, m)) => (h.parse().ok()?, m.parse().ok()?),
        None => (s.parse().ok()?, 0),
    };
    (m < 60).then_some((h, m))
}

/// A bare hour after `at`: 1–6 → afternoon, 7–11 → morning, 12 → noon,
/// 0 and 13–23 as written.
fn guess_hour(h: u32) -> Option<u32> {
    match h {
        1..=6 => Some(h + 12),
        0 | 7..=23 => Some(h),
        _ => None,
    }
}

fn apply_meridiem(h: u32, m: u32, suffix: &str) -> Option<(u32, u32)> {
    if !(1..=12).contains(&h) {
        return None;
    }
    match suffix {
        "am" => Some((h % 12, m)),
        "pm" => Some((h % 12 + 12, m)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Pass 1: sigiled tokens (+proj, @ctx)
// ---------------------------------------------------------------------------

fn pass_sigiled(scratch: &mut Scratch, p: &mut ParsedNl) {
    let words = scratch.word_cache.clone();
    for (s, e) in words {
        if !scratch.is_live(s, e) {
            continue;
        }
        let tok = scratch.word_orig((s, e)).to_string();
        // A bare `+` / `@` is still being typed (or is just text).
        let named = |n: &str| !n.is_empty() && todo::is_valid_tag_name(n);
        if let Some(name) = tok.strip_prefix('+').filter(|n| named(n)) {
            push_unique(&mut p.projects, name);
            scratch.kind = Some(FieldKind::Project);
            scratch.mark(s, e);
        } else if let Some(name) = tok.strip_prefix('@').filter(|n| named(n)) {
            push_unique(&mut p.contexts, name);
            scratch.kind = Some(FieldKind::Context);
            scratch.mark(s, e);
        }
    }
}

fn push_unique(out: &mut Vec<String>, name: &str) {
    if name.is_empty() || !todo::is_valid_tag_name(name) {
        return;
    }
    if !out.iter().any(|x| x == name) {
        out.push(name.to_string());
    }
}

// ---------------------------------------------------------------------------
// Reminders ("remind me 15 min before") and duration ("for 1h")
// ---------------------------------------------------------------------------

/// Minutes in `n` of `unit` ("min", "hours", "day"…), when `unit` is a
/// unit of time; `days` also allows days and weeks.
fn unit_minutes(unit: &str, days: bool) -> Option<u32> {
    Some(match unit {
        "m" | "min" | "mins" | "minute" | "minutes" => 1,
        "h" | "hr" | "hrs" | "hour" | "hours" => 60,
        "d" | "day" | "days" if days => 60 * 24,
        "w" | "week" | "weeks" if days => 60 * 24 * 7,
        _ => return None,
    })
}

/// A length at `words[i]`: "15 min", "1 hour", "an hour", "half an hour",
/// or one word like "90m" / "1h30m". Returns minutes and words used.
fn length_at(
    scratch: &Scratch,
    words: &[(usize, usize)],
    i: usize,
    days: bool,
) -> Option<(u32, usize)> {
    let live = |j: usize| words.get(j).is_some_and(|r| scratch.is_live(r.0, r.1));
    if !live(i) {
        return None;
    }
    let w = scratch.word_lc(words[i]);
    if w == "half" && live(i + 1) && live(i + 2) {
        let (a, h) = (scratch.word_lc(words[i + 1]), scratch.word_lc(words[i + 2]));
        if (a == "an" || a == "a") && h == "hour" {
            return Some((30, 3));
        }
    }
    if (w == "an" || w == "a") && live(i + 1) {
        let unit = scratch.word_lc(words[i + 1]);
        if matches!(unit, "hour" | "day" | "week") {
            return unit_minutes(unit, days).map(|m| (m, 2));
        }
    }
    if let Some(n) = parse_number(w)
        && live(i + 1)
        && let Some(per) = unit_minutes(scratch.word_lc(words[i + 1]), days)
    {
        return Some((n.checked_mul(per)?, 2));
    }
    // One word: "90m", "1h30m", "2d".
    let compact = crate::duration::parse_minutes(w)?;
    let has_long_unit = w.contains('d') || w.contains('w');
    (days || !has_long_unit).then_some((compact, 1))
}

/// "remind me 15 min before", "reminder 1 day before", "alert 10 minutes
/// before", and without a reminder word "15 min before" (minutes and hours
/// only, so "3 days before" stays a show-from).
fn pass_reminder(scratch: &mut Scratch, p: &mut ParsedNl) {
    scratch.kind = Some(FieldKind::Reminder);
    if !p.reminders.is_empty() {
        return;
    }
    let words = scratch.word_cache.clone();
    let live = |s: &Scratch, j: usize| words.get(j).is_some_and(|r| s.is_live(r.0, r.1));
    for i in 0..words.len() {
        if !live(scratch, i) {
            continue;
        }
        let w = scratch.word_lc(words[i]);
        let mut at = i;
        let worded = matches!(w, "remind" | "reminder" | "alert" | "notify");
        if worded {
            at += 1;
            if live(scratch, at) && matches!(scratch.word_lc(words[at]), "me" | "us") {
                at += 1;
            }
        }
        let Some((minutes, n)) = length_at(scratch, &words, at, worded) else {
            continue;
        };
        let end = at + n;
        if !(live(scratch, end) && scratch.word_lc(words[end]) == "before") {
            continue;
        }
        scratch.mark(words[i].0, words[end].1);
        p.reminders.push(minutes);
        return;
    }
}

/// "for 1h", "for 30 min", "for half an hour", "takes 2 hours", and a
/// bare "45 min" / "1h30m". Minutes and hours only: "for 3 days" is not a
/// duration tasq tracks.
fn pass_duration(scratch: &mut Scratch, p: &mut ParsedNl) {
    scratch.kind = Some(FieldKind::Duration);
    if p.duration.is_some() {
        return;
    }
    let words = scratch.word_cache.clone();
    for i in 0..words.len() {
        if !scratch.is_live(words[i].0, words[i].1) {
            continue;
        }
        let w = scratch.word_lc(words[i]);
        let lead = matches!(w, "for" | "takes" | "lasting");
        let at = if lead { i + 1 } else { i };
        let Some((minutes, n)) = length_at(scratch, &words, at, false) else {
            continue;
        };
        // A bare number + "m" could be anything ("5 m of cable"); without
        // "for" only unmistakable units count.
        if !lead && n == 2 && matches!(scratch.word_lc(words[at + 1]), "m" | "h") {
            continue;
        }
        scratch.mark(words[i].0, words[at + n - 1].1);
        p.duration = Some(minutes);
        return;
    }
}

// ---------------------------------------------------------------------------
// Pass 2: threshold ("show N (day|week|month)s? before [the] [due [date]]")
// ---------------------------------------------------------------------------

fn pass_threshold(scratch: &mut Scratch, p: &mut ParsedNl) {
    scratch.kind = Some(FieldKind::ShowFrom);
    let words = scratch.word_cache.clone();
    let mut i = 0;
    while i + 2 < words.len() {
        if !scratch.is_live(words[i].0, words[i].1) {
            i += 1;
            continue;
        }
        let Some(n) = parse_number(scratch.word_lc(words[i])) else {
            i += 1;
            continue;
        };
        let Some(unit) = unit_char(scratch.word_lc(words[i + 1])) else {
            i += 1;
            continue;
        };
        // Only d/w/m for threshold (years are not in the t: grammar).
        if !matches!(unit, 'd' | 'w' | 'm') {
            i += 1;
            continue;
        }
        if scratch.word_lc(words[i + 2]) != "before" {
            i += 1;
            continue;
        }

        // Look backward for "show [the (todo|task|item)] [me|it]" preamble.
        let mut start_word = i;
        const PREAMBLE: &[&str] = &["show", "the", "todo", "task", "item", "me", "it"];
        let mut saw_show = false;
        while start_word > 0 {
            let w = scratch.word_lc(words[start_word - 1]);
            if PREAMBLE.contains(&w) {
                if w == "show" {
                    saw_show = true;
                }
                start_word -= 1;
            } else {
                break;
            }
        }
        if !saw_show {
            start_word = i;
        }

        // Look forward through "[the] [due] [date]".
        let mut end_word = i + 3;
        const TRAILERS: &[&str] = &["the", "due", "date"];
        while end_word < words.len() {
            let w = scratch.word_lc(words[end_word]);
            if TRAILERS.contains(&w) {
                end_word += 1;
            } else {
                break;
            }
        }

        let start_byte = words[start_word].0;
        let end_byte = words[end_word - 1].1;
        scratch.mark(start_byte, end_byte);
        p.threshold = Some(format!("-{n}{unit}"));
        return;
    }
}

fn parse_number(s: &str) -> Option<u32> {
    if let Ok(n) = s.parse::<u32>() {
        return Some(n);
    }
    word_number(s)
}

fn word_number(s: &str) -> Option<u32> {
    Some(match s {
        "one" => 1,
        "two" => 2,
        "three" => 3,
        "four" => 4,
        "five" => 5,
        "six" => 6,
        "seven" => 7,
        "eight" => 8,
        "nine" => 9,
        "ten" => 10,
        _ => return None,
    })
}

fn unit_char(s: &str) -> Option<char> {
    Some(match s {
        "day" | "days" => 'd',
        "week" | "weeks" => 'w',
        "month" | "months" => 'm',
        "year" | "years" => 'y',
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Pass 3: recurrence
// ---------------------------------------------------------------------------

fn pass_recurrence(scratch: &mut Scratch, p: &mut ParsedNl) -> Option<Weekday> {
    scratch.kind = Some(FieldKind::Repeat);
    if p.rec.is_some() {
        return None;
    }
    let words = scratch.word_cache.clone();
    for i in 0..words.len() {
        if !scratch.is_live(words[i].0, words[i].1) {
            continue;
        }
        let w = scratch.word_lc(words[i]);
        let standalone = match w {
            "daily" => Some(("+1d".to_string(), 1, None)),
            "weekly" => Some(("+1w".to_string(), 1, None)),
            "biweekly" => Some(("+2w".to_string(), 1, None)),
            "monthly" => Some(("+1m".to_string(), 1, None)),
            "yearly" | "annually" => Some(("+1y".to_string(), 1, None)),
            _ => None,
        };
        let (rec, count, wh) = if let Some(s) = standalone {
            s
        } else if w == "every" || w == "each" {
            match parse_every_phrase(scratch, &words, i) {
                Some(v) => with_on_weekday(scratch, &words, i, v),
                None => continue,
            }
        } else if let Some((mask, count)) = plural_list(scratch, &words, i) {
            // "fridays" on its own means every friday; "fridays and
            // saturdays" both.
            weekly_on(mask, count)
        } else if w == "on"
            && let Some((mask, count)) = plural_list(scratch, &words, i + 1)
        {
            // "on fridays".
            weekly_on(mask, count + 1)
        } else {
            continue;
        };
        let start_byte = words[i].0;
        let end_byte = words[i + count - 1].1;
        scratch.mark(start_byte, end_byte);
        p.rec = Some(rec);
        return wh;
    }
    None
}

/// Parse `every <...>` starting at index `i`. Returns `(rec_value, word_count, weekday_hint)`
/// on success. `word_count` includes `every` itself.
fn parse_every_phrase(
    scratch: &Scratch,
    words: &[(usize, usize)],
    i: usize,
) -> Option<(String, usize, Option<Weekday>)> {
    if i + 1 >= words.len() {
        return None;
    }
    let w1 = scratch.word_lc(words[i + 1]);

    if w1 == "weekday" {
        return Some(("+1b".to_string(), 2, None));
    }

    if w1 == "business" {
        if i + 2 < words.len() {
            let w2 = scratch.word_lc(words[i + 2]);
            if w2 == "day" || w2 == "days" {
                return Some(("+1b".to_string(), 3, None));
            }
        }
        return None;
    }

    if w1 == "other" {
        if i + 2 >= words.len() {
            return None;
        }
        let w2 = scratch.word_lc(words[i + 2]);
        // "every other monday, wednesday and friday".
        if let Some((mask, count)) = weekday_list(scratch, words, i + 2)
            && mask.count_ones() > 1
        {
            let days = crate::recurrence::format_days(mask);
            return Some((format!("+2w:{days}"), 2 + count, None));
        }
        if let Some(wd) = parse_weekday(w2) {
            return Some(("+2w".to_string(), 3, Some(wd)));
        }
        let unit = match w2 {
            "day" | "days" => 'd',
            "week" | "weeks" => 'w',
            "month" | "months" => 'm',
            "year" | "years" => 'y',
            _ => return None,
        };
        return Some((format!("+2{unit}"), 3, None));
    }

    // "every friday, saturday and sunday", "every weekend".
    if let Some((mask, count)) = weekday_list(scratch, words, i + 1)
        && mask.count_ones() > 1
    {
        let days = crate::recurrence::format_days(mask);
        return Some((format!("+1w:{days}"), 1 + count, None));
    }
    if let Some(wd) = parse_weekday(w1) {
        return Some(("+1w".to_string(), 2, Some(wd)));
    }

    if let Some(n) = parse_number(w1) {
        if i + 2 >= words.len() {
            return None;
        }
        let unit = match scratch.word_lc(words[i + 2]) {
            "day" | "days" => 'd',
            "week" | "weeks" => 'w',
            "month" | "months" => 'm',
            "year" | "years" => 'y',
            _ => return None,
        };
        return Some((format!("+{n}{unit}"), 3, None));
    }

    let unit = match w1 {
        "day" => 'd',
        "week" => 'w',
        "month" => 'm',
        "year" => 'y',
        _ => return None,
    };
    Some((format!("+1{unit}"), 2, None))
}

/// Extend a weekly `every …` phrase with a trailing "on friday(s)":
/// "every week on fridays", "every 2 weeks on monday".
fn with_on_weekday(
    scratch: &Scratch,
    words: &[(usize, usize)],
    i: usize,
    (rec, count, wh): (String, usize, Option<Weekday>),
) -> (String, usize, Option<Weekday>) {
    if wh.is_some() || !rec.ends_with('w') {
        return (rec, count, wh);
    }
    let at = i + count;
    let live = |j: usize| words.get(j).is_some_and(|r| scratch.is_live(r.0, r.1));
    if live(at)
        && scratch.word_lc(words[at]) == "on"
        && let Some((mask, n)) = weekday_list(scratch, words, at + 1)
    {
        if mask.count_ones() == 1 {
            let wd = crate::recurrence::days_in(mask)[0];
            return (rec, count + 1 + n, Some(wd));
        }
        let days = crate::recurrence::format_days(mask);
        return (format!("{rec}:{days}"), count + 1 + n, None);
    }
    (rec, count, wh)
}

/// A weekly rule on the weekdays of `mask`, as `pass_recurrence` returns it.
fn weekly_on(mask: u8, count: usize) -> (String, usize, Option<Weekday>) {
    if mask.count_ones() == 1 {
        let wd = crate::recurrence::days_in(mask)[0];
        return ("+1w".to_string(), count, Some(wd));
    }
    let days = crate::recurrence::format_days(mask);
    (format!("+1w:{days}"), count, None)
}

/// A list of weekdays starting at `words[j]`: "friday, saturday and
/// sunday", "mon,wed,fri", "weekend". Commas may stick to the words or stand
/// apart. Returns the weekday mask (`RecSpec::days`) and the words used.
fn weekday_list(scratch: &Scratch, words: &[(usize, usize)], j: usize) -> Option<(u8, usize)> {
    let mut mask = 0u8;
    let mut used = 0;
    let mut k = j;
    while let Some(r) = words.get(k) {
        let w = scratch.word_lc(*r);
        // A comma on its own joins like "and" (whatever other passes made
        // of it).
        if mask != 0 && w.chars().all(|c| c == ',') {
            k += 1;
            continue;
        }
        if !scratch.is_live(r.0, r.1) {
            break;
        }
        let mut word_mask = 0u8;
        let mut ok = true;
        let mut joiner = false;
        for part in w.split(',').filter(|p| !p.is_empty()) {
            if let Some(wd) = parse_weekday(part).or_else(|| plural_weekday(part)) {
                word_mask |= crate::recurrence::day_bit(wd);
            } else if part == "weekend" || part == "weekends" {
                word_mask |= crate::recurrence::day_bit(Weekday::Sat)
                    | crate::recurrence::day_bit(Weekday::Sun);
            } else if (part == "and" || part == "&") && mask != 0 {
                joiner = true;
            } else {
                ok = false;
                break;
            }
        }
        if !ok || (word_mask == 0 && !joiner) {
            break;
        }
        k += 1;
        if word_mask != 0 {
            mask |= word_mask;
            used = k - j;
        }
    }
    (mask != 0).then_some((mask, used))
}

/// Like [`weekday_list`], but only when it opens with a plural weekday
/// ("fridays", "mondays and thursdays"), which reads as recurring.
fn plural_list(scratch: &Scratch, words: &[(usize, usize)], j: usize) -> Option<(u8, usize)> {
    let first = words.get(j).filter(|r| scratch.is_live(r.0, r.1))?;
    let lead = scratch.word_lc(*first).split(',').next().unwrap_or("");
    plural_weekday(lead)?;
    weekday_list(scratch, words, j)
}

/// "mondays", "fridays"… — a plural weekday, which reads as recurring.
fn plural_weekday(s: &str) -> Option<Weekday> {
    let singular = s.strip_suffix('s')?;
    // Full names only: "sats"/"suns" are not weekdays.
    if singular.len() < 6 {
        return None;
    }
    parse_weekday(singular)
}

fn parse_weekday(s: &str) -> Option<Weekday> {
    Some(match s {
        "monday" | "mon" => Weekday::Mon,
        "tuesday" | "tue" | "tues" => Weekday::Tue,
        "wednesday" | "wed" => Weekday::Wed,
        "thursday" | "thu" | "thurs" => Weekday::Thu,
        "friday" | "fri" => Weekday::Fri,
        "saturday" | "sat" => Weekday::Sat,
        "sunday" | "sun" => Weekday::Sun,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Pass 4: date
// ---------------------------------------------------------------------------

fn pass_date(
    scratch: &mut Scratch,
    p: &mut ParsedNl,
    today: NaiveDate,
    weekday_hint: Option<Weekday>,
) {
    let words = scratch.word_cache.clone();
    for i in 0..words.len() {
        if p.planned.is_some() && p.due.is_some() {
            break;
        }
        if !scratch.is_live(words[i].0, words[i].1) {
            continue;
        }
        // "by friday", "due friday", "deadline friday" set the deadline;
        // any other date is when it's planned.
        let deadline = matches!(
            scratch.word_lc(words[i]),
            "by" | "due" | "deadline" | "before"
        );
        if (deadline && p.due.is_some()) || (!deadline && p.planned.is_some()) {
            continue;
        }
        if let Some((date, count)) = match_date_at(scratch, &words, i, today) {
            scratch.kind = Some(if deadline {
                FieldKind::Deadline
            } else {
                FieldKind::Date
            });
            scratch.mark(words[i].0, words[i + count - 1].1);
            if deadline {
                p.due = Some(date);
            } else {
                p.planned = Some(date);
            }
        }
    }
    if p.planned.is_some() {
        return;
    }
    // "every fri, sat and sun": planned on the first of those days.
    if let Some(first) = p.rec.as_deref().and_then(|r| first_rec_day(r, today)) {
        p.planned = Some(first);
        return;
    }
    if let Some(wd) = weekday_hint
        && let Some(d) = next_weekday(today, wd, true)
    {
        p.planned = Some(d);
    }
}

/// First day of a weekly rule on given weekdays (`+1w:fri,sat,sun`) after
/// `today`; `None` for any other rule.
pub fn first_rec_day(rec: &str, today: NaiveDate) -> Option<NaiveDate> {
    let mask = crate::recurrence::parse_rec_spec(rec)?.days;
    crate::recurrence::days_in(mask)
        .into_iter()
        .filter_map(|wd| next_weekday(today, wd, true))
        .min()
}

/// Try every supported date phrase starting at `words[i]`. Returns the
/// resolved date and the number of words to consume.
fn match_date_at(
    scratch: &Scratch,
    words: &[(usize, usize)],
    i: usize,
    today: NaiveDate,
) -> Option<(NaiveDate, usize)> {
    let w = scratch.word_lc(words[i]);

    if let Ok(d) = NaiveDate::parse_from_str(w, "%Y-%m-%d") {
        return Some((d, 1));
    }

    if w == "today" || w == "tonight" {
        return Some((today, 1));
    }
    if w == "tomorrow" {
        return Some((today.checked_add_days(Days::new(1))?, 1));
    }
    if w == "yesterday" {
        return Some((today.checked_sub_days(Days::new(1))?, 1));
    }

    // Marker words that introduce a date phrase: "due April 15", "on Friday",
    // "by the 15th", "starting Friday", "before December 5". The marker is
    // consumed along with the date so it doesn't survive into the body. Any
    // "before" still standing at this point has already been ignored by the
    // threshold pass (which would have consumed "N <unit> before [trailers]").
    if matches!(w, "starting" | "on" | "due" | "by" | "before" | "deadline")
        && let Some((d, count)) = next_alive_match(scratch, words, i + 1, today)
    {
        return Some((d, 1 + count));
    }

    if (w == "this" || w == "next")
        && i + 1 < words.len()
        && let Some(wd) = parse_weekday(scratch.word_lc(words[i + 1]))
    {
        let strict = w == "next";
        if let Some(d) = next_weekday(today, wd, strict) {
            return Some((d, 2));
        }
    }

    if let Some(wd) = parse_weekday(w)
        && let Some(d) = next_weekday(today, wd, false)
    {
        return Some((d, 1));
    }

    // "in N <unit>s?"
    if w == "in"
        && i + 2 < words.len()
        && let Some(n) = parse_number(scratch.word_lc(words[i + 1]))
    {
        let unit = scratch.word_lc(words[i + 2]);
        if let Some(d) = advance_from(today, n, unit) {
            return Some((d, 3));
        }
    }

    // "N <unit>s? from (now|today)"
    if let Some(n) = parse_number(w)
        && i + 3 < words.len()
    {
        let unit = scratch.word_lc(words[i + 1]);
        let from = scratch.word_lc(words[i + 2]);
        let nowt = scratch.word_lc(words[i + 3]);
        if from == "from"
            && (nowt == "now" || nowt == "today")
            && let Some(d) = advance_from(today, n, unit)
        {
            return Some((d, 4));
        }
    }

    // "MONTH D[ord]?(, YYYY)?"
    if let Some(month) = parse_month(w)
        && i + 1 < words.len()
        && let Some(day) = parse_day_ordinal(scratch.word_lc(words[i + 1]))
    {
        let (year, consumed) = match try_parse_year(scratch, words, i + 2) {
            Some(y) => (y, 3),
            None => (today.year(), 2),
        };
        if let Some(d) = NaiveDate::from_ymd_opt(year, month, day) {
            let rolled = if consumed == 2 && d < today {
                NaiveDate::from_ymd_opt(year + 1, month, day).unwrap_or(d)
            } else {
                d
            };
            return Some((rolled, consumed));
        }
    }

    // "D[ord] (of)? MONTH(, YYYY)?"
    if let Some(day) = parse_day_ordinal(w) {
        let mut j = i + 1;
        if j < words.len() && scratch.word_lc(words[j]) == "of" {
            j += 1;
        }
        if j < words.len()
            && let Some(month) = parse_month(scratch.word_lc(words[j]))
        {
            let (year, year_extra) = match try_parse_year(scratch, words, j + 1) {
                Some(y) => (y, 1),
                None => (today.year(), 0),
            };
            let consumed = j - i + 1 + year_extra;
            if let Some(d) = NaiveDate::from_ymd_opt(year, month, day) {
                let rolled = if year_extra == 0 && d < today {
                    NaiveDate::from_ymd_opt(year + 1, month, day).unwrap_or(d)
                } else {
                    d
                };
                return Some((rolled, consumed));
            }
        }
    }

    // "the (Nth|first|...) (of (the|next) month)?"
    if w == "the"
        && i + 1 < words.len()
        && let Some(day) = parse_day_ordinal(scratch.word_lc(words[i + 1]))
    {
        let (date, consumed) = resolve_ordinal_month_phrase(scratch, words, i + 2, today, day);
        return Some((date, 2 + consumed));
    }

    // "(first|1st) of (the|next) month"
    if (w == "first" || w == "1st") && i + 3 < words.len() {
        let w1 = scratch.word_lc(words[i + 1]);
        let w2 = scratch.word_lc(words[i + 2]);
        let w3 = scratch.word_lc(words[i + 3]);
        if w1 == "of" && (w2 == "the" || w2 == "next") && w3 == "month" {
            let next_month = w2 == "next";
            let target = if next_month {
                today.checked_add_months(Months::new(1))?
            } else {
                today
            };
            if let Some(d) = NaiveDate::from_ymd_opt(target.year(), target.month(), 1) {
                let rolled = if !next_month && d < today {
                    today
                        .checked_add_months(Months::new(1))
                        .and_then(|n| NaiveDate::from_ymd_opt(n.year(), n.month(), 1))
                        .unwrap_or(d)
                } else {
                    d
                };
                return Some((rolled, 4));
            }
        }
    }

    None
}

/// Recurse into the date matcher at `i`, skipping over any consumed words.
/// Used by the `starting`/`on` wrappers so they can prefix a real date phrase.
fn next_alive_match(
    scratch: &Scratch,
    words: &[(usize, usize)],
    mut i: usize,
    today: NaiveDate,
) -> Option<(NaiveDate, usize)> {
    while i < words.len() && !scratch.is_live(words[i].0, words[i].1) {
        i += 1;
    }
    if i >= words.len() {
        return None;
    }
    match_date_at(scratch, words, i, today)
}

fn try_parse_year(scratch: &Scratch, words: &[(usize, usize)], i: usize) -> Option<i32> {
    if i >= words.len() {
        return None;
    }
    // `word_lc` already strips trailing punctuation, which handles the
    // common "April 15, 2026" shape (the comma sticks to "15,").
    let y: i32 = scratch.word_lc(words[i]).parse().ok()?;
    if (1900..=9999).contains(&y) {
        Some(y)
    } else {
        None
    }
}

fn resolve_ordinal_month_phrase(
    scratch: &Scratch,
    words: &[(usize, usize)],
    j: usize,
    today: NaiveDate,
    day: u32,
) -> (NaiveDate, usize) {
    // After the ordinal: optional "of (the|next) month".
    let mut extra = 0;
    let mut next_month = false;
    if j < words.len() && scratch.word_lc(words[j]) == "of" {
        if j + 2 < words.len() {
            let w1 = scratch.word_lc(words[j + 1]);
            let w2 = scratch.word_lc(words[j + 2]);
            if (w1 == "the" || w1 == "next") && w2 == "month" {
                if w1 == "next" {
                    next_month = true;
                }
                extra = 3;
            }
        }
        if extra == 0 && j + 1 < words.len() && scratch.word_lc(words[j + 1]) == "month" {
            extra = 2;
        }
    }

    let target = if next_month {
        today.checked_add_months(Months::new(1)).unwrap_or(today)
    } else {
        today
    };
    let candidate = NaiveDate::from_ymd_opt(target.year(), target.month(), day);
    let resolved = match candidate {
        Some(d) if !next_month && d < today => today
            .checked_add_months(Months::new(1))
            .and_then(|n| NaiveDate::from_ymd_opt(n.year(), n.month(), day))
            .unwrap_or(d),
        Some(d) => d,
        None => today,
    };
    (resolved, extra)
}

fn advance_from(today: NaiveDate, n: u32, unit: &str) -> Option<NaiveDate> {
    let unit_char = unit_char(unit)?;
    match unit_char {
        'd' => today.checked_add_days(Days::new(u64::from(n))),
        'w' => today.checked_add_days(Days::new(u64::from(n) * 7)),
        'm' => today.checked_add_months(Months::new(n)),
        'y' => today.checked_add_months(Months::new(n.checked_mul(12)?)),
        _ => None,
    }
}

/// Next occurrence of `target` weekday. With `strict = true`, today is
/// skipped (so "every monday" on a Monday rolls forward by 7 days).
fn next_weekday(today: NaiveDate, target: Weekday, strict: bool) -> Option<NaiveDate> {
    let cur = today.weekday().num_days_from_monday();
    let tgt = target.num_days_from_monday();
    let mut diff = (tgt + 7 - cur) % 7;
    if diff == 0 && strict {
        diff = 7;
    }
    today.checked_add_days(Days::new(u64::from(diff)))
}

fn parse_month(s: &str) -> Option<u32> {
    Some(match s {
        "january" | "jan" => 1,
        "february" | "feb" => 2,
        "march" | "mar" => 3,
        "april" | "apr" => 4,
        "may" => 5,
        "june" | "jun" => 6,
        "july" | "jul" => 7,
        "august" | "aug" => 8,
        "september" | "sep" | "sept" => 9,
        "october" | "oct" => 10,
        "november" | "nov" => 11,
        "december" | "dec" => 12,
        _ => return None,
    })
}

fn parse_day_ordinal(s: &str) -> Option<u32> {
    if let Ok(n) = s.parse::<u32>() {
        if (1..=31).contains(&n) {
            return Some(n);
        }
        return None;
    }
    // "1st", "2nd", "3rd", "15th". strip_suffix matches by content, so it is
    // char-boundary safe — a word ending in a multibyte char (e.g. "дня)")
    // simply won't match an ASCII ordinal suffix instead of panicking.
    if let Some(num) = ["st", "nd", "rd", "th"]
        .iter()
        .find_map(|suf| s.strip_suffix(suf))
        && let Ok(n) = num.parse::<u32>()
        && (1..=31).contains(&n)
    {
        return Some(n);
    }
    Some(match s {
        "first" => 1,
        "second" => 2,
        "third" => 3,
        "fourth" => 4,
        "fifth" => 5,
        "sixth" => 6,
        "seventh" => 7,
        "eighth" => 8,
        "ninth" => 9,
        "tenth" => 10,
        "eleventh" => 11,
        "twelfth" => 12,
        "thirteenth" => 13,
        "fourteenth" => 14,
        "fifteenth" => 15,
        "sixteenth" => 16,
        "seventeenth" => 17,
        "eighteenth" => 18,
        "nineteenth" => 19,
        "twentieth" => 20,
        "thirtieth" => 30,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Pass 5: project / context prose
// ---------------------------------------------------------------------------

fn pass_project_context(scratch: &mut Scratch, p: &mut ParsedNl) {
    let words = scratch.word_cache.clone();
    let mut i = 0;
    while i < words.len() {
        if !scratch.is_live(words[i].0, words[i].1) {
            i += 1;
            continue;
        }
        let w = scratch.word_lc(words[i]);
        let is_project = w == "project" || w == "proj";
        let is_context = w == "context" || w == "ctx";
        if !is_project && !is_context {
            i += 1;
            continue;
        }
        // Find the next live word as the name.
        let mut name_idx = i + 1;
        while name_idx < words.len() && !scratch.is_live(words[name_idx].0, words[name_idx].1) {
            name_idx += 1;
        }
        if name_idx >= words.len() {
            i += 1;
            continue;
        }
        let name = scratch.word_orig(words[name_idx]).to_string();
        if !todo::is_valid_tag_name(&name) {
            i += 1;
            continue;
        }
        // Walk back over connector words ("and", "part", "of", "for", "in", "it's", "the").
        const CONNECTORS: &[&str] = &[
            "and", "or", "part", "of", "for", "in", "the", "it's", "its", "a", "an",
        ];
        let mut start_word = i;
        while start_word > 0 {
            let prev_range = words[start_word - 1];
            if !scratch.is_live(prev_range.0, prev_range.1) {
                break;
            }
            let prev = scratch.word_lc(prev_range);
            if CONNECTORS.contains(&prev) {
                start_word -= 1;
            } else {
                break;
            }
        }
        let end_byte = words[name_idx].1;
        scratch.kind = Some(if is_project {
            FieldKind::Project
        } else {
            FieldKind::Context
        });
        scratch.mark(words[start_word].0, end_byte);
        if is_project {
            push_unique(&mut p.projects, &name);
        } else {
            push_unique(&mut p.contexts, &name);
        }
        i = name_idx + 1;
    }
}

// ---------------------------------------------------------------------------
// Pass 6: priority words
// ---------------------------------------------------------------------------

fn pass_priority(scratch: &mut Scratch, p: &mut ParsedNl) {
    scratch.kind = Some(FieldKind::Priority);
    if p.priority.is_some() {
        return;
    }
    let words = scratch.word_cache.clone();
    for i in 0..words.len() {
        if !scratch.is_live(words[i].0, words[i].1) {
            continue;
        }
        // A todo.txt `(A)` typed anywhere, not just in front.
        let raw = &scratch.text.as_bytes()[words[i].0..words[i].1];
        if raw.len() == 3 && raw[0] == b'(' && raw[1].is_ascii_uppercase() && raw[2] == b')' {
            scratch.mark(words[i].0, words[i].1);
            p.priority = Some(raw[1] as char);
            return;
        }
        let w = scratch.word_lc(words[i]);
        let prio = match w {
            "high" | "highest" if next_lc(scratch, &words, i + 1) == Some("priority") => {
                Some(('A', 2))
            }
            "medium" | "med" if next_lc(scratch, &words, i + 1) == Some("priority") => {
                Some(('B', 2))
            }
            "low" if next_lc(scratch, &words, i + 1) == Some("priority") => Some(('C', 2)),
            "priority" => match next_lc(scratch, &words, i + 1) {
                Some("a") => Some(('A', 2)),
                Some("b") => Some(('B', 2)),
                Some("c") => Some(('C', 2)),
                Some("high") | Some("highest") => Some(('A', 2)),
                Some("medium") | Some("med") => Some(('B', 2)),
                Some("low") => Some(('C', 2)),
                _ => None,
            },
            _ => None,
        };
        if let Some((c, count)) = prio {
            scratch.mark(words[i].0, words[i + count - 1].1);
            p.priority = Some(c);
            return;
        }
    }
}

fn next_lc<'a>(scratch: &'a Scratch, words: &[(usize, usize)], i: usize) -> Option<&'a str> {
    words.get(i).map(|r| scratch.word_lc(*r))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn every_few_weekdays() {
        // 2026-10-03 is a Saturday.
        let today = d("2026-10-03");
        for (input, rec, due, body) in [
            (
                "llamar Ana every friday,saturday and sunday",
                "+1w:fri,sat,sun",
                "2026-10-04",
                "llamar Ana",
            ),
            (
                "gym every mon, wed and fri",
                "+1w:mon,wed,fri",
                "2026-10-05",
                "gym",
            ),
            (
                "gym every monday , wednesday",
                "+1w:mon,wed",
                "2026-10-05",
                "gym",
            ),
            ("hike every weekend", "+1w:sat,sun", "2026-10-04", "hike"),
            (
                "swim mondays and thursdays",
                "+1w:mon,thu",
                "2026-10-05",
                "swim",
            ),
            (
                "run every week on tuesday and friday",
                "+1w:tue,fri",
                "2026-10-06",
                "run",
            ),
            (
                "standup every 2 weeks on mon,thu",
                "+2w:mon,thu",
                "2026-10-05",
                "standup",
            ),
        ] {
            let p = detect(input, today, &[]).parsed;
            assert_eq!(p.rec.as_deref(), Some(rec), "{input}");
            assert_eq!(p.planned, Some(d(due)), "{input}");
            assert_eq!(p.body.trim(), body, "{input}");
        }
        // A single day stays the plain weekly rule.
        let p = detect("call mom every sunday and relax", today, &[]).parsed;
        assert_eq!(p.rec.as_deref(), Some("+1w"));
        assert!(p.body.contains("and relax"), "{p:?}");
    }

    #[test]
    fn planned_and_deadline_durations_and_reminders() {
        // 2026-10-03 is a Saturday.
        let today = d("2026-10-03");
        let p = detect("study topic 3 on monday by friday", today, &[]).parsed;
        assert_eq!(p.planned, Some(d("2026-10-05")));
        assert_eq!(p.due, Some(d("2026-10-09")));
        assert_eq!(p.body, "study topic 3");

        let p = detect("deadline friday submit essay", today, &[]).parsed;
        assert_eq!(p.due, Some(d("2026-10-09")));
        assert_eq!(p.planned, None);

        for (input, minutes, body) in [
            ("gym tomorrow at 7am for 1h", 60, "gym"),
            ("call for 30 min", 30, "call"),
            ("meeting for half an hour", 30, "meeting"),
            ("deep work for 2 hours", 120, "deep work"),
            ("review 45 min", 45, "review"),
            ("focus 1h30m", 90, "focus"),
            ("lunch for an hour", 60, "lunch"),
        ] {
            let p = detect(input, today, &[]).parsed;
            assert_eq!(p.duration, Some(minutes), "{input}");
            assert_eq!(p.body, body, "{input}");
        }
        // Not lengths of a task.
        for input in ["buy 2 m of cable", "for the kids", "plan for 3 days"] {
            assert_eq!(detect(input, today, &[]).parsed.duration, None, "{input}");
        }

        for (input, reminders, body) in [
            (
                "dentist at 5pm remind me 15 min before",
                vec![15],
                "dentist",
            ),
            ("exam friday reminder 1 day before", vec![1440], "exam"),
            ("call at 6pm 10 minutes before", vec![10], "call"),
            ("flight alert 2 hours before", vec![120], "flight"),
        ] {
            let p = detect(input, today, &[]).parsed;
            assert_eq!(p.reminders, reminders, "{input}");
            assert_eq!(p.body, body, "{input}");
        }
        // "3 days before" without a reminder word is still a show-from.
        let p = detect("pay rent friday show 3 days before", today, &[]).parsed;
        assert!(p.reminders.is_empty());
        assert_eq!(p.threshold.as_deref(), Some("-3d"));

        let p = detect(
            "gym monday at 7am for 1h remind me 15 min before",
            today,
            &[],
        )
        .parsed;
        assert_eq!(
            format_as_todo_txt(&p),
            "gym plan:2026-10-05 at:07:00 dur:1h remind:15m"
        );
        // And the tags read back.
        let p = detect("gym plan:2026-10-05 dur:1h30m remind:15m,1d", today, &[]).parsed;
        assert_eq!(p.planned, Some(d("2026-10-05")));
        assert_eq!(p.duration, Some(90));
        assert_eq!(p.reminders, vec![15, 1440]);
        assert_eq!(p.body, "gym");
    }

    #[test]
    fn a_todo_txt_priority_counts_anywhere() {
        let p = detect("pay rent tomorrow (A)", d("2026-10-03"), &[]).parsed;
        assert_eq!(p.priority, Some('A'));
        assert!(!p.body.contains("(A)"), "{p:?}");
        // Lower case stays text: "(a)" is how prose numbers options.
        let p = detect("pick option (a)", d("2026-10-03"), &[]).parsed;
        assert_eq!(p.priority, None);
    }

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn detection_skips_already_tokenized() {
        assert!(!looks_like_natural_language("Buy milk due:2026-05-10"));
        assert!(!looks_like_natural_language("Task rec:+1w"));
        assert!(!looks_like_natural_language("Hidden t:-3d"));
    }

    #[test]
    fn detection_skips_plain_words() {
        assert!(!looks_like_natural_language("Buy milk"));
        assert!(!looks_like_natural_language("(A) Buy milk"));
        assert!(!looks_like_natural_language("Buy milk +groceries @store"));
    }

    #[test]
    fn detection_fires_on_triggers() {
        assert!(looks_like_natural_language("Buy milk tomorrow"));
        assert!(looks_like_natural_language("Pay rent monthly"));
        assert!(looks_like_natural_language("Submit timesheet every friday"));
        assert!(looks_like_natural_language("Meeting in 3 days"));
        assert!(looks_like_natural_language("Call mom on tuesday"));
    }

    #[test]
    fn parses_user_example() {
        let today = d("2026-05-11");
        let input = "Pay rent monthly on the first of the month, show the todo 3 days before the due date. It's part of project home and context bank";
        let parsed = try_parse(input, today).unwrap();
        assert_eq!(parsed.body, "Pay rent");
        assert_eq!(parsed.planned, Some(d("2026-06-01")));
        assert_eq!(parsed.rec.as_deref(), Some("+1m"));
        assert_eq!(parsed.threshold.as_deref(), Some("-3d"));
        assert_eq!(parsed.projects, vec!["home".to_string()]);
        assert_eq!(parsed.contexts, vec!["bank".to_string()]);
        assert_eq!(parsed.priority, None);
    }

    #[test]
    fn formats_user_example_canonically() {
        let today = d("2026-05-11");
        let input = "Pay rent monthly on the first of the month, show the todo 3 days before the due date. It's part of project home and context bank";
        let parsed = try_parse(input, today).unwrap();
        let out = format_as_todo_txt(&parsed);
        assert_eq!(out, "Pay rent +home @bank plan:2026-06-01 rec:+1m t:-3d");
    }

    #[test]
    fn cyrillic_body_with_parenthetical_does_not_panic() {
        // Regression: a word like "дня)" is 7 bytes (three 2-byte Cyrillic
        // chars + ")"). parse_day_ordinal sliced at byte len-2, landing inside
        // the multibyte 'я' and panicking. The whole app crashed on save.
        let today = d("2026-05-17");
        let parsed = try_parse("Приготовить ужин (на 2 дня) today", today).unwrap();
        assert_eq!(parsed.planned, Some(today));
        assert_eq!(parsed.body, "Приготовить ужин (на 2 дня)");
    }

    #[test]
    fn parses_buy_milk_tomorrow() {
        let today = d("2026-05-11");
        let parsed = try_parse("Buy milk tomorrow", today).unwrap();
        assert_eq!(parsed.body, "Buy milk");
        assert_eq!(parsed.planned, Some(d("2026-05-12")));
        assert_eq!(parsed.rec, None);
        assert_eq!(parsed.threshold, None);
    }

    #[test]
    fn parses_call_mom_every_week_starting_friday() {
        let today = d("2026-05-11"); // Monday
        let parsed = try_parse(
            "Call mom every week starting Friday for project family",
            today,
        )
        .unwrap();
        assert_eq!(parsed.body, "Call mom");
        assert_eq!(parsed.rec.as_deref(), Some("+1w"));
        assert_eq!(parsed.planned, Some(d("2026-05-15"))); // next Friday
        assert_eq!(parsed.projects, vec!["family".to_string()]);
    }

    #[test]
    fn parses_annual_review_due_april_15() {
        let today = d("2026-05-11");
        let parsed = try_parse("Annual review due April 15 +work @office", today).unwrap();
        // "Annual" stays in body: we only treat "annually" as a recurrence
        // trigger ("Annual review" reads as an adjective). "due" is consumed
        // as the date marker so it doesn't survive into the body.
        assert_eq!(parsed.body, "Annual review");
        assert_eq!(parsed.due, Some(d("2027-04-15"))); // April 15 already past this year
        assert_eq!(parsed.projects, vec!["work".to_string()]);
        assert_eq!(parsed.contexts, vec!["office".to_string()]);
        assert_eq!(parsed.rec, None);
    }

    #[test]
    fn date_marker_words_are_consumed() {
        // "due", "by", "on", "starting", "before" preceding a date are
        // consumed alongside the date phrase — none survive in the body.
        let today = d("2026-05-11");
        for input in [
            "Pay rent due Friday",
            "Pay rent by Friday",
            "Pay rent on Friday",
            "Pay rent before Friday",
            "Pay rent starting Friday",
        ] {
            let parsed =
                try_parse(input, today).unwrap_or_else(|| panic!("no parse for {input:?}"));
            assert_eq!(parsed.body, "Pay rent", "input: {input:?}");
            // "due", "by" and "before" set the deadline; the others when
            // it's planned.
            let deadline = ["due", "by", "before"].iter().any(|m| input.contains(m));
            assert_eq!(parsed.due.is_some(), deadline, "input: {input:?}");
            assert_eq!(parsed.planned.is_some(), !deadline, "input: {input:?}");
        }
    }

    #[test]
    fn dangling_before_with_no_date_extracts_nothing() {
        // "before" alone (no following date) is a trigger but yields no
        // extraction — caller falls through and saves as plain prose.
        let today = d("2026-05-11");
        assert!(try_parse("Pay rent before payday", today).is_none());
    }

    #[test]
    fn parses_every_other_friday_show_one_day_before() {
        let today = d("2026-05-11");
        let parsed = try_parse(
            "Submit timesheet every other friday show 1 day before",
            today,
        )
        .unwrap();
        assert_eq!(parsed.body, "Submit timesheet");
        assert_eq!(parsed.rec.as_deref(), Some("+2w"));
        assert_eq!(parsed.threshold.as_deref(), Some("-1d"));
        assert_eq!(parsed.planned, Some(d("2026-05-15")));
    }

    #[test]
    fn idempotent_on_canonical_form() {
        let today = d("2026-05-11");
        let parsed = try_parse(
            "Pay rent monthly on the first, show 3 days before due, project home",
            today,
        )
        .unwrap();
        let canonical = format_as_todo_txt(&parsed);
        // Detection should refuse to re-parse the canonical form.
        assert!(!looks_like_natural_language(&canonical));
    }

    #[test]
    fn first_of_the_month_rolls_forward() {
        let today = d("2026-05-11");
        let parsed = try_parse("Pay rent on the first of the month", today).unwrap();
        assert_eq!(parsed.planned, Some(d("2026-06-01")));
    }

    #[test]
    fn every_monday_on_a_monday_picks_next_week() {
        let today = d("2026-05-11"); // Monday
        let parsed = try_parse("Standup every monday", today).unwrap();
        assert_eq!(parsed.rec.as_deref(), Some("+1w"));
        assert_eq!(parsed.planned, Some(d("2026-05-18")));
    }

    #[test]
    fn daily_standup_has_rec_no_due() {
        let today = d("2026-05-11");
        let parsed = try_parse("daily standup", today).unwrap();
        assert_eq!(parsed.body, "standup");
        assert_eq!(parsed.rec.as_deref(), Some("+1d"));
        assert_eq!(parsed.due, None);
    }

    #[test]
    fn business_day_recurrence() {
        let today = d("2026-05-11");
        let parsed = try_parse("Standup every business day", today).unwrap();
        assert_eq!(parsed.rec.as_deref(), Some("+1b"));
        assert_eq!(parsed.body, "Standup");
    }

    #[test]
    fn empty_body_falls_back_to_todo() {
        let today = d("2026-05-11");
        let parsed = try_parse("every monday", today).unwrap();
        let out = format_as_todo_txt(&parsed);
        assert!(out.starts_with("todo "));
        assert!(out.contains("rec:+1w"));
    }

    #[test]
    fn multiple_projects_collected() {
        let today = d("2026-05-11");
        let parsed = try_parse(
            "Plan offsite tomorrow for project home and project rentals",
            today,
        )
        .unwrap();
        assert_eq!(
            parsed.projects,
            vec!["home".to_string(), "rentals".to_string()]
        );
    }

    #[test]
    fn invalid_project_name_left_in_body() {
        // "project two words" has "two" as the candidate name. Valid tag name
        // (no spaces in "two"), so we'd actually consume "project two" — the
        // bare word "words" remains. This is the documented behavior.
        let today = d("2026-05-11");
        let parsed = try_parse("Refactor tomorrow project two words", today).unwrap();
        assert_eq!(parsed.projects, vec!["two".to_string()]);
        assert!(parsed.body.contains("words"));
    }

    #[test]
    fn sigiled_tokens_collected() {
        let today = d("2026-05-11");
        let parsed = try_parse("Buy milk tomorrow +groceries @store", today).unwrap();
        assert_eq!(parsed.projects, vec!["groceries".to_string()]);
        assert_eq!(parsed.contexts, vec!["store".to_string()]);
        assert_eq!(parsed.body, "Buy milk");
    }

    #[test]
    fn priority_high_priority_maps_to_a() {
        let today = d("2026-05-11");
        let parsed = try_parse("Fix bug high priority tomorrow", today).unwrap();
        assert_eq!(parsed.priority, Some('A'));
        assert_eq!(parsed.planned, Some(d("2026-05-12")));
        assert_eq!(parsed.body, "Fix bug");
    }

    #[test]
    fn leading_priority_prefix_is_recognized() {
        // "(A) " at the head of the buffer sets priority and is stripped from
        // the body. Without this pass, the body would carry the prefix and
        // format_as_todo_txt would emit "(A) (A) Buy milk ..." if the prose
        // also mentioned priority.
        let today = d("2026-05-11");
        let parsed = try_parse("(A) Buy milk tomorrow", today).unwrap();
        assert_eq!(parsed.priority, Some('A'));
        assert_eq!(parsed.body, "Buy milk");
        assert_eq!(parsed.planned, Some(d("2026-05-12")));
        assert_eq!(format_as_todo_txt(&parsed), "(A) Buy milk plan:2026-05-12");
    }

    #[test]
    fn leading_priority_does_not_double_up_with_prose() {
        // If both the prefix and a prose priority phrase are present, the
        // prefix wins and the prose pass is short-circuited so the output
        // doesn't carry two "(X) " heads.
        let today = d("2026-05-11");
        let parsed = try_parse("(B) Fix bug high priority tomorrow", today).unwrap();
        assert_eq!(parsed.priority, Some('B'));
        let out = format_as_todo_txt(&parsed);
        assert_eq!(out.matches("(B)").count(), 1);
        assert!(!out.contains("(A)"));
    }

    #[test]
    fn try_parse_returns_none_when_nothing_extracted() {
        let today = d("2026-05-11");
        // No triggers, no extraction — try_parse returns None and the caller
        // falls through to the plain save path.
        assert!(try_parse("Hello world", today).is_none());
        // Trigger fires ("every") but the recurrence phrase is unrecognizable,
        // and no other pass finds anything — extraction is still empty.
        assert!(try_parse("every gnarbax", today).is_none());
    }

    #[test]
    fn rec_values_are_recurrence_module_compatible() {
        // Cross-check: every emitted rec: value must round-trip through the
        // recurrence parser the rest of the app uses. Catches drift if either
        // parser's grammar changes.
        let today = d("2026-05-11");
        for input in [
            "every day standup",
            "weekly review",
            "every monday meeting",
            "every 3 weeks haircut",
            "every other friday",
            "every business day check inbox",
            "yearly taxes",
            "biweekly retro",
        ] {
            let parsed =
                try_parse(input, today).unwrap_or_else(|| panic!("no parse for {input:?}"));
            let rec = parsed.rec.unwrap_or_else(|| panic!("no rec for {input:?}"));
            assert!(
                crate::recurrence::parse_rec_spec(&rec).is_some(),
                "rec value {rec:?} from {input:?} failed recurrence::parse_rec_spec"
            );
        }
    }

    #[test]
    fn threshold_values_are_threshold_module_compatible() {
        let today = d("2026-05-11");
        for input in [
            "Task due tomorrow show 3 days before due",
            "Task due tomorrow 2 weeks before due",
            "Task due tomorrow show 1 month before",
        ] {
            let parsed =
                try_parse(input, today).unwrap_or_else(|| panic!("no parse for {input:?}"));
            let t = parsed
                .threshold
                .unwrap_or_else(|| panic!("no threshold for {input:?}"));
            assert!(
                crate::threshold::parse_threshold(&t).is_some(),
                "t value {t:?} from {input:?} failed threshold::parse_threshold"
            );
        }
    }

    // ---- live detection --------------------------------------------------

    fn spans_of(text: &str, det: &Detection) -> Vec<(String, FieldKind)> {
        det.spans
            .iter()
            .map(|s| (text[s.start..s.end].to_string(), s.kind))
            .collect()
    }

    #[test]
    fn detect_reports_each_phrase_with_its_kind() {
        let text = "call anna on friday at 6pm +work @calls";
        let det = detect(text, d("2026-09-21"), &[]);
        assert_eq!(
            spans_of(text, &det),
            [
                ("on friday".to_string(), FieldKind::Date),
                ("at 6pm".to_string(), FieldKind::Time),
                ("+work".to_string(), FieldKind::Project),
                ("@calls".to_string(), FieldKind::Context),
            ]
        );
        assert_eq!(det.parsed.body, "call anna");
        assert_eq!(det.parsed.planned, Some(d("2026-09-25")));
        assert_eq!(det.parsed.time, Some((18, 0)));
        assert_eq!(
            det.to_todo_txt(),
            "call anna +work @calls plan:2026-09-25 at:18:00"
        );
    }

    #[test]
    fn detect_recurrence_and_priority_phrases() {
        let text = "gym every week high priority";
        let det = detect(text, d("2026-09-21"), &[]);
        let kinds: Vec<FieldKind> = det.spans.iter().map(|s| s.kind).collect();
        assert!(kinds.contains(&FieldKind::Repeat), "{det:?}");
        assert!(kinds.contains(&FieldKind::Priority), "{det:?}");
        assert_eq!(det.parsed.body, "gym");
    }

    #[test]
    fn a_rejected_phrase_stays_in_the_body() {
        let text = "notes from friday meeting";
        let det = detect(text, d("2026-09-21"), &[]);
        assert_eq!(det.parsed.planned, Some(d("2026-09-25")));

        let rejected = [(FieldKind::Date, "friday".to_string())];
        let det = detect(text, d("2026-09-21"), &rejected);
        assert!(det.parsed.planned.is_none());
        assert!(det.is_empty());
        assert_eq!(det.parsed.body, "notes from friday meeting");
    }

    #[test]
    fn canonical_tokens_count_as_detected_fields() {
        let text = "(B) pay rent due:2026-10-01 rec:+1m t:-3d at:09:30";
        let det = detect(text, d("2026-09-21"), &[]);
        assert_eq!(det.parsed.priority, Some('B'));
        assert_eq!(det.parsed.due, Some(d("2026-10-01")));
        assert_eq!(det.parsed.rec.as_deref(), Some("+1m"));
        assert_eq!(det.parsed.time, Some((9, 30)));
        assert_eq!(det.parsed.threshold.as_deref(), Some("-3d"));
        assert_eq!(det.parsed.body, "pay rent");
        assert_eq!(
            det.to_todo_txt(),
            "(B) pay rent due:2026-10-01 rec:+1m t:-3d at:09:30"
        );
    }

    #[test]
    fn time_phrases() {
        let today = d("2026-09-21");
        for (text, want) in [
            ("standup at 9am", Some((9, 0))),
            ("call at 6 pm", Some((18, 0))),
            ("dinner at 21:15", Some((21, 15))),
            ("lunch 12:30pm", Some((12, 30))),
            ("meet at noon", Some((12, 0))),
            ("buy 7am coffee beans", Some((7, 0))),
            ("score 18:30 written down", None),
            ("meet at 7", Some((7, 0))),
            ("call ana at 6", Some((18, 0))),
            ("review at 12", Some((12, 0))),
            ("deploy at 23", Some((23, 0))),
            ("meet at 99", None),
            ("meet at home", None),
            ("table for 12pmx", None),
        ] {
            assert_eq!(detect(text, today, &[]).parsed.time, want, "{text}");
        }
    }

    #[test]
    fn plain_text_detects_nothing() {
        let det = detect("buy milk", d("2026-09-21"), &[]);
        assert!(det.is_empty());
        assert_eq!(det.to_todo_txt(), "buy milk");
    }

    #[test]
    fn weekly_on_a_weekday_and_plural_weekdays() {
        let today = d("2026-10-03"); // a Saturday
        for text in [
            "call ana every week on fridays",
            "call ana every week on friday",
            "call ana on fridays",
            "call ana fridays",
        ] {
            let det = detect(text, today, &[]);
            assert_eq!(det.parsed.rec.as_deref(), Some("+1w"), "{text}");
            assert_eq!(
                det.parsed.planned,
                Some(d("2026-10-09")),
                "next friday: {text}"
            );
            assert_eq!(det.parsed.body, "call ana", "{text}");
        }
        let det = detect("water plants every 2 weeks on monday", today, &[]);
        assert_eq!(det.parsed.rec.as_deref(), Some("+2w"));
        assert_eq!(det.parsed.planned, Some(d("2026-10-05")));
    }

    #[test]
    fn the_reported_example() {
        // "at 6" is a time, "every week on fridays" a weekly repeat.
        let det = detect("call ana at 6 every week on fridays", d("2026-10-03"), &[]);
        assert_eq!(det.parsed.time, Some((18, 0)));
        assert_eq!(det.parsed.rec.as_deref(), Some("+1w"));
        assert_eq!(det.parsed.planned, Some(d("2026-10-09")));
        assert_eq!(det.parsed.body, "call ana");
    }
}
