//! SQLite storage: the local database that is the source of truth for the
//! task list and its archive. todo.txt stays an import/export format.
//!
//! Every task is a row with a stable id (a ULID). The todo.txt line is kept
//! verbatim in `raw` — it is what the app edits — and the fields it carries
//! (title, dates, priority, tags…) are stored next to it as columns, ready
//! for queries, calendar export and sync.
//!
//! Saving writes the whole list in one transaction but only touches rows
//! that changed, so `updated_at` means "last edited". Edits made by another
//! process (a second tasq, the CLI, the phone-capture server) are spotted
//! with SQLite's `data_version`, which moves whenever another connection
//! commits.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use crate::todo::{self, Task};

/// The schema version this build reads and writes.
const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS tasks (
    id          TEXT PRIMARY KEY,
    list        TEXT NOT NULL CHECK (list IN ('live', 'archive')),
    position    INTEGER NOT NULL,
    raw         TEXT NOT NULL,
    title       TEXT NOT NULL,
    done        INTEGER NOT NULL,
    done_on     TEXT,
    created_on  TEXT,
    priority    TEXT,
    starred     INTEGER NOT NULL,
    due         TEXT,
    show_from   TEXT,
    repeat      TEXT,
    time        TEXT,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS tasks_by_list ON tasks (list, position);
CREATE TABLE IF NOT EXISTS notes (
    id          TEXT PRIMARY KEY,
    path        TEXT NOT NULL UNIQUE,
    body        TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS task_tags (
    task_id TEXT NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    kind    TEXT NOT NULL CHECK (kind IN ('project', 'context')),
    name    TEXT NOT NULL,
    PRIMARY KEY (task_id, kind, name)
);
";

/// Which of the two lists a task belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum List {
    Live,
    Archive,
}

impl List {
    fn as_str(self) -> &'static str {
        match self {
            List::Live => "live",
            List::Archive => "archive",
        }
    }
}

pub struct Db {
    conn: Connection,
    path: PathBuf,
    /// `PRAGMA data_version` as of our last read or write.
    data_version: i64,
}

fn io_err(e: rusqlite::Error) -> std::io::Error {
    std::io::Error::other(e)
}

