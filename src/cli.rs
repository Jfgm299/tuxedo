//! Shared CLI helpers used by the TUI entry point and the one-shot CLI
//! commands. Kept in the library crate so both resolve target paths the same
//! way. This module is the single place that reads the todo.txt environment
//! variables (`TODO_FILE`, `TODO_DIR`, `DONE_FILE`) — the core stays env-free.

use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};

use crate::core::Store;
use crate::sample;
use crate::todo;

/// Where the database lives: `$TASQ_DB` if set, otherwise
/// `$XDG_DATA_HOME/tasq/tasq.db` (`~/.local/share/tasq/tasq.db`).
pub fn db_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("TASQ_DB").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    Some(crate::xdg::data_home()?.join("tasq").join("tasq.db"))
}

/// The todo.txt a first run imports, found the way todo.sh (and tuxedo)
/// look for it: `$TODO_FILE`, `$TODO_DIR/todo.txt`, then `./todo.txt`.
/// Only an existing file counts.
pub fn legacy_todo_path() -> Option<PathBuf> {
    let candidates = [
        std::env::var_os("TODO_FILE").map(PathBuf::from),
        std::env::var_os("TODO_DIR").map(|d| PathBuf::from(d).join("todo.txt")),
        Some(PathBuf::from("todo.txt")),
    ];
    candidates.into_iter().flatten().find(|p| p.is_file())
}

/// What a first run imported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imported {
    pub from: PathBuf,
    pub live: usize,
    pub archived: usize,
}

/// The database store, plus what was imported if this run created it.
pub struct Opened {
    pub store: Store,
    pub imported: Option<Imported>,
}

/// Open the database (creating it if needed). When this run creates it and
/// a todo.txt is found ([`legacy_todo_path`]), its tasks and its `done.txt`
/// are imported; the files themselves are left untouched.
pub fn open_database(today: String) -> io::Result<Opened> {
    let path = db_path()
        .ok_or_else(|| io::Error::other("can't find a home directory for the database"))?;
    let fresh = !path.exists();
    let mut store = Store::open_db(path, today)?;
    let mut imported = None;
    if fresh && let Some(from) = legacy_todo_path() {
        let done = done_path(&from);
        let (live, archived) = import_files(&mut store, &from, Some(&done))?;
        imported = Some(Imported {
            from,
            live,
            archived,
        });
    }
    Ok(Opened { store, imported })
}

/// Append the tasks of `todo` (and of `done`, as archived tasks, when it
/// exists) to the database store. Returns how many of each were added.
pub fn import_files(
    store: &mut Store,
    todo: &Path,
    done: Option<&Path>,
) -> io::Result<(usize, usize)> {
    let live = todo::parse_file(&std::fs::read_to_string(todo)?);
    let archived = match done {
        Some(d) => match std::fs::read_to_string(d) {
            Ok(body) => todo::parse_file(&body),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        },
        None => Vec::new(),
    };
    let counts = (live.len(), archived.len());
    store
        .import(live, archived)
        .map_err(|e| io::Error::other(e.to_string()))?;
    Ok(counts)
}

/// Resolve the todo.txt path. Resolution order (todo.sh-compatible):
///
/// * `Some(path)` — an explicit positional CLI argument (TUI only) wins,
///   creating an empty file if it doesn't exist.
/// * `$TODO_FILE` — used verbatim if set.
/// * `$TODO_DIR/todo.txt` — if `TODO_DIR` is set.
/// * `./todo.txt` — if it exists in the current directory.
/// * Otherwise — the bundled sample in the temp dir.
///
/// For every case except the cwd/sample fallbacks the file (and any missing
/// parent directories) is created if absent, so a first run just works.
pub fn resolve_path(arg: Option<String>) -> io::Result<PathBuf> {
    if let Some(p) = arg {
        return ensure_file(PathBuf::from(p));
    }
    if let Some(f) = std::env::var_os("TODO_FILE") {
        return ensure_file(PathBuf::from(f));
    }
    if let Some(dir) = std::env::var_os("TODO_DIR") {
        return ensure_file(PathBuf::from(dir).join("todo.txt"));
    }
    let cwd_todo = PathBuf::from("todo.txt");
    if cwd_todo.is_file() {
        return Ok(cwd_todo);
    }
    sample_path()
}

