//! XDG Base Directory resolution shared between `config` and `theme` loaders.

use std::path::PathBuf;

/// Resolve the XDG base config directory. Per the XDG Base Directory Spec,
/// `XDG_CONFIG_HOME` MUST be an absolute path; relative values are ignored.
/// We warn once so users debugging path resolution can see why their relative
/// override didn't take effect.
pub fn config_home() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os("XDG_CONFIG_HOME")
        && !v.is_empty()
    {
        let p = PathBuf::from(&v);
        if p.is_absolute() {
            return Some(p);
        }
        eprintln!(
            "tasq: ignoring non-absolute XDG_CONFIG_HOME={:?} (per XDG spec)",
            p.display()
        );
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config"))
}

/// Resolve the XDG base data directory: `XDG_DATA_HOME` when absolute,
/// otherwise `~/.local/share` (also on macOS, like most command-line tools).
pub fn data_home() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os("XDG_DATA_HOME")
        && !v.is_empty()
    {
        let p = PathBuf::from(&v);
        if p.is_absolute() {
            return Some(p);
        }
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".local").join("share"))
}

/// First run after the rename: copy tuxedo's settings (`config.toml`,
/// `keybinds.toml`, `themes/`) from `~/.config/tuxedo` to `~/.config/tasq`,
/// unless the latter already exists. Best effort; returns the source when
/// something was copied.
pub fn migrate_config_from_tuxedo() -> Option<PathBuf> {
    let base = config_home()?;
    let (old, new) = (base.join("tuxedo"), base.join("tasq"));
    if new.exists() || !old.is_dir() {
        return None;
    }
    copy_dir(&old, &new).ok()?;
    Some(old)
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