impl Db {
    /// Open (creating if needed) the database at `path`, including any
    /// missing parent directories.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).map_err(io_err)?;
        Self::init(conn, path.to_path_buf())
    }

    /// An in-memory database, for tests.
    #[cfg(test)]
    pub fn in_memory() -> Self {
        #[allow(clippy::unwrap_used)]
        Self::init(
            Connection::open_in_memory().unwrap(),
            PathBuf::from(":memory:"),
        )
        .unwrap()
    }

    fn init(conn: Connection, path: PathBuf) -> std::io::Result<Self> {
        // WAL lets the phone-capture server or the CLI write while the TUI
        // reads; the busy timeout makes concurrent writers wait, not fail.
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(io_err)?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(io_err)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(io_err)?;
        conn.execute_batch(SCHEMA).map_err(io_err)?;
        let version: Option<String> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(io_err)?;
        match version.and_then(|v| v.parse::<i64>().ok()) {
            None => {
                conn.execute(
                    "INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', ?1)",
                    [SCHEMA_VERSION.to_string()],
                )
                .map_err(io_err)?;
            }
            Some(v) if v > SCHEMA_VERSION => {
                return Err(std::io::Error::other(format!(
                    "{} was written by a newer tasq (schema {v}); update tasq to open it",
                    path.display()
                )));
            }
            Some(_) => {}
        }
        let mut db = Self {
            conn,
            path,
            data_version: 0,
        };
        db.data_version = db.read_data_version().map_err(io_err)?;
        Ok(db)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read_data_version(&self) -> rusqlite::Result<i64> {
        self.conn.query_row("PRAGMA data_version", [], |r| r.get(0))
    }

    /// True when another connection has committed since our last look.
    pub fn changed_externally(&mut self) -> std::io::Result<bool> {
        let v = self.read_data_version().map_err(io_err)?;
        if v == self.data_version {
            return Ok(false);
        }
        self.data_version = v;
        Ok(true)
    }

    /// True when the database holds no task at all, live or archived.
    pub fn is_empty(&self) -> std::io::Result<bool> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
            .map_err(io_err)?;
        Ok(n == 0)
    }

    /// The tasks of `list`, in order. Rows whose line no longer parses are
    /// skipped (it can only happen if the file was edited by hand).
    pub fn load(&self, list: List) -> std::io::Result<Vec<Task>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, raw FROM tasks WHERE list = ?1 ORDER BY position")
            .map_err(io_err)?;
        let rows = stmt
            .query_map([list.as_str()], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(io_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (id, raw) = row.map_err(io_err)?;
            if let Ok(mut task) = todo::parse_line(&raw) {
                task.id = id;
                out.push(task);
            }
        }
        Ok(out)
    }

    /// Write `live` and/or `archive` as the full contents of those lists, in
    /// one transaction. Tasks without an id get one (written back into the
    /// slice). Rows of a given list that are not in it are deleted — unless
    /// the task just moved to the other list, which is a plain update.
    pub fn save(
        &mut self,
        live: Option<&mut [Task]>,
        archive: Option<&mut [Task]>,
    ) -> std::io::Result<()> {
        let now = now_rfc3339();
        let tx = self.conn.transaction().map_err(io_err)?;
        let existing: HashMap<String, (String, String, i64)> = {
            let mut stmt = tx
                .prepare("SELECT id, list, raw, position FROM tasks")
                .map_err(io_err)?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?, r.get(3)?)))
                })
                .map_err(io_err)?;
            rows.collect::<Result<_, _>>().map_err(io_err)?
        };
        let mut kept: HashSet<String> = HashSet::new();
        let mut written: Vec<List> = Vec::new();
        for (list, tasks) in [(List::Live, live), (List::Archive, archive)] {
            let Some(tasks) = tasks else { continue };
            written.push(list);
            for (pos, task) in tasks.iter_mut().enumerate() {
                if task.id.is_empty() || kept.contains(&task.id) {
                    // A duplicate id (a copied task) gets its own.
                    task.id = new_ulid();
                }
                kept.insert(task.id.clone());
                let pos = pos as i64;
                match existing.get(&task.id) {
                    Some((l, raw, p)) if l == list.as_str() && *raw == task.raw && *p == pos => {}
                    Some((_, raw, _)) if *raw == task.raw => {
                        tx.execute(
                            "UPDATE tasks SET list = ?2, position = ?3 WHERE id = ?1",
                            params![task.id, list.as_str(), pos],
                        )
                        .map_err(io_err)?;
                    }
                    Some(_) => write_row(&tx, task, list, pos, None, &now)?,
                    None => write_row(&tx, task, list, pos, Some(&now), &now)?,
                }
            }
        }
        for (id, (l, _, _)) in &existing {
            let in_written = written.iter().any(|w| w.as_str() == l);
            if in_written && !kept.contains(id) {
                tx.execute("DELETE FROM tasks WHERE id = ?1", [id])
                    .map_err(io_err)?;
            }
        }
        tx.commit().map_err(io_err)?;
        // Our own commit doesn't move our data_version; re-read anyway so a
        // commit from elsewhere that landed just before ours isn't mistaken
        // for news later (we've just overwritten the lists with our view).
        self.data_version = self.read_data_version().map_err(io_err)?;
        Ok(())
    }
}

/// Insert or fully rewrite one task row (and its tags). `created` is set for
/// a new row; an existing row keeps its `created_at`.
fn write_row(
    tx: &rusqlite::Transaction<'_>,
    task: &Task,
    list: List,
    pos: i64,
    created: Option<&str>,
    now: &str,
) -> std::io::Result<()> {
    let time = todo::find_kv(todo::body_after_priority(&task.raw), "at");
    let priority = task.priority.map(|c| c.to_string());
    let title = title_of(&task.raw);
    match created {
        Some(created) => tx.execute(
            "INSERT INTO tasks (id, list, position, raw, title, done, done_on, created_on,
                 priority, starred, due, show_from, repeat, time, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                task.id,
                list.as_str(),
                pos,
                task.raw,
                title,
                task.done,
                task.done_date,
                task.created_date,
                priority,
                task.starred,
                task.due,
                task.threshold,
                task.rec,
                time,
                created,
                now
            ],
        ),
        None => tx.execute(
            "UPDATE tasks SET list = ?2, position = ?3, raw = ?4, title = ?5, done = ?6,
                 done_on = ?7, created_on = ?8, priority = ?9, starred = ?10, due = ?11,
                 show_from = ?12, repeat = ?13, time = ?14, updated_at = ?15
             WHERE id = ?1",
            params![
                task.id,
                list.as_str(),
                pos,
                task.raw,
                title,
                task.done,
                task.done_date,
                task.created_date,
                priority,
                task.starred,
                task.due,
                task.threshold,
                task.rec,
                time,
                now
            ],
        ),
    }
    .map_err(io_err)?;
    tx.execute("DELETE FROM task_tags WHERE task_id = ?1", [&task.id])
        .map_err(io_err)?;
    for (kind, names) in [("project", &task.projects), ("context", &task.contexts)] {
        for name in names {
            tx.execute(
                "INSERT OR IGNORE INTO task_tags (task_id, kind, name) VALUES (?1, ?2, ?3)",
                params![task.id, kind, name],
            )
            .map_err(io_err)?;
        }
    }
    Ok(())
}

