//! Where task notes are read and written.
//!
//! Notes are addressed by path everywhere in the app (`notes_dir/tasks/<id>/
//! name.md`), and that stays the case: with the database in use, the paths
//! under `notes_dir/tasks/` and `notes_dir/unlinked/` are names of rows in
//! its `notes` table instead of files, so they sync with everything else.
//! Any other path — a note in your own notes folder linked with `note:` —
//! is still a plain file. Without the database (a todo.txt opened
//! directly) everything is a file, as before.
//!
//! `$EDITOR` gets a real file: [`edit_in`] writes the note to a temporary
//! file, runs the editor on it and saves what comes back.

use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use rusqlite::{Connection, OptionalExtension, params};

use crate::note::{NOTES_TASKS_SUBDIR, NOTES_UNLINKED_SUBDIR};

struct NotesDb {
    conn: Connection,
    notes_dir: PathBuf,
}

static DB: OnceLock<Mutex<NotesDb>> = OnceLock::new();

fn io_err(e: rusqlite::Error) -> io::Error {
    io::Error::other(e)
}

/// Keep the notes under `notes_dir/tasks/` and `notes_dir/unlinked/` in
/// the database at `db_path` (whose schema the task store has created).
/// The first time, the existing note files there are copied in; the files
/// are left as they were. Call once, at startup.
pub fn use_database(db_path: &Path, notes_dir: &Path) -> io::Result<()> {
    let conn = Connection::open(db_path).map_err(io_err)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(io_err)?;
    let db = NotesDb {
        conn,
        notes_dir: notes_dir.to_path_buf(),
    };
    import_files_once(&db)?;
    let _ = DB.set(Mutex::new(db));
    Ok(())
}

/// The note's key in the database — its path relative to `notes_dir`,
/// with `/` separators — when it is one of the database's notes.
fn key(db: &NotesDb, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(&db.notes_dir).ok()?;
    let parts: Vec<&str> = rel
        .components()
        .map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect::<Option<_>>()?;
    let top = *parts.first()?;
    (top == NOTES_TASKS_SUBDIR || top == NOTES_UNLINKED_SUBDIR).then(|| parts.join("/"))
}

/// Run `f` on the database when `path` is one of its notes, or return
/// `None` so the caller falls back to the file system.
fn with_db<T>(path: &Path, f: impl FnOnce(&NotesDb, String) -> T) -> Option<T> {
    let db = DB.get()?.lock().ok()?;
    let k = key(&db, path)?;
    Some(f(&db, k))
}

pub fn read(path: &Path) -> io::Result<String> {
    if let Some(r) = with_db(path, |db, k| {
        db.conn
            .query_row("SELECT body FROM notes WHERE path = ?1", [k], |r| r.get(0))
            .optional()
            .map_err(io_err)
    }) {
        return r?.ok_or_else(|| io::Error::from(io::ErrorKind::NotFound));
    }
    std::fs::read_to_string(path)
}

pub fn write(path: &Path, body: &str) -> io::Result<()> {
    if let Some(r) = with_db(path, |db, k| {
        let now = crate::core::db::now_rfc3339();
        db.conn
            .execute(
                "INSERT INTO notes (id, path, body, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?4)
                 ON CONFLICT (path) DO UPDATE SET body = excluded.body,
                     updated_at = excluded.updated_at
                 WHERE notes.body != excluded.body",
                params![crate::core::db::new_ulid(), k, body, now],
            )
            .map(|_| ())
            .map_err(io_err)
    }) {
        return r;
    }
    std::fs::write(path, body)
}

pub fn exists(path: &Path) -> bool {
    with_db(path, |db, k| {
        db.conn
            .query_row("SELECT 1 FROM notes WHERE path = ?1", [k], |_| Ok(()))
            .optional()
            .ok()
            .flatten()
            .is_some()
    })
    .unwrap_or_else(|| path.exists())
}

/// A folder only exists implicitly in the database; on disk it's created.
pub fn create_dir_all(dir: &Path) -> io::Result<()> {
    if with_db(dir, |_, _| ()).is_some() {
        return Ok(());
    }
    std::fs::create_dir_all(dir)
}