/// Resolve the `done.txt` path for archiving. Honors `$DONE_FILE`; otherwise
/// the sibling `done.txt` next to the todo file (the core's default).
pub fn done_path(todo_path: &Path) -> PathBuf {
    if let Some(f) = std::env::var_os("DONE_FILE") {
        return PathBuf::from(f);
    }
    todo_path
        .parent()
        .map(|p| p.join("done.txt"))
        .unwrap_or_else(|| PathBuf::from("done.txt"))
}

/// Create `pb` (and any missing parent directories) if it doesn't exist, then
/// return it. `create_new` avoids the TOCTOU window where a concurrently-created
/// file would otherwise be truncated.
pub fn ensure_file(pb: PathBuf) -> io::Result<PathBuf> {
    if let Some(parent) = pb.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    match OpenOptions::new().write(true).create_new(true).open(&pb) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    Ok(pb)
}

/// Resolve the TUI target without the sample fallback. Mirrors
/// [`resolve_path`]'s precedence (arg → `$TODO_FILE` → `$TODO_DIR` →
/// `./todo.txt`), but when none of those apply it returns
/// [`Target::FirstRun`] so the caller can prompt instead of silently
/// opening the sample. `File` targets are created if absent, exactly like
/// `resolve_path`.
pub fn resolve_target(arg: Option<String>) -> io::Result<Target> {
    let decision = decide_target(
        arg,
        std::env::var_os("TODO_FILE"),
        std::env::var_os("TODO_DIR"),
        Path::new("todo.txt").is_file(),
    );
    match decision {
        TargetDecision::File(pb) => Ok(Target::File(ensure_file(pb)?)),
        TargetDecision::FirstRun => Ok(Target::FirstRun),
    }
}

/// What the TUI should open. `File` is a concrete path (created on resolve if
/// missing); `FirstRun` means nothing was specified and no `./todo.txt`
/// exists, so the caller should show the welcome prompt.
pub enum Target {
    File(PathBuf),
    FirstRun,
}

/// Pure decision half of [`resolve_target`]: precedence only, no filesystem
/// side effects. Split out so the precedence is unit-testable without
/// touching the process environment or cwd.
fn decide_target(
    arg: Option<String>,
    todo_file: Option<std::ffi::OsString>,
    todo_dir: Option<std::ffi::OsString>,
    cwd_todo_exists: bool,
) -> TargetDecision {
    if let Some(p) = arg {
        return TargetDecision::File(PathBuf::from(p));
    }
    if let Some(f) = todo_file {
        return TargetDecision::File(PathBuf::from(f));
    }
    if let Some(dir) = todo_dir {
        return TargetDecision::File(PathBuf::from(dir).join("todo.txt"));
    }
    if cwd_todo_exists {
        return TargetDecision::File(PathBuf::from("todo.txt"));
    }
    TargetDecision::FirstRun
}

#[derive(Debug, PartialEq, Eq)]
enum TargetDecision {
    File(PathBuf),
    FirstRun,
}

/// Write the bundled sample todo.txt to the system temp dir and return
/// its path. Also resets the sibling `done.txt` so a previous session's
/// archived rows don't leak back as duplicates.
pub fn sample_path() -> io::Result<PathBuf> {
    let dir = std::env::temp_dir();
    let pb = dir.join("tuxedo-sample.txt");
    std::fs::write(&pb, sample::TODO_RAW)?;
    match std::fs::remove_file(dir.join("done.txt")) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    Ok(pb)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn explicit_arg_wins_over_everything() {
        let d = decide_target(
            Some("custom.txt".into()),
            Some(OsString::from("/env/file.txt")),
            Some(OsString::from("/env/dir")),
            true,
        );
        assert_eq!(d, TargetDecision::File(PathBuf::from("custom.txt")));
    }

    #[test]
    fn todo_file_env_used_verbatim_when_no_arg() {
        let d = decide_target(None, Some(OsString::from("/env/file.txt")), None, true);
        assert_eq!(d, TargetDecision::File(PathBuf::from("/env/file.txt")));
    }

    #[test]
    fn todo_dir_env_appends_todo_txt() {
        let d = decide_target(None, None, Some(OsString::from("/env/dir")), true);
        assert_eq!(d, TargetDecision::File(PathBuf::from("/env/dir/todo.txt")));
    }

    #[test]
    fn existing_cwd_todo_txt_opens_directly() {
        let d = decide_target(None, None, None, true);
        assert_eq!(d, TargetDecision::File(PathBuf::from("todo.txt")));
    }

    #[test]
    fn nothing_specified_is_first_run() {
        let d = decide_target(None, None, None, false);
        assert_eq!(d, TargetDecision::FirstRun);
    }
}