/// The task's words without the todo.txt markup: no completion mark, dates,
/// priority, `+project`, `@context` or `key:value` tags.
pub fn title_of(raw: &str) -> String {
    todo::body_after_priority(raw)
        .split_whitespace()
        .filter(|tok| {
            let tag = (tok.starts_with('+') || tok.starts_with('@')) && tok.len() > 1;
            let kv = tok.split_once(':').is_some_and(|(k, v)| {
                todo::is_valid_key(k) && !v.is_empty() && !v.starts_with("//")
            });
            !tag && !kv
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Current UTC time as RFC 3339 with milliseconds.
pub(crate) fn now_rfc3339() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

/// A new ULID: 48 bits of milliseconds since the epoch, 80 random bits,
/// Crockford base32. Sorts by creation time, unique without coordination.
pub fn new_ulid() -> String {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let ms = chrono::Utc::now().timestamp_millis().max(0) as u128;
    let mut rand = [0u8; 10];
    // A failing OS RNG is not worth crashing over; the timestamp still
    // spreads ids, and duplicates are re-rolled on save.
    let _ = getrandom::getrandom(&mut rand);
    let mut n: u128 = ms << 80;
    for (i, b) in rand.iter().enumerate() {
        n |= u128::from(*b) << (8 * (9 - i));
    }
    (0..26)
        .map(|i| ALPHABET[((n >> (5 * (25 - i))) & 31) as usize] as char)
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn tasks(lines: &[&str]) -> Vec<Task> {
        lines.iter().map(|l| todo::parse_line(l).unwrap()).collect()
    }

    fn updated(db: &Db, id: &str) -> String {
        db.conn
            .query_row("SELECT updated_at FROM tasks WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap()
    }

    #[test]
    fn ulids_are_26_chars_and_distinct() {
        let a = new_ulid();
        let b = new_ulid();
        assert_eq!(a.len(), 26);
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn save_assigns_ids_and_load_round_trips() {
        let mut db = Db::in_memory();
        let mut live = tasks(&[
            "(A) Pay rent +home @bank due:2026-06-01",
            "Call anna at:18:00",
        ]);
        db.save(Some(&mut live), None).unwrap();
        assert!(live.iter().all(|t| t.id.len() == 26));
        let back = db.load(List::Live).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].id, live[0].id);
        assert_eq!(back[0].raw, live[0].raw);
        let (title, time): (String, Option<String>) = db
            .conn
            .query_row(
                "SELECT title, time FROM tasks WHERE id = ?1",
                [&live[1].id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(title, "Call anna");
        assert_eq!(time.as_deref(), Some("18:00"));
        let tags: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM task_tags", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tags, 2);
    }

    #[test]
    fn unchanged_rows_keep_their_updated_at_and_removed_rows_go() {
        let mut db = Db::in_memory();
        let mut live = tasks(&["one", "two", "three"]);
        db.save(Some(&mut live), None).unwrap();
        let before = updated(&db, &live[0].id);
        std::thread::sleep(std::time::Duration::from_millis(5));
        // Edit "two", drop "three", reorder.
        let mut edited = todo::parse_line("two, edited").unwrap();
        edited.id = live[1].id.clone();
        let mut next = vec![edited, live[0].clone()];
        db.save(Some(&mut next), None).unwrap();
        assert_eq!(updated(&db, &live[0].id), before, "only moved");
        assert_ne!(updated(&db, &live[1].id), before, "edited");
        let back = db.load(List::Live).unwrap();
        assert_eq!(
            back.iter().map(|t| t.raw.as_str()).collect::<Vec<_>>(),
            ["two, edited", "one"]
        );
    }

    #[test]
    fn archiving_moves_rows_between_lists() {
        let mut db = Db::in_memory();
        let mut live = tasks(&["x 2026-10-03 2026-10-01 done thing", "open thing"]);
        db.save(Some(&mut live), None).unwrap();
        let mut archive = vec![live[0].clone()];
        let mut remaining = vec![live[1].clone()];
        db.save(Some(&mut remaining), Some(&mut archive)).unwrap();
        assert_eq!(db.load(List::Live).unwrap().len(), 1);
        let arch = db.load(List::Archive).unwrap();
        assert_eq!(arch.len(), 1);
        assert_eq!(arch[0].id, live[0].id, "same row, now archived");
    }

    #[test]
    fn duplicate_ids_are_split() {
        let mut db = Db::in_memory();
        let mut live = tasks(&["a"]);
        db.save(Some(&mut live), None).unwrap();
        let mut twice = vec![live[0].clone(), live[0].clone()];
        db.save(Some(&mut twice), None).unwrap();
        assert_ne!(twice[0].id, twice[1].id);
        assert_eq!(db.load(List::Live).unwrap().len(), 2);
    }

    #[test]
    fn another_connection_is_noticed() {
        let dir = std::env::temp_dir().join(format!("tasq-db-test-{}", new_ulid()));
        let path = dir.join("tasq.db");
        let mut a = Db::open(&path).unwrap();
        let mut b = Db::open(&path).unwrap();
        assert!(!a.changed_externally().unwrap());
        let mut live = tasks(&["from b"]);
        b.save(Some(&mut live), None).unwrap();
        assert!(a.changed_externally().unwrap());
        assert!(!a.changed_externally().unwrap(), "only once");
        let mut mine = a.load(List::Live).unwrap();
        mine.push(todo::parse_line("from a").unwrap());
        a.save(Some(&mut mine), None).unwrap();
        assert!(!a.changed_externally().unwrap(), "own writes aren't news");
        assert_eq!(b.load(List::Live).unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn titles_drop_markup() {
        assert_eq!(
            title_of("x 2026-10-03 2026-10-01 Pay rent +home @bank due:2026-06-01 see https://x.y"),
            "Pay rent see https://x.y"
        );
        assert_eq!(
            title_of("(A) 2026-10-01 Call anna @calls at:18:00"),
            "Call anna"
        );
    }

    // ---- the Store on a database ----

    use crate::core::Store;

    fn raws(tasks: &[Task]) -> Vec<&str> {
        tasks.iter().map(|t| t.raw.as_str()).collect()
    }

    #[test]
    fn store_add_complete_archive_undo() {
        let mut s = Store::in_memory_db("2026-10-03");
        s.add_finalized("Pay rent due:2026-10-05 rec:+1m");
        s.add_finalized("Call anna");
        assert!(s.tasks().iter().all(|t| !t.id.is_empty()));
        let rent = s.tasks()[0].id.clone();

        // Completing the recurring task spawns the next one with a new id.
        s.toggle_complete(0);
        assert_eq!(s.tasks().len(), 3);
        assert_eq!(s.tasks()[0].id, rent, "completion keeps the id");
        assert!(s.tasks()[0].done);
        assert_ne!(s.tasks()[1].id, rent);
        assert!(
            s.tasks()[1].raw.contains("due:2026-11-05"),
            "{:?}",
            raws(s.tasks())
        );

        s.archive_completed();
        assert_eq!(s.tasks().len(), 2);
        assert_eq!(s.archive().tasks()[0].id, rent);
        let db = s.db.as_ref().unwrap();
        assert_eq!(db.load(List::Archive).unwrap()[0].id, rent);
        assert_eq!(db.load(List::Live).unwrap().len(), 2);

        // Undo brings it back to the live list, out of the archive.
        s.undo();
        assert_eq!(s.tasks().len(), 3);
        assert!(s.archive().tasks().is_empty());
        let db = s.db.as_ref().unwrap();
        assert!(db.load(List::Archive).unwrap().is_empty());
        assert_eq!(db.load(List::Live).unwrap().len(), 3);
    }

    #[test]
    fn store_edits_keep_ids_and_unarchive_round_trips() {
        let mut s = Store::in_memory_db("2026-10-03");
        s.add_finalized("Buy milk");
        let id = s.tasks()[0].id.clone();
        s.edit_line(0, "Buy oat milk +shop");
        s.set_priority_at(0, Some('A'));
        s.add_project(0, "home");
        assert_eq!(s.tasks()[0].id, id);
        s.toggle_complete(0);
        s.archive_completed();
        assert!(s.tasks().is_empty());
        s.unarchive(0);
        assert_eq!(s.tasks()[0].id, id);
        assert!(!s.tasks()[0].done);
        let live = s.db.as_ref().unwrap().load(List::Live).unwrap();
        assert_eq!(live[0].id, id);
        assert_eq!(live[0].raw, s.tasks()[0].raw);
    }

    #[test]
    fn store_import_and_external_reload() {
        let dir = std::env::temp_dir().join(format!("tasq-store-test-{}", new_ulid()));
        let path = dir.join("tasq.db");
        let mut a = Store::open_db(path.clone(), "2026-10-03".into()).unwrap();
        a.import(
            todo::parse_file("(A) one\ntwo\n"),
            todo::parse_file("x 2026-10-01 2026-09-30 old\n"),
        )
        .unwrap();
        let mut b = Store::open_db(path.clone(), "2026-10-03".into()).unwrap();
        assert_eq!(raws(b.tasks()), ["(A) one", "two"]);
        assert_eq!(b.archive().len(), 1);
        b.add_finalized("three");
        // `a` sees b's write and refuses to act on stale state.
        assert_eq!(a.reconcile(), crate::core::Reconcile::Reloaded);
        assert_eq!(a.tasks().len(), 3);
        assert_eq!(a.reconcile(), crate::core::Reconcile::Unchanged);
        let _ = std::fs::remove_dir_all(dir);
    }
}
