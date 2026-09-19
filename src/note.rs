use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::todo::{self, Task};

/// Subdirectory of `notes_dir` under which per-task notes folders live:
/// `notes_dir/tasks/<id>/*.md`.
pub const NOTES_TASKS_SUBDIR: &str = "tasks";

/// A resolved notes folder for a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotesFolder {
    /// The stable id, e.g. from the `notes:<id>/` token.
    pub id: String,
    /// Absolute dir: `notes_dir/tasks/<id>/`.
    pub dir: PathBuf,
    /// True if the task already had a `notes:` token.
    pub existed_in_task: bool,
}

pub fn notes_dir_from_config(configured: Option<&str>) -> PathBuf {
    if let Some(value) = configured.map(str::trim).filter(|s| !s.is_empty()) {
        return expand_note_dir(value);
    }
    if let Some(value) = std::env::var_os("NOTES_DIR") {
        return PathBuf::from(value);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join("notes");
    }
    PathBuf::from("notes")
}

/// Extract the id from an existing `notes:<id>/` token in a task's raw
/// line, if any.
pub fn notes_id_from_raw(raw: &str) -> Option<String> {
    raw.split_whitespace()
        .find_map(|token| token.strip_prefix("notes:"))
        .map(|s| s.trim_matches('"'))
        .and_then(|s| s.strip_suffix('/'))
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

/// Generate a new stable id for a task's notes folder. Does not depend on
/// task title/content: 8 bytes (16 hex chars) of CSPRNG entropy from
/// `getrandom`, with an extremely unlikely fallback to a
/// time+counter-derived id if entropy is unavailable, so this never panics.
pub fn generate_notes_id() -> String {
    let mut bytes = [0u8; 8];
    if getrandom::getrandom(&mut bytes).is_ok() {
        return hex_encode(&bytes);
    }
    fallback_notes_id()
}

/// Resolve (but do not create) the notes folder for a task. If the task has
/// a `notes:<id>/` token, resolve that. Otherwise `existed_in_task` is
/// false and `id`/`dir` are a freshly generated candidate the caller may
/// use if it decides to actually create the folder.
pub fn folder_for_task(task: &Task, notes_dir: &Path) -> NotesFolder {
    if let Some(id) = notes_id_from_raw(&task.raw) {
        let dir = folder_path_for_id(notes_dir, &id);
        return NotesFolder {
            id,
            dir,
            existed_in_task: true,
        };
    }

    let id = generate_notes_id();
    let dir = folder_path_for_id(notes_dir, &id);
    NotesFolder {
        id,
        dir,
        existed_in_task: false,
    }
}

/// List the `.md` files inside a notes folder, sorted by filename. Returns
/// an empty `Vec` if the folder doesn't exist yet.
pub fn list_notes(dir: &Path) -> Vec<PathBuf> {
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

/// Move a legacy single-file note into a newly-created notes folder,
/// returning the new path inside that folder (same filename, just
/// relocated). Creates `new_dir` if needed. Does not touch the task's
/// todo.txt line; that is the caller's responsibility once it has
/// Store/App access to rewrite it.
pub fn migrate_legacy_note(old_path: &Path, new_dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(new_dir)?;
    let file_name = old_path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "old_path has no file name",
        )
    })?;
    let new_path = new_dir.join(file_name);
    if std::fs::rename(old_path, &new_path).is_err() {
        // Fall back to copy+remove, e.g. when old_path and new_dir live on
        // different filesystems/devices and rename(2) can't do it in place.
        std::fs::copy(old_path, &new_path)?;
        std::fs::remove_file(old_path)?;
    }
    Ok(new_path)
}

fn folder_path_for_id(notes_dir: &Path, id: &str) -> PathBuf {
    notes_dir.join(NOTES_TASKS_SUBDIR).join(id)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(hex_nibble(b >> 4));
        out.push(hex_nibble(b & 0xf));
    }
    out
}

fn hex_nibble(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        _ => (b'a' + (n - 10)) as char,
    }
}

static FALLBACK_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Only reached if the OS entropy source itself fails, which in practice
/// doesn't happen on supported platforms; kept so `generate_notes_id` never
/// panics or needs to return a `Result`.
fn fallback_notes_id() -> String {
    let counter = FALLBACK_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}{counter:x}")
}

pub fn note_template(task: &Task) -> String {
    let title = todo::body_only(&task.raw);
    let title = if title.is_empty() { "Task" } else { &title };
    let mut out = String::new();
    out.push_str("# ");
    out.push_str(title);
    out.push_str("\n\n");
    out.push_str("## Metadata\n\n");
    if let Some(priority) = task.priority {
        out.push_str(&format!("- Priority: {priority}\n"));
    }
    if let Some(created) = &task.created_date {
        out.push_str(&format!("- Created: {created}\n"));
    }
    if let Some(due) = &task.due {
        out.push_str(&format!("- Due: {due}\n"));
    }
    if !task.projects.is_empty() {
        out.push_str("- Projects: ");
        out.push_str(
            &task
                .projects
                .iter()
                .map(|p| format!("+{p}"))
                .collect::<Vec<_>>()
                .join(" "),
        );
        out.push('\n');
    }
    if !task.contexts.is_empty() {
        out.push_str("- Contexts: ");
        out.push_str(
            &task
                .contexts
                .iter()
                .map(|c| format!("@{c}"))
                .collect::<Vec<_>>()
                .join(" "),
        );
        out.push('\n');
    }
    for key in ["clickup", "clickup_status"] {
        if let Some(value) = kv_from_raw(&task.raw, key) {
            let label = match key {
                "clickup" => "ClickUp",
                "clickup_status" => "ClickUp status",
                _ => key,
            };
            out.push_str(&format!("- {label}: {value}\n"));
        }
    }
    if let Some(url) = task
        .raw
        .split_whitespace()
        .find(|token| token.starts_with("http://") || token.starts_with("https://"))
    {
        out.push_str(&format!("- URL: {url}\n"));
    }
    out.push_str("\n## Task\n\n```todo.txt\n");
    out.push_str(&task.raw);
    out.push_str("\n```\n\n## My notes\n\n");
    out
}

