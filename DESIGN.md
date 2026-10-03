# tasq — design

> Capture your notes, plan your routines from the terminal: fast, local, and
> in your calendars.

This document is the reference for where the project is going: what tasq is,
how its data is modelled, how sync and calendars will work, what is free and
what is paid, and the order things ship in. It records decisions made so far;
open questions are listed at the end.

tasq grows out of a personal fork of [webstonehq/tuxedo](https://github.com/webstonehq/tuxedo)
(MIT), itself built on the [todo.txt](http://todotxt.org/) format. Both keep
their credit: the MIT notice stays, the README says where tasq comes from, and
todo.txt remains a first-class import/export format.

---

## 1. Product

**Who it's for.** Everyone who wants a fast, simple way to write things down
and plan their days — people who live in a terminal first, but not only them.
The terminal app is the flagship; a web app comes later for people who would
rather not open a terminal.

**Principles.**

1. **Local-first.** The app is complete without an account or a network. Your
   data lives on your machine; a server only ever carries it to your other
   devices.
2. **Fast capture.** Typing a task should take seconds: natural language,
   detected live, no forms.
3. **Nothing disappears.** No task silently vanishes from view because of a
   date. Views decide what is shown; dates never hide things on their own.
4. **Your data is yours.** Import and export everything (todo.txt, Markdown,
   iCalendar). Sync is end-to-end encrypted.
5. **Keyboard first, never keyboard only.** Every action has a key; the
   interface stays readable for people who don't know the keys yet.

**Free vs paid.**

| Free, forever | Paid (subscription) |
| --- | --- |
| The whole local app: tasks, notes, routines, views, natural-language capture, themes | Sync across devices |
| Week/month calendar views inside the app | Phone and web apps |
| Import/export: todo.txt, Markdown, `.ics` | Connected calendars: Google / Apple, written by the server, current even when your computer is off |
| Publishing a calendar feed from your own machine | |

Target price around €3–4/month; free during the beta, with founder pricing for
early users.

**Code and repositories.**

- `tasq` — the terminal app. **Public, open source** (MIT). Trust matters with
  end-to-end encryption, and an open client is the project's visibility.
- `tasq-cloud` — sync server, accounts, billing, calendar integrations.
  **Private.** Not self-hostable. The open client documents the wire format;
  security never depends on it being secret.

**Platforms.** macOS, Linux, Windows. Homebrew first (own tap), other package
managers after.

**Building in public.** A landing page with a waitlist, and a short release
every 2–3 weeks, each one with something worth a 30-second video.

---

## 2. Data model

Storage moves from todo.txt to a **local SQLite database**, the source of
truth on each device. todo.txt becomes an import/export format; the current
todo.txt files and notes migrate automatically on first launch.

### Task

| Field | Meaning |
| --- | --- |
| `id` | Stable unique id (ULID). Needed by sync, calendars and the phone. |
| `title` | One line. |
| `notes` | Markdown, any length. Edited in the built-in editor or in `$EDITOR`. |
| `status` | open / done (with completion timestamp). |
| `space` | Optional. The one folder the task lives in (see below). |
| `tags` | Any number of tags. |
| `priority` | A / B / C / none. How much it matters. |
| `starred` | Pinned to the top of its group. Ordering, not importance. |
| `planned` | Optional date: when you intend to do it ("on friday"). |
| `due` | Optional date: the deadline ("by friday"). |
| `time` | Optional time of day ("at 6pm"). Drives reminders and calendar events. |
| `duration` | Optional, only if typed ("1h", "30 min"). |
| `repeat` | Optional recurrence rule (see below). |
| `reminders` | Optional offsets before `time` ("15 min before"). |
| `created`, `updated` | Timestamps. |

Each field answers one question, which is what keeps them from overlapping:

| Field | Question |
| --- | --- |
| space | Which part of my life is this? |
| tags | How or where do I do it? |
| priority | How much does it matter? |
| starred | Do I want it at the very top? |
| planned / due | When do I do it / when is it due? |

### Spaces (folders)

Spaces form a tree, like folders:

```
Uni
├── Theory
├── Labs
└── Exams
Personal
└── Moving house
```

- A task lives in **at most one** space; it can be a top-level one ("pay
  tuition" in Uni) or a sub-space ("study topic 3" in Uni › Exams). A task in
  Exams also belongs to Uni.
- Opening a space shows its tasks **and** its sub-spaces' tasks.
- Hiding a space hides its sub-spaces too; a single sub-space can be hidden on
  its own.
- A space can be **published as a calendar** (opt-in, per space).
- Spaces are optional: a task with no space is simply "unfiled".

Tags cut across spaces: a task in Uni › Exams tagged `practical` shows up when
filtering by `practical`, wherever it lives.

**Typing a space.** Only the space's name is typed, never its path:
`in exams` (accent- and case-insensitive, prefix-matched) resolves to
**Uni › Exams**, which is what the chip shows. Ambiguous names list every
match with the path, most used first.

### Recurrence

Rules follow iCalendar's RRULE (RFC 5545), the format Google and Apple
calendars use, so they map one-to-one:

- frequency and interval — every 2 weeks;
- specific weekdays — Mon, Wed, Fri;
- **until** a date, or a number of times;

plus one mode calendars don't have: **after completion** ("water the plants 3
days after the last time").

Lists show only the next occurrence of a repeating task; calendar views show
every occurrence.

### Visibility

Dates never hide a task by themselves. Views decide:

- **Today** — overdue, planned for today, due today.
- **Upcoming** — the next days, grouped by day; tasks planned further ahead
  appear dimmed under "Later" instead of vanishing.
- **Space views** — everything in a space and its sub-spaces.
- Saved filters combine spaces, tags, dates and priority.

Inside a view, tasks sort by priority, starred first within each group.

### Notes

Notes live in the database (so they sync), and stay editable as plain
Markdown: the built-in vim-style editor, or `E` to open the note in `$EDITOR`
as a temporary `.md` file that is read back on exit.

---

## 3. Sync

- **Local-first, opt-in.** Signing in is optional and only turns sync on.
- **Operation-based.** Devices exchange changes ("task X completed"), not
  files, and merge them with a CRDT library (Automerge or Loro, both Rust), so
  concurrent edits on two devices merge instead of conflicting.
- **End-to-end encrypted.** The server stores and relays encrypted changes it
  cannot read.
- **Calendar exception.** A space published to a calendar is the one explicit
  exception: the user allows the server to read that space's tasks so it can
  write them to Google / Apple. Everything else stays encrypted.

## 4. Calendars

- **One-way:** changes in tasq go to the calendar, not back.
- **Granular:** each published space (usually a sub-space: Uni › Exams) becomes
  its own calendar. Google and Apple have no nested calendars; the tree lives
  in tasq.
- **Created automatically** when a space is published, renamed and deleted with
  it (Google Calendar API; Apple via EventKit on macOS or CalDAV — to be
  verified).
- **Events:** a task with `time` becomes a timed event (`duration`, or a short
  default); without `time`, an all-day event. Reminders become alarms.
- **Free tier:** an `.ics` feed published from your own machine. **Paid:** the
  server writes to the calendars directly.

## 5. Capture: natural language

The new-task box understands natural language **live**, while typing:

- each recognised phrase is coloured in place, and a **row of chips** below
  fills in as it's detected (date, space, tags, priority, repeat, time, …);
  empty chips stay dimmed as a hint of what can be added;
- the text stays as typed; it is converted on save;
- `Tab` moves through the chips — `x` discards a wrong detection (its words
  become plain text again), `Enter` opens that chip's picker;
- `Ctrl+Z` undoes the last detection;
- one `Enter` saves; a toast confirms ("added: call anna") with an undo, and
  the box stays open for the next task;
- "on friday" means **planned**, "by friday" means **due**.

**Languages.** The parsing engine is separate from each language's
vocabulary (weekdays, months, "tomorrow", "every", "by", "at", …), so adding a
language means adding a vocabulary, not touching the engine. English ships
first; Spanish and others follow. Several languages can be active at once.
Short common words ("a", "el", "en") only count inside a pattern, never alone.

## 6. Interface

- `tasq` opens a **home screen**: the space tree, Today / Upcoming / Week /
  Month views, notes, search, saved filters.
- Rounded boxes, theme-aware colours, a status chip per mode.
- **Icons:** plain Unicode by default (works in every terminal); Nerd Font
  icons optional. A first-run onboarding detects the terminal, shows an icon
  test, and offers to install the icon font ("Symbols Nerd Font") and add it as
  a fallback — your own font stays.
- An MCP server so AI assistants can read and add tasks, read-only by default.

## 7. Releases

Each release is usable on its own and worth a short video.

| # | Release | Content |
| --- | --- | --- |
| 1 | **Live capture** | The new-task box with live detection and chips (English), on the current code base. |
| 2 | **tasq core** | New name. SQLite storage, the task model above, automatic migration from todo.txt. |
| 3 | **Spaces and views** | Home screen, space tree, Today / Upcoming. |
| 4 | **Routines** | Full recurrence (weekdays, every N, until), week and month views. |
| 5 | **Calendar, local** | `.ics` feed from your machine, per-space calendars. |
| 6 | **Accounts and sync** | Optional sign-in, end-to-end encrypted sync — the paid plan. |
| 7 | **Phone and web** | Offline-capable web app on the same sync. |
| 8 | **Connected calendars** | Server-side Google / Apple. |
| — | Any time | MCP server, more capture languages, onboarding and fonts. |

**Parked:** inbox, file-manager view, images in notes, two-way calendar sync.

## 8. Open questions

- Phone app: web app (faster) or native later?
- Exact pricing and what the free trial looks like.
- Apple calendar creation from the server (CalDAV) — needs a test.
- Domain and handles (`tasq.sh` looks free; `tasq.com/.app/.dev/.io` are taken).
