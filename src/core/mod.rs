//! Headless core: the durable task store, its persistence/I-O, and all task
//! mutations. Carries no view, input, or presentation state — operations return
//! structured [`outcome`] values rather than user-facing strings. Both the TUI
//! (`App` wraps a `Store`) and the CLI (`cmd`) drive this type.

use std::path::{Path, PathBuf};

use crate::todo::{self, Task};

mod archive;
pub mod db;
mod external;
mod history;
mod mutations;

pub mod filter;
pub mod outcome;

#[cfg(test)]
pub(crate) mod test_support;

pub use archive::Archive;
pub use history::History;
pub use outcome::{
    AddOutcome, ArchiveDeleteOutcome, ArchiveOutcome, BulkCompleteOutcome, BulkDeleteOutcome,
    CompleteOutcome, DeleteOutcome, DrainReport, EditOutcome, MoveOutcome, PriorityOutcome,
    Reconcile, RenameOutcome, StoreError, TagOutcome, UnarchiveOutcome, UndoOutcome,
};

/// The durable task store. Owns the live task list, the sibling `done.txt`
/// archive, undo history, and the on-disk reconciliation snapshot.
pub struct Store {
    pub(crate) tasks: Vec<Task>,
    pub(crate) history: History,
    pub(crate) archive: Archive,
    pub(crate) file_path: PathBuf,
    /// Snapshot of the file body the last time we read or wrote it; used by
    /// `reconcile` to detect external edits.
    pub(crate) last_disk: String,
    pub(crate) today: String,
    /// The database, when the store is backed by one; `None` for a plain
    /// todo.txt file. With a database, `file_path` is the database's path
    /// and `last_disk` is unused.
    pub(crate) db: Option<db::Db>,
}

impl Store {
    /// Construct a store, loading the archive (`done.txt`) off-thread from the
    /// sibling of `file_path`. Used by the TUI so the first frame doesn't wait
    /// on the archive read.
    pub fn new(file_path: PathBuf, body: String, today: String) -> Self {
        let archive = Archive::spawn(&file_path);
        Self::assemble(file_path, archive, body, today)
    }

    /// Like [`Store::new`] but with an explicit `done.txt` path (e.g. from a
    /// `DONE_FILE` env var that isn't a sibling of the todo file).
    pub fn new_with_done(
        file_path: PathBuf,
        done_path: PathBuf,
        body: String,
        today: String,
    ) -> Self {
        let archive = Archive::spawn_at(done_path);
        Self::assemble(file_path, archive, body, today)
    }

    /// Construct a store, loading the sibling archive synchronously (no
    /// background thread). Used by the one-shot CLI.
    pub fn open_sync(file_path: PathBuf, body: String, today: String) -> Self {
        let archive = Archive::load_sync(&file_path);
        Self::assemble(file_path, archive, body, today)
    }

    /// Like [`Store::open_sync`] but with an explicit `done.txt` path.
    pub fn open_sync_with_done(
        file_path: PathBuf,
        done_path: PathBuf,
        body: String,
        today: String,
    ) -> Self {
        let archive = Archive::load_sync_at(done_path);
        Self::assemble(file_path, archive, body, today)
    }

    fn assemble(file_path: PathBuf, archive: Archive, body: String, today: String) -> Self {
        let tasks = todo::parse_file(&body);
        Self {
            tasks,
            history: History::default(),
            archive,
            file_path,
            last_disk: body,
            today,
            db: None,
        }
    }

    /// Open the database at `path` (created if missing) with its live list
    /// and archive.
    pub fn open_db(path: PathBuf, today: String) -> std::io::Result<Self> {
        let db = db::Db::open(&path)?;
        Self::from_db(db, path, today)
    }

    fn from_db(db: db::Db, path: PathBuf, today: String) -> std::io::Result<Self> {
        let tasks = db.load(db::List::Live)?;
        let archived = db.load(db::List::Archive)?;
        Ok(Self {
            tasks,
            history: History::default(),
            archive: Archive::in_db(archived, path.clone()),
            file_path: path,
            last_disk: String::new(),
            today,
            db: Some(db),
        })
    }

    /// An in-memory database store, for tests.
    #[cfg(test)]
    pub(crate) fn in_memory_db(today: &str) -> Self {
        #[allow(clippy::unwrap_used)]
        Self::from_db(db::Db::in_memory(), PathBuf::from(":memory:"), today.into()).unwrap()
    }

    /// True when backed by a database rather than a todo.txt file.
    pub fn is_db(&self) -> bool {
        self.db.is_some()
    }

    /// Replace both lists wholesale (import), saving them in one go. Every
    /// task gets a fresh id. Only meaningful for a database store.
    pub fn import(&mut self, live: Vec<Task>, archived: Vec<Task>) -> Result<(), StoreError> {
        let mut live = live;
        let mut archived = archived;
        for t in live.iter_mut().chain(archived.iter_mut()) {
            t.id.clear();
        }
        let mut all_live = self.tasks.clone();
        all_live.extend(live);
        let mut all_archived = self.archive.tasks.clone();
        all_archived.extend(archived);
        self.save_lists(Some(&mut all_live), Some(&mut all_archived))?;
        self.tasks = all_live;
        self.archive.tasks = all_archived;
        self.history.clear();
        Ok(())
    }

    /// Database write of either list. Errors map to the live/archive kinds
    /// the file backend uses.
    pub(crate) fn save_lists(
        &mut self,
        live: Option<&mut [Task]>,
        archive: Option<&mut [Task]>,
    ) -> Result<(), StoreError> {
        let Some(db) = self.db.as_mut() else {
            return Ok(());
        };
        db.save(live, archive).map_err(StoreError::Write)
    }

    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }

    pub fn archive(&self) -> &Archive {
        &self.archive
    }

    pub fn today(&self) -> &str {
        &self.today
    }

    pub fn file_path(&self) -> &Path {
        &self.file_path
    }

    /// Cloned `raw` for the task at `abs`, or `None` if out of range.
    pub fn task_raw(&self, abs: usize) -> Option<String> {
        self.tasks.get(abs).map(|t| t.raw.clone())
    }

    /// True when at least one live task is marked done.
    pub fn has_completed(&self) -> bool {
        self.tasks.iter().any(|t| t.done)
    }

    /// Update the cached "today". Returns `true` iff the value changed, so the
    /// caller knows to recompute any date-dependent view state.
    pub fn set_today(&mut self, today: String) -> bool {
        if self.today == today {
            return false;
        }
        self.today = today;
        true
    }
}