fn expand_note_dir(value: &str) -> PathBuf {
    if value == "~"
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home);
    }
    if let Some(rest) = value.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(value)
}

fn kv_from_raw(raw: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    raw.split_whitespace()
        .find_map(|token| token.strip_prefix(&prefix))
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::todo::parse_line;

    #[test]
    fn notes_dir_prefers_configured_value() {
        let dir = notes_dir_from_config(Some("/tmp/custom-notes"));
        assert_eq!(dir, PathBuf::from("/tmp/custom-notes"));
    }

    #[test]
    fn notes_dir_expands_configured_tilde() {
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let dir = notes_dir_from_config(Some("~/notes-work"));
        assert_eq!(dir, PathBuf::from(home).join("notes-work"));
    }

    #[test]
    fn template_contains_title_metadata_and_preserved_notes_section() {
        let task = parse_line(
            "(B) Flow +EstudoViabilidade @charlie @clickup due:2026-06-23 clickup:86ahz8gcg",
        )
        .unwrap();
        let rendered = note_template(&task);

        assert!(rendered.starts_with("# Flow\n"));
        assert!(rendered.contains("- Priority: B\n"));
        assert!(rendered.contains("- Due: 2026-06-23\n"));
        assert!(rendered.contains("- Projects: +EstudoViabilidade\n"));
        assert!(rendered.contains("- Contexts: @charlie @clickup\n"));
        assert!(rendered.contains("- ClickUp: 86ahz8gcg\n"));
        assert!(rendered.contains("## My notes\n\n"));
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tuxedo-test-note-{}-{}-{:?}",
            label,
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn notes_id_from_raw_finds_id_from_token() {
        let raw = "Write PR summary +work notes:a1b2c3d4/";
        assert_eq!(notes_id_from_raw(raw), Some("a1b2c3d4".to_string()));
    }

    #[test]
    fn notes_id_from_raw_returns_none_when_absent() {
        let raw = "Write PR summary +work";
        assert_eq!(notes_id_from_raw(raw), None);
    }

    #[test]
    fn folder_for_task_resolves_existing_notes_token() {
        let task = parse_line("Do thing +Proj @ctx notes:deadbeef/").unwrap();
        let folder = folder_for_task(&task, Path::new("/home/me/notes"));

        assert_eq!(folder.id, "deadbeef");
        assert_eq!(folder.dir, PathBuf::from("/home/me/notes/tasks/deadbeef"));
        assert!(folder.existed_in_task);
    }

    #[test]
    fn folder_for_task_generates_fresh_id_when_no_notes_token() {
        let task = parse_line("Do thing +Proj @ctx").unwrap();
        let folder = folder_for_task(&task, Path::new("/home/me/notes"));

        assert!(!folder.existed_in_task);
        assert!(!folder.id.is_empty());
        assert!(folder.dir.starts_with("/home/me/notes/tasks"));
        assert_eq!(
            folder.dir,
            PathBuf::from("/home/me/notes/tasks").join(&folder.id)
        );
    }

    #[test]
    fn generate_notes_id_produces_distinct_values_without_title_input() {
        let mut ids = std::collections::HashSet::new();
        for _ in 0..100 {
            ids.insert(generate_notes_id());
        }
        assert_eq!(ids.len(), 100);
    }

    #[test]
    fn list_notes_returns_only_md_files_sorted() {
        let dir = unique_temp_dir("list-notes");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("b.md"), "b").unwrap();
        std::fs::write(dir.join("a.md"), "a").unwrap();
        std::fs::write(dir.join("ignore.txt"), "x").unwrap();
        std::fs::write(dir.join("c.MD"), "c").unwrap();

        let files = list_notes(&dir);

        assert_eq!(files, vec![dir.join("a.md"), dir.join("b.md")]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_notes_on_missing_dir_returns_empty() {
        let dir = unique_temp_dir("missing-dir");
        assert!(!dir.exists());
        assert_eq!(list_notes(&dir), Vec::<PathBuf>::new());
    }

    #[test]
    fn migrate_legacy_note_moves_file_into_new_dir() {
        let base = unique_temp_dir("migrate");
        std::fs::create_dir_all(&base).unwrap();
        let old_path = base.join("legacy.md");
        std::fs::write(&old_path, "legacy content").unwrap();
        let new_dir = base.join("tasks").join("newid");
        assert!(!new_dir.exists());

        let new_path = migrate_legacy_note(&old_path, &new_dir).unwrap();

        assert_eq!(new_path, new_dir.join("legacy.md"));
        assert!(!old_path.exists());
        assert!(new_path.exists());
        assert_eq!(
            std::fs::read_to_string(&new_path).unwrap(),
            "legacy content"
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}