/// The `.md` notes directly inside `dir`, sorted by name.
pub fn list_md(dir: &Path) -> Vec<PathBuf> {
    if let Some(found) = with_db(dir, |db, k| {
        let prefix = format!("{k}/");
        let mut stmt = match db
            .conn
            .prepare("SELECT path FROM notes WHERE substr(path, 1, ?2) = ?1")
        {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(params![prefix, prefix.len() as i64], |r| {
            r.get::<_, String>(0)
        });
        let mut out: Vec<PathBuf> = rows
            .map(|rows| rows.filter_map(Result::ok).collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .filter(|p| {
                let name = &p[prefix.len()..];
                !name.contains('/') && name.ends_with(".md")
            })
            .map(|p| db.notes_dir.join(p))
            .collect();
        out.sort();
        out
    }) {
        return found;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("md"))
        .collect();
    files.sort();
    files
}

pub fn remove(path: &Path) -> io::Result<()> {
    if let Some(r) = with_db(path, |db, k| {
        db.conn
            .execute("DELETE FROM notes WHERE path = ?1", [k])
            .map(|_| ())
            .map_err(io_err)
    }) {
        return r;
    }
    std::fs::remove_file(path)
}

/// Move a note, also between a file and the database. Within the database
/// it is a rename of the same row (its id stays).
pub fn rename(old: &Path, new: &Path) -> io::Result<()> {
    let both_db = with_db(old, |db, k_old| key(db, new).map(|k_new| (k_old, k_new))).flatten();
    if let Some((k_old, k_new)) = both_db {
        let r = with_db(old, |db, _| {
            db.conn
                .execute(
                    "UPDATE notes SET path = ?2, updated_at = ?3 WHERE path = ?1",
                    params![k_old, k_new, crate::core::db::now_rfc3339()],
                )
                .map_err(io_err)
        });
        return match r {
            Some(Ok(1)) => Ok(()),
            Some(Ok(_)) => Err(io::Error::from(io::ErrorKind::NotFound)),
            Some(Err(e)) => Err(e),
            None => Err(io::Error::other("notes database closed")),
        };
    }
    let in_db = |p: &Path| with_db(p, |_, _| ()).is_some();
    if !in_db(old) && !in_db(new) {
        if std::fs::rename(old, new).is_err() {
            std::fs::copy(old, new)?;
            std::fs::remove_file(old)?;
        }
        return Ok(());
    }
    write(new, &read(old)?)?;
    remove(old)
}

/// Run `edit` (the external editor) on a real file holding the note: the
/// note's own file, or for a database note a temporary copy whose result
/// is saved back.
pub fn edit_in<E>(path: &Path, edit: impl FnOnce(&Path) -> Result<(), E>) -> Result<(), E>
where
    E: From<io::Error>,
{
    if with_db(path, |_, _| ()).is_none() {
        return edit(path);
    }
    let body = read(path).unwrap_or_default();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "note.md".into());
    let dir = std::env::temp_dir().join(format!("tasq-{}", crate::core::db::new_ulid()));
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join(name);
    std::fs::write(&tmp, &body)?;
    let result = edit(&tmp);
    let edited = std::fs::read_to_string(&tmp);
    let _ = std::fs::remove_dir_all(&dir);
    result?;
    let edited = edited?;
    if edited != body {
        write(path, &edited)?;
    }
    Ok(())
}

/// Copy the note files under `tasks/` and `unlinked/` into an empty notes
/// table (first run on the database).
fn import_files_once(db: &NotesDb) -> io::Result<()> {
    let count: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM notes", [], |r| r.get(0))
        .map_err(io_err)?;
    let done: Option<String> = db
        .conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'notes_imported'",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(io_err)?;
    if count > 0 || done.is_some() {
        return Ok(());
    }
    let mut files = Vec::new();
    for sub in [NOTES_TASKS_SUBDIR, NOTES_UNLINKED_SUBDIR] {
        collect_md(&db.notes_dir.join(sub), &mut files);
    }
    let now = crate::core::db::now_rfc3339();
    for file in files {
        let (Some(k), Ok(body)) = (key(db, &file), std::fs::read_to_string(&file)) else {
            continue;
        };
        db.conn
            .execute(
                "INSERT OR IGNORE INTO notes (id, path, body, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?4)",
                params![crate::core::db::new_ulid(), k, body, now],
            )
            .map_err(io_err)?;
    }
    db.conn
        .execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('notes_imported', ?1)",
            [now],
        )
        .map_err(io_err)?;
    Ok(())
}

fn collect_md(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            collect_md(&path, out);
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// One test drives the process-wide database (it can be set only once):
    /// import, read/write, list, rename, unlink across the boundary, and the
    /// `$EDITOR` round trip.
    #[test]
    fn notes_live_in_the_database_once_it_is_in_use() {
        let base = std::env::temp_dir().join(format!("tasq-notes-{}", crate::core::db::new_ulid()));
        let notes = base.join("notes");
        let folder = notes.join("tasks").join("abc");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("plan.md"), "# Plan\n").unwrap();
        std::fs::write(notes.join("vault.md"), "my own note").unwrap();
        let db_path = base.join("tasq.db");
        crate::core::db::Db::open(&db_path).unwrap();
        use_database(&db_path, &notes).unwrap();

        // Imported; the file itself is untouched.
        assert_eq!(read(&folder.join("plan.md")).unwrap(), "# Plan\n");
        std::fs::remove_dir_all(&folder).unwrap();
        assert_eq!(list_md(&folder), vec![folder.join("plan.md")]);

        // New notes and edits go to the database, not to disk.
        create_dir_all(&notes.join("tasks").join("new")).unwrap();
        let fresh = notes.join("tasks").join("new").join("idea.md");
        write(&fresh, "idea").unwrap();
        assert!(exists(&fresh));
        assert!(!fresh.exists(), "no file is written");
        write(&fresh, "idea, refined").unwrap();
        assert_eq!(read(&fresh).unwrap(), "idea, refined");

        // Rename keeps the row; unlink moves it to unlinked/.
        let renamed = notes.join("tasks").join("new").join("idea2.md");
        rename(&fresh, &renamed).unwrap();
        assert!(!exists(&fresh));
        let unlinked = notes.join("unlinked").join("idea2.md");
        rename(&renamed, &unlinked).unwrap();
        assert_eq!(read(&unlinked).unwrap(), "idea, refined");
        assert!(list_md(&notes.join("tasks").join("new")).is_empty());

        // A note outside tasks/ and unlinked/ stays a file.
        assert_eq!(read(&notes.join("vault.md")).unwrap(), "my own note");

        // $EDITOR edits a temporary copy whose result is saved.
        edit_in::<io::Error>(&unlinked, |tmp| {
            assert!(tmp.is_file());
            std::fs::write(tmp, "edited outside").map_err(Into::into)
        })
        .unwrap();
        assert_eq!(read(&unlinked).unwrap(), "edited outside");

        remove(&unlinked).unwrap();
        assert!(!exists(&unlinked));
        let _ = std::fs::remove_dir_all(base);
    }
}
