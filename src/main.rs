#![warn(clippy::unwrap_used)]

use std::io;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use std::io::Write;

use tuxedo::action::{Action, RecAction};
use tuxedo::app::{
    AddOutcome, App, CalendarTarget, DialogInputMode, Mode, NoteEditorMode, NoteEditorState,
    OverlayKind, View,
};
use tuxedo::cli;
use tuxedo::config::Config;
use tuxedo::config_watcher;
use tuxedo::keybinds::{KeyBindings, ResolvedKey};
use tuxedo::theme;
use tuxedo::ui::hyperlinks;
use tuxedo::{clipboard, todo, ui, update};

const EVENT_POLL: Duration = Duration::from_millis(250);

fn main() -> Result<()> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // A recognized subcommand (possibly preceded by `-f`/`--json`) runs the
    // one-shot CLI and exits; otherwise we fall through to the TUI.
    if let Some(code) = tuxedo::cmd::run(&argv)? {
        std::process::exit(code);
    }
    let arg = argv.first().cloned();
    // `start_mode` is `Welcome` only on a true first run (no target and no
    // ./todo.txt); every other entry opens straight into Normal.
    let (path, start_mode) = match arg.as_deref() {
        Some("--help") | Some("-h") => {
            print_usage();
            return Ok(());
        }
        Some("--version") | Some("-V") => {
            println!("tuxedo {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("update") => {
            update::run()?;
            return Ok(());
        }
        Some("--sample") => (cli::sample_path()?, Mode::Normal),
        Some(s) if s.starts_with('-') => {
            eprintln!("tuxedo: unknown option: {s}");
            eprintln!("try `tuxedo --help`");
            std::process::exit(2);
        }
        _ => match cli::resolve_target(arg)? {
            cli::Target::File(p) => (p, Mode::Normal),
            // Open into the welcome prompt backed by an as-yet-uncreated
            // ./todo.txt; `handle_welcome` materializes the file the user picks.
            cli::Target::FirstRun => (std::path::PathBuf::from("todo.txt"), Mode::Welcome),
        },
    };
    // A freshly-created file is empty; otherwise read it. We accept NotFound
    // (race with deletion between resolve_path and now) as "empty file" but
    // refuse to silently swallow other IO errors — an unreadable or non-UTF-8
    // file would otherwise present as an empty editor that, on first save,
    // overwrites the user's data.
    let body = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(e).with_context(|| format!("reading {}", path.display()));
        }
    };
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let cfg = Config::load();
    let keybinds = KeyBindings::load();
    // Load user-supplied themes before constructing App, so the theme named
    // in cfg can resolve to a custom theme on the first `Prefs::from_config`.
    let theme_warnings = match theme::themes_dir() {
        Some(dir) => {
            let (user_themes, warnings) = theme::load_user_themes(&dir);
            theme::init(user_themes);
            warnings
        }
        None => {
            theme::init(Vec::new());
            Vec::new()
        }
    };
    let done = cli::done_path(&path);
    let mut app_state = App::new_with_done(path.clone(), done, body, today, cfg);
    app_state.config_path = Config::path();
    app_state.mode = start_mode;
    // Start the config hot-reload watcher.
    let config_rx = app_state
        .config_path
        .as_ref()
        .and_then(|p| config_watcher::spawn(p.clone()));
    // Surface theme-load problems on the first frame. Flash is single-line,
    // so collapse multiple warnings to a count and let the user investigate
    // their themes directory.
    match theme_warnings.len() {
        0 => {}
        1 => app_state.flash(theme_warnings.into_iter().next().expect("len==1")),
        n => app_state.flash(format!(
            "{n} theme(s) skipped — check ~/.config/tuxedo/themes/"
        )),
    }
    if std::env::var_os("TUXEDO_NO_UPDATE_CHECK").is_none() {
        app_state.set_update_check(update::spawn_check());
    }

    let terminal = ratatui::init();
    // Give the window/tab a consistent `tuxedo <path>` title across terminals
    // and operating systems, shortening long paths to fit a fixed budget.
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let title = ui::title::terminal_title(&path, home.as_deref(), ui::title::DEFAULT_BUDGET);
    let _ = crossterm::execute!(io::stdout(), crossterm::terminal::SetTitle(title));
    let result = run(terminal, &mut app_state, &keybinds, config_rx);
    ratatui::restore();
    // Clear the title on exit so the shell retitles on its next prompt rather
    // than leaving `tuxedo …` behind.
    let _ = crossterm::execute!(io::stdout(), crossterm::terminal::SetTitle(""));
    // Print the file path *after* restoring the terminal so the message
    // survives in the user's scrollback rather than being eaten by the
    // alt-screen. Read it back from the app: the welcome prompt may have
    // rebound to the sample. Skip the line if the user quit the welcome
    // prompt without choosing — no file was opened.
    if app_state.mode != Mode::Welcome {
        eprintln!("tuxedo: {}", app_state.file_path.display());
    }
    result
}

fn print_usage() {
    println!("usage: tuxedo [FILE]                 launch the TUI");
    println!("       tuxedo <command> [args]       run a one-shot command");
    println!("       tuxedo update");
    println!();
    println!("Without FILE or a command, opens ./todo.txt if present; otherwise");
    println!("prompts to create ./todo.txt here or open a sample todo.txt, in");
    println!("the interactive TUI.");
    println!();
    println!("Inside the TUI, press `s` to expose a phone-friendly capture");
    println!("endpoint on your LAN and show a QR code for it. Captures land");
    println!("in a sibling inbox.txt that the TUI merges on the next poll.");
    println!();
    println!("Commands (task numbers are 1-based file lines, as shown by `list`):");
    println!("  add, a TEXT...            add a task (natural-language dates supported)");
    println!("  append, app N TEXT...     append text to task N");
    println!("  prepend, prep N TEXT...   prepend text to task N");
    println!("  replace N TEXT...         replace task N");
    println!("  pri, p N PRIORITY         set priority A-Z on task N");
    println!("  depri, dp N...            remove priority from task N");
    println!("  done, do N...             mark task N complete");
    println!("  del, rm N [TERM]          delete task N (prompts; -f to force), or remove TERM");
    println!("  archive                   move completed tasks to done.txt");
    println!("  list, ls [TERM...]        list tasks (TERM: +project @context or text)");
    println!("  listall, lsa [TERM...]    list todo.txt and done.txt");
    println!("  listpri, lsp [PRIORITY]   list prioritized tasks");
    println!("  listproj, lsprj           list +projects");
    println!("  listcon, lsc              list @contexts");
    println!("  update                    print instructions for upgrading tuxedo");
    println!();
    println!("Options:");
    println!("  -f, --force      skip confirmation prompts (e.g. for del)");
    println!("      --json       machine-readable output for the commands above");
    println!("  -h, --help       show this message and exit");
    println!("  -V, --version    print version and exit");
    println!("      --sample     open the sample todo.txt in the TUI");
    println!();
    println!("Environment:");
    println!("  TODO_DIR     directory holding todo.txt / done.txt");
    println!("  TODO_FILE    path to the todo file (default $TODO_DIR/todo.txt)");
    println!("  DONE_FILE    path to the archive file (default sibling done.txt)");
}

fn run(
    mut terminal: DefaultTerminal,
    app: &mut App,
    keybinds: &KeyBindings,
    config_rx: Option<mpsc::Receiver<()>>,
) -> Result<()> {
    let mut dirty = true;
    while !app.should_quit {
        // Pick up midnight rollover so threshold-hidden tasks reveal
        // themselves without requiring an app restart.
        if app.refresh_today(chrono::Local::now().format("%Y-%m-%d").to_string()) {
            dirty = true;
        }
        // Drain the startup archive loader (and pick up external edits to
        // done.txt). Non-blocking: the first frame can render todo.txt
        // before the archive read completes.
        if app.poll_archive() {
            dirty = true;
        }
        // Pick up the update-check result so the status-bar indicator can
        // appear without waiting for a keystroke.
        if app.poll_update_check() {
            dirty = true;
        }
        // Poll the config hot-reload watcher. On signal, reload strictly
        // and apply the new prefs. On parse failure the old config stays
        // intact and a warning is flashed.
        if poll_config_reload(app, &config_rx) {
            dirty = true;
        }
        if dirty {
            // Extract URL runs from the completed frame before the borrow on
            // terminal ends, then write the OSC 8 overlay directly to the
            // backend writer. Doing this here (rather than inside `ui::draw`)
            // keeps cell symbols byte-identical to a plain render, so
            // ratatui's diff width calculation doesn't skip cells past the
            // URL — see `ui::hyperlinks` for the full explanation.
            let runs = {
                let frame = terminal.draw(|f| ui::draw(f, app))?;
                hyperlinks::collect(frame.buffer)
            };
            if !runs.is_empty() {
                let backend = terminal.backend_mut();
                hyperlinks::emit_overlay(backend, &runs)?;
                backend.flush()?;
            }
            dirty = false;
        }
        let timeout = next_timeout(app);
        if event::poll(timeout)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    handle_key(app, key, keybinds);
                    if let Some(path) = app.take_pending_editor_path() {
                        open_path_in_editor(&path)?;
                        terminal.clear()?;
                    }
                    dirty = true;
                }
                // A terminal resize must trigger an immediate redraw;
                // otherwise the screen stays stale until the next keystroke.
                Event::Resize(_, _) => {
                    dirty = true;
                }
                _ => {}
            }
        } else if !app.check_external_changes() {
            // Idle tick — file changed under us; reload was performed.
            dirty = true;
        }
        if app.flash_should_clear() {
            app.clear_flash();
            dirty = true;
        }
        if app.chord.should_clear() {
            app.chord.clear();
            dirty = true;
        }
    }
    Ok(())
}

/// Poll the config watcher channel. On signal, reload config strictly and
/// apply it to the app. Returns `true` when a reload was attempted (whether
/// successful or not) so the caller can trigger a redraw.
///
/// Deferred while `Mode::PickTheme` is open: its live preview
/// (`pick_theme_step`) mutates `prefs.theme_idx` in memory without saving,
/// so an unrelated reload landing mid-preview would silently clobber it
/// back to whatever's on disk. The signal stays queued in `rx` and is
/// applied on the first poll after the picker closes.
fn poll_config_reload(app: &mut App, rx: &Option<mpsc::Receiver<()>>) -> bool {
    if app.mode == Mode::PickTheme {
        return false;
    }
    let rx = match rx {
        Some(r) => r,
        None => return false,
    };
    match rx.try_recv() {
        Ok(()) => {}
        Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => return false,
    }
    let Some(ref path) = app.config_path else {
        return true;
    };
    match Config::load_strict(path) {
        Ok(new_cfg) => {
            app.reload_config(new_cfg);
            app.flash("config reloaded");
            true
        }
        Err(e) => {
            app.flash(format!("config reload failed: {e}"));
            true
        }
    }
}

fn open_path_in_editor(path: &std::path::Path) -> Result<()> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "nvim".to_string());
    ratatui::restore();
    let status = std::process::Command::new(&editor)
        .arg(path)
        .status()
        .with_context(|| format!("failed to launch editor `{editor}`"));
    ratatui::crossterm::terminal::enable_raw_mode()?;
    ratatui::crossterm::execute!(
        io::stdout(),
        ratatui::crossterm::terminal::EnterAlternateScreen
    )?;
    match status {
        Ok(_) => Ok(()),
        Err(e) => Err(e),
    }
}

fn next_timeout(app: &App) -> Duration {
    let earliest = match (app.flash_deadline(), app.chord.deadline()) {
        (Some(f), Some(c)) => Some(f.min(c)),
        (a, b) => a.or(b),
    };
    match earliest {
        Some(deadline) => deadline
            .saturating_duration_since(Instant::now())
            .min(EVENT_POLL),
        None => EVENT_POLL,
    }
}

fn is_exit_key(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('q')
        || (key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL)
}

fn handle_key(app: &mut App, key: KeyEvent, keybinds: &KeyBindings) {
    // Detect external edits before processing the key. On detection the
    // file is reloaded, the keystroke is consumed (re-press to act on the
    // new state), and the per-mutator checks become no-ops downstream.
    if !app.check_external_changes() {
        return;
    }

    // T11: while the pinned note has keyboard focus, `app.mode` stays
    // `Mode::Normal` throughout (see `src/app/pinned_note.rs`'s doc
    // comment) — it never changes, precisely so the rest of the app keeps
    // rendering normally underneath. That means the ordinary `match
    // app.mode` dispatch below *can't* tell "focus is on the pinned note"
    // apart from "focus is on the main list"; this early branch is what
    // does, routing every key to the pinned note's own Normal/Insert
    // sub-mode instead. `z`/`Z` (`Action::TogglePinFocus`/`ClosePinnedNote`)
    // are checked first so they stay reachable to exit focus (or close the
    // pin outright) — but only while the pinned note's own sub-mode is
    // Normal: in Insert sub-mode `z`/`Z` are ordinary typed characters,
    // matching how this app never lets a global Action key interrupt typing
    // in any other text-input context (Mode::Insert, Search, prompts, …).
    if app.pinned_focus {
        let editor_mode = app.pinned_note.as_ref().map(|e| e.mode());
        if editor_mode == Some(NoteEditorMode::Normal) {
            match key.code {
                KeyCode::Char('z') => {
                    app.toggle_pin_focus();
                    return;
                }
                KeyCode::Char('Z') => {
                    app.close_pinned_note();
                    return;
                }
                _ => {}
            }
        }
        handle_pinned_note_key(app, key, editor_mode);
        return;
    }

    match app.mode {
        Mode::Insert => handle_insert(app, key, keybinds),
        Mode::Search => handle_search(app, key),
        Mode::Help => handle_help(app, key),
        Mode::Settings => handle_settings(app, key),
        Mode::PromptProject
        | Mode::PromptContext
        | Mode::PromptRenameProject
        | Mode::PromptRenameContext
        | Mode::PromptSaveFilter => handle_prompt(app, key),
        Mode::PickProject | Mode::PickContext | Mode::PickSavedFilter => handle_pick(app, key),
        Mode::PickTheme => handle_pick_theme(app, key),
        Mode::CommandPalette => handle_command_palette(app, key),
        Mode::Share => handle_share(app, key),
        Mode::Notes => handle_notes(app, key),
        Mode::Welcome => handle_welcome(app, key),
        Mode::Normal | Mode::Visual => handle_normal(app, key, keybinds),
    }
}

/// First-run welcome prompt. `c` creates `./todo.txt` (the App's current
/// `file_path`) and edits it; `s` opens the bundled sample; `q`/`Esc` quits
/// without creating anything. Any other key is ignored so a stray press
/// doesn't silently pick an option.
fn handle_welcome(app: &mut App, key: KeyEvent) {
    if is_exit_key(key) {
        app.should_quit = true;
        return;
    }

    match key.code {
        KeyCode::Char('c') => match cli::ensure_file(app.file_path.clone()) {
            Ok(_) => app.mode = Mode::Normal,
            Err(e) => app.flash(format!("could not create {}: {e}", app.file_path.display())),
        },
        KeyCode::Char('s') => match cli::sample_path() {
            Ok(sample) => {
                let done = cli::done_path(&sample);
                let body = std::fs::read_to_string(&sample).unwrap_or_default();
                app.open_file(sample, done, body);
                app.mode = Mode::Normal;
            }
            Err(e) => app.flash(format!("could not open sample: {e}")),
        },
        KeyCode::Esc => app.should_quit = true,
        _ => {}
    }
}

/// Share overlay: any key dismisses, returning to Normal. The server
/// keeps running in the background; pressing `s` again re-shows the
/// same QR without rebinding.
fn handle_share(app: &mut App, _key: KeyEvent) {
    app.mode = Mode::Normal;
}

/// Notes-list popup: j/k (or the arrows) move the cursor, `n` starts an
/// inline "new note name" prompt, `r` renames the selected row (same inline
/// prompt, pre-filled), `d` opens a delete-confirmation sub-state, `u`
/// unlinks the selected row, `e`/`i` open the selected row into the embedded
/// editor (Normal/Insert sub-mode respectively — see
/// `App::open_note_editor_normal`/`_insert`), Esc closes back to Normal.
/// While the editor is open (`app.notes_popup.active_editor.is_some()`),
/// this function routes to `handle_note_editor` instead — none of the list
/// keys below (including `n`/`r`/`d`/`u`) reach the list while a file is
/// being edited. MVP scope deliberately skips a `gg`/`G` chord here:
/// `app.chord` is a single shared leader-state machine already loaded with
/// Normal-mode meanings (`dd`, `yy`, `gg`, `fp`/`fc`/`ff`), and this is a
/// small list with no urgent need for jump-to-top/bottom, so plain up/down
/// keeps this slice small.
///
/// `n`/`r`/`d`/`u`/`e`/`i` reuse mnemonics from elsewhere in the app (`n`
/// from the main list's `BeginAdd`, `d` from `dd`'s delete, `e`/`i` from
/// `BeginEdit`/`BeginEditInsert`), but this popup owns its own keyspace (per
/// the doc comment on `src/keybinds.rs`) and never goes through
/// `Action`/`KeyBindings` — none of these bindings can collide with the
/// global versions.
fn handle_notes(app: &mut App, key: KeyEvent) {
    if app.notes_popup.active_editor.is_some() {
        // T11: `z` pins the note currently open in the floating editor (the
        // only place pinning can start from — see `App::toggle_pin_focus`'s
        // doc comment). Only intercepted from the editor's own Normal
        // sub-mode, same restriction as everywhere else `z`/`Z` are checked
        // in this file: in Insert sub-mode `z` is an ordinary typed
        // character, not a request to pin.
        if key.code == KeyCode::Char('z')
            && app.notes_popup.active_editor.as_ref().map(|e| e.mode())
                == Some(NoteEditorMode::Normal)
        {
            app.toggle_pin_focus();
            return;
        }
        handle_note_editor(app, key);
        return;
    }
    if app.notes_popup.prompt.is_some() {
        handle_notes_prompt(app, key);
        return;
    }
    if app.notes_popup.pending_delete.is_some() {
        handle_notes_delete_confirm(app, key);
        return;
    }
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => app.notes_popup.move_down(),
        KeyCode::Char('k') | KeyCode::Up => app.notes_popup.move_up(),
        KeyCode::Char('n') => app.begin_new_note_prompt(),
        KeyCode::Char('r') => app.begin_rename_prompt(),
        KeyCode::Char('d') => app.begin_delete_note_confirm(),
        KeyCode::Char('u') => app.unlink_selected_note(),
        KeyCode::Char('e') => app.open_note_editor_normal(),
        KeyCode::Char('i') => app.open_note_editor_insert(),
        KeyCode::Esc => app.mode = Mode::Normal,
        _ => {}
    }
}

/// What happened when a key was applied to a `NoteEditorState` by
/// [`handle_note_editor_normal`]/[`handle_note_editor_insert`]. Both the
/// floating popup's editor (`handle_note_editor`) and T11's pinned/docked
/// editor (`handle_pinned_note_key`) call the exact same two functions —
/// this is how each caller learns what, if anything, it needs to do at its
/// own level (flash a save error, or react to Esc stepping "out" one layer,
/// which means something different in each context).
#[derive(Debug, Clone, PartialEq, Eq)]
enum NoteEditorSignal {
    /// The key was fully handled inside the editor; nothing more to do.
    Handled,
    /// `Ctrl+S` was pressed and `NoteEditorState::save` failed; the caller
    /// (which owns `App` and can call `app.flash`) reports it.
    SaveFailed(String),
    /// Esc was pressed in the editor's Normal sub-mode — "step back out one
    /// layer". What that means depends on the caller: the floating popup
    /// closes the editor back to the notes list; the pinned note drops
    /// keyboard focus back to the main app (it has no list to step back to).
    Esc,
}

/// The embedded note editor nested inside `Mode::Notes` (see
/// `NotesPopupState::active_editor`, `src/app/note_editor.rs`). Dispatches on
/// the editor's own Normal/Insert sub-mode, mirroring how `Mode::Insert`
/// dispatches on `DialogInputMode` elsewhere in this file.
fn handle_note_editor(app: &mut App, key: KeyEvent) {
    let Some(editor) = app.notes_popup.active_editor.as_mut() else {
        return;
    };
    let signal = match editor.mode() {
        NoteEditorMode::Normal => handle_note_editor_normal(editor, key),
        NoteEditorMode::Insert => handle_note_editor_insert(editor, key),
    };
    match signal {
        NoteEditorSignal::Handled => {}
        NoteEditorSignal::SaveFailed(msg) => app.flash(msg),
        // First Esc: back to the notes list, `active_editor` becomes `None`,
        // `Mode::Notes` itself untouched. A *second* Esc from there (now the
        // bare list, handled by `handle_notes` above) is what closes the
        // whole popup to `Mode::Normal`.
        NoteEditorSignal::Esc => app.close_note_editor(),
    }
}

/// T11: the pinned/docked note while it has keyboard focus
/// (`app.pinned_focus`), routed here by the early branch in `handle_key`
/// instead of the `Mode::Notes` path above — reuses the exact same
/// `handle_note_editor_normal`/`_insert` functions the floating popup uses,
/// operating on `app.pinned_note` instead of
/// `app.notes_popup.active_editor`. `editor_mode` is passed in by the caller
/// (which already read it to decide whether `z`/`Z` should be intercepted as
/// global actions ahead of this call) rather than re-reading it here.
fn handle_pinned_note_key(app: &mut App, key: KeyEvent, editor_mode: Option<NoteEditorMode>) {
    let Some(mode) = editor_mode else {
        return;
    };
    let Some(editor) = app.pinned_note.as_mut() else {
        return;
    };
    let signal = match mode {
        NoteEditorMode::Normal => handle_note_editor_normal(editor, key),
        NoteEditorMode::Insert => handle_note_editor_insert(editor, key),
    };
    match signal {
        NoteEditorSignal::Handled => {}
        NoteEditorSignal::SaveFailed(msg) => app.flash(msg),
        // The pinned note has no list to step back to — Esc from its Normal
        // sub-mode "steps back out" the same way `z` does: focus returns to
        // the main app, but the note stays pinned and visible.
        NoteEditorSignal::Esc => app.toggle_pin_focus(),
    }
}

/// Normal sub-mode of the embedded note editor: `hjkl`/arrows move the
/// cursor, `i` enters Insert, `Ctrl+S` saves (see the module doc on
/// `src/app/note_editor.rs` for why this key was chosen over a
/// `:`-command-line this codebase doesn't have), Esc reports
/// [`NoteEditorSignal::Esc`] for the caller to interpret. Operates directly
/// on `&mut NoteEditorState` (not through `App`'s `note_editor_*`
/// delegators) so this exact function serves both the floating popup's
/// editor and T11's pinned one — see [`NoteEditorSignal`]'s doc comment.
fn handle_note_editor_normal(editor: &mut NoteEditorState, key: KeyEvent) -> NoteEditorSignal {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
        return match editor.save() {
            Ok(()) => NoteEditorSignal::Handled,
            Err(e) => NoteEditorSignal::SaveFailed(format!("note save failed: {e}")),
        };
    }
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => editor.move_down(),
        KeyCode::Char('k') | KeyCode::Up => editor.move_up(),
        KeyCode::Char('h') | KeyCode::Left => editor.move_left(),
        KeyCode::Char('l') | KeyCode::Right => editor.move_right(),
        KeyCode::Char('i') => editor.enter_insert(),
        KeyCode::Esc => return NoteEditorSignal::Esc,
        _ => {}
    }
    NoteEditorSignal::Handled
}

/// Insert sub-mode of the embedded note editor: characters type in at the
/// cursor, Enter splits the line, Backspace deletes (joining with the
/// previous line at column 0), `Ctrl+S` saves, Esc returns to the editor's
/// own Normal sub-mode — it does not leave the editor (see
/// `NoteEditorState::esc_to_normal`), so unlike the Normal sub-mode's Esc
/// this always reports `Handled`, never `Esc`. Same `&mut NoteEditorState`
/// signature as `handle_note_editor_normal`, for the same reason.
fn handle_note_editor_insert(editor: &mut NoteEditorState, key: KeyEvent) -> NoteEditorSignal {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
        return match editor.save() {
            Ok(()) => NoteEditorSignal::Handled,
            Err(e) => NoteEditorSignal::SaveFailed(format!("note save failed: {e}")),
        };
    }
    match key.code {
        KeyCode::Esc => editor.esc_to_normal(),
        KeyCode::Enter => editor.split_line(),
        KeyCode::Backspace => editor.backspace(),
        KeyCode::Char(c) => editor.insert_char(c),
        _ => {}
    }
    NoteEditorSignal::Handled
}

/// The inline text prompt nested inside `Mode::Notes` — shared by create and
/// rename (see `NotePromptKind`/`prompt: Option<String>` on
/// `NotesPopupState`). Deliberately a bespoke minimal text buffer — no
/// cursor movement, no autocomplete, no NL pre-pass — rather than reusing
/// `app.draft`/`DraftState`: a filename has none of the todo.txt-line
/// structure `DraftState` exists to manage (project/context autocomplete, NL
/// rewriting, slash-menu overlays), so pulling it in would mean carrying
/// that machinery for no benefit. This mirrors the *shape* of
/// `handle_prompt`'s much lighter `PromptProject`/`PromptContext` driving
/// loop (Esc/Enter/character-editing only, no chord/overlay layering) rather
/// than `handle_insert`'s full vim-like Normal/Insert stack — just without
/// also inheriting `DraftState` itself.
fn handle_notes_prompt(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.cancel_note_prompt(),
        KeyCode::Enter => app.confirm_note_prompt(),
        KeyCode::Backspace => app.notes_popup.prompt_backspace(),
        KeyCode::Char(c) => app.notes_popup.prompt_push(c),
        _ => {}
    }
}

/// The delete-confirmation sub-state nested inside `Mode::Notes` (see
/// `pending_delete: Option<usize>` on `NotesPopupState`): `y`/Enter deletes,
/// `n`/Esc cancels back to browsing. Any other key is ignored so a stray
/// press can't accidentally confirm or cancel.
fn handle_notes_delete_confirm(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('y') | KeyCode::Enter => app.confirm_delete_note(),
        KeyCode::Char('n') | KeyCode::Esc => app.cancel_delete_note_confirm(),
        _ => {}
    }
}

/// What the draft buffer changed (or didn't) in response to a key. Lets
/// callers like search distinguish a text edit (which must re-run the filter)
/// from a cursor move (which must not, otherwise navigating within the search
/// box would reset the visible-list cursor on every arrow press).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DraftEffect {
    Unhandled,
    CursorMoved,
    TextChanged,
}

/// A single text-editing operation on the draft buffer. Covers the standard
/// keys (insert/backspace/delete/arrows/Home/End) plus the readline/emacs set
/// (Ctrl+A/E/B/F/H/D/W/U/K, Alt+B/F/D). Modeling the keystroke as an action
/// keeps the insert/search/prompt/command-palette contexts in sync — they all
/// route through the same resolver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditAction {
    Insert(char),
    DeleteBackward,
    DeleteForward,
    DeleteWordBackward,
    DeleteWordForward,
    KillToStart,
    KillToEnd,
    MoveLeft,
    MoveRight,
    MoveHome,
    MoveEnd,
    MoveWordForward,
    MoveWordBackward,
}

impl EditAction {
    fn apply(self, app: &mut App) -> DraftEffect {
        match self {
            EditAction::Insert(c) => {
                app.draft_insert_char(c);
                DraftEffect::TextChanged
            }
            EditAction::DeleteBackward => {
                app.draft_backspace();
                DraftEffect::TextChanged
            }
            EditAction::DeleteForward => {
                app.draft_delete_forward();
                DraftEffect::TextChanged
            }
            EditAction::DeleteWordBackward => {
                app.draft_delete_word_backward();
                DraftEffect::TextChanged
            }
            EditAction::DeleteWordForward => {
                app.draft_delete_word_forward();
                DraftEffect::TextChanged
            }
            EditAction::KillToStart => {
                app.draft_kill_to_start();
                DraftEffect::TextChanged
            }
            EditAction::KillToEnd => {
                app.draft_kill_to_end();
                DraftEffect::TextChanged
            }
            EditAction::MoveLeft => {
                app.draft_left();
                DraftEffect::CursorMoved
            }
            EditAction::MoveRight => {
                app.draft_right();
                DraftEffect::CursorMoved
            }
            EditAction::MoveHome => {
                app.draft_home();
                DraftEffect::CursorMoved
            }
            EditAction::MoveEnd => {
                app.draft_end();
                DraftEffect::CursorMoved
            }
            EditAction::MoveWordForward => {
                app.draft_word_forward();
                DraftEffect::CursorMoved
            }
            EditAction::MoveWordBackward => {
                app.draft_word_backward();
                DraftEffect::CursorMoved
            }
        }
    }
}

/// Map a single keystroke to an `EditAction`, or `None` when the key isn't a
/// text-editing key. A *single* Control or Alt chord is matched first and
/// never falls through to the plain `Char(c)` insert arm, so an unmapped chord
/// (e.g. Ctrl+G) is swallowed rather than typed as a literal control letter —
/// this is what fixes Ctrl+H inserting an 'h' instead of deleting. Ctrl+N/Ctrl+P
/// are deliberately left unmapped: upstream handlers reserve them for popup
/// and list navigation.
///
/// CONTROL **and** ALT together is AltGr, which crossterm reports for printable
/// characters on international layouts (e.g. AltGr+E → `€`). That is text, not a
/// chord, so the chord arms are gated on exactly one modifier being held and
/// AltGr falls through to the `Char(c)` insert arm.
fn resolve_edit_key(key: KeyEvent) -> Option<EditAction> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    if ctrl && !alt {
        return match key.code {
            KeyCode::Char('a') => Some(EditAction::MoveHome),
            KeyCode::Char('e') => Some(EditAction::MoveEnd),
            KeyCode::Char('b') => Some(EditAction::MoveLeft),
            KeyCode::Char('f') => Some(EditAction::MoveRight),
            KeyCode::Char('h') => Some(EditAction::DeleteBackward),
            KeyCode::Char('d') => Some(EditAction::DeleteForward),
            KeyCode::Char('w') => Some(EditAction::DeleteWordBackward),
            KeyCode::Char('u') => Some(EditAction::KillToStart),
            KeyCode::Char('k') => Some(EditAction::KillToEnd),
            // Ctrl+Backspace as delete-word is a common modern expectation;
            // terminals that report it this way get it for free.
            KeyCode::Backspace => Some(EditAction::DeleteWordBackward),
            _ => None,
        };
    }
    if alt && !ctrl {
        return match key.code {
            KeyCode::Char('b') => Some(EditAction::MoveWordBackward),
            KeyCode::Char('f') => Some(EditAction::MoveWordForward),
            KeyCode::Char('d') => Some(EditAction::DeleteWordForward),
            // M-DEL is readline's backward-kill-word.
            KeyCode::Backspace => Some(EditAction::DeleteWordBackward),
            _ => None,
        };
    }
    match key.code {
        KeyCode::Backspace => Some(EditAction::DeleteBackward),
        KeyCode::Delete => Some(EditAction::DeleteForward),
        KeyCode::Left => Some(EditAction::MoveLeft),
        KeyCode::Right => Some(EditAction::MoveRight),
        KeyCode::Home => Some(EditAction::MoveHome),
        KeyCode::End => Some(EditAction::MoveEnd),
        KeyCode::Char(c) => Some(EditAction::Insert(c)),
        _ => None,
    }
}

/// Apply a standard text-editing key to the draft. Thin wrapper over
/// `resolve_edit_key` + `EditAction::apply`, returning `Unhandled` for keys
/// that aren't text editing so callers can layer their own handling.
fn apply_to_draft(app: &mut App, key: KeyEvent) -> DraftEffect {
    match resolve_edit_key(key) {
        Some(action) => action.apply(app),
        None => DraftEffect::Unhandled,
    }
}

fn handle_insert_normal(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Enter => {
            let outcome = if app.selection.editing().is_some() {
                app.save_edit();
                AddOutcome::Saved
            } else {
                app.add_from_draft()
            };
            if !matches!(outcome, AddOutcome::Parsed) {
                app.mode = Mode::Normal;
                app.draft_clear();
                app.selection.exit_edit();
            }
        }
        KeyCode::Esc => {
            app.mode = Mode::Normal;
            app.draft_clear();
            app.selection.exit_edit();
        }
        KeyCode::Char('h') | KeyCode::Left => app.draft_left(),
        KeyCode::Char('l') | KeyCode::Right => app.draft_right(),
        KeyCode::Char('w') if app.chord.consume('d') => app.draft_delete_word_forward(),
        KeyCode::Char('w') if app.chord.consume('c') => {
            app.draft_delete_word_forward();
            app.draft.set_input_mode(DialogInputMode::Insert);
        }
        KeyCode::Char('w') => app.draft_word_forward(),
        KeyCode::Char('b') => app.draft_word_backward(),
        KeyCode::Char('e') => app.draft_word_end(),
        KeyCode::Char('d') => app.chord.arm('d'),
        KeyCode::Char('c') => app.chord.arm('c'),
        KeyCode::Char('x') => app.draft_delete_forward(),
        KeyCode::Char('i') => app.draft.set_input_mode(DialogInputMode::Insert),
        KeyCode::Char('a') => {
            app.draft_right();
            app.draft.set_input_mode(DialogInputMode::Insert);
        }
        KeyCode::Char('A') => {
            app.draft_end();
            app.draft.set_input_mode(DialogInputMode::Insert);
        }
        _ => {}
    }
}

fn handle_insert(app: &mut App, key: KeyEvent, keybinds: &KeyBindings) {
    if app.draft.input_mode() == DialogInputMode::Normal {
        handle_insert_normal(app, key);
        return;
    }

    // Metadata-picker overlays take precedence. Non-slash overlays fully
    // consume keys until accepted or cancelled; the slash menu intercepts
    // only its navigation keys and lets text editing flow through so the
    // filter text in the buffer keeps growing as the user types.
    let overlay = app.draft.overlay().map(|o| o.kind());
    match overlay {
        Some(OverlayKind::Calendar) => {
            handle_insert_calendar(app, key);
            return;
        }
        Some(OverlayKind::RecurrenceBuilder) => {
            handle_insert_rec_builder(app, key, keybinds);
            return;
        }
        Some(OverlayKind::PriorityChooser) => {
            handle_insert_priority(app, key);
            return;
        }
        Some(OverlayKind::SlashMenu) => {
            if handle_insert_slash_menu(app, key) {
                return;
            }
            // Fall through — let the key flow into the editor so filter chars
            // can be typed/erased. We re-check the overlay invariants after.
            apply_to_draft(app, key);
            // Backspacing past the `/` closes the menu; typing more chars
            // just narrows the filter.
            app.slash_menu_revalidate();
            return;
        }
        None => {}
    }

    // Autocomplete bindings take precedence — only when the popup is visible.
    // Tab accepts; Enter falls through to save so the popup never swallows the
    // submit keystroke (e.g. when the typed token already matches an existing
    // project/context). Esc with the popup open dismisses the popup but leaves
    // Insert mode intact; a second Esc enters Normal mode (handled below).
    if app.autocomplete_visible() {
        match key.code {
            KeyCode::Tab | KeyCode::Enter => {
                app.autocomplete_accept();
                app.draft.suppress_autocomplete();
                return;
            }
            _ => {
                if handle_autocomplete_keys(app, key) {
                    return;
                }
            }
        }
    }

    match key.code {
        KeyCode::Esc => {
            app.draft.set_input_mode(DialogInputMode::Normal);
        }
        KeyCode::Enter => {
            let outcome = if app.selection.editing().is_some() {
                app.save_edit();
                AddOutcome::Saved
            } else {
                app.add_from_draft()
            };
            // `Parsed` means the NL parser rewrote the draft into canonical
            // todo.txt and is asking the user to confirm — stay in Insert so
            // they can review/edit before a second Enter saves.
            if !matches!(outcome, AddOutcome::Parsed) {
                app.mode = Mode::Normal;
                app.draft_clear();
                app.selection.exit_edit();
            }
        }
        _ => {
            let before = app.draft.text().len();
            let effect = apply_to_draft(app, key);
            // `/` opens the slash menu; `:` after a recognised key
            // (`due` / `t` / `rec`) opens the matching picker directly. Both
            // detections run post-insert so they inspect what actually
            // landed in the buffer.
            if effect == DraftEffect::TextChanged && app.draft.text().len() > before {
                match key.code {
                    KeyCode::Char('/') => app.maybe_open_slash_menu(),
                    KeyCode::Char(':') => app.maybe_open_kv_overlay(),
                    _ => {}
                }
            }
        }
    }
}

/// Slash-menu key handler. Returns `true` when the key was consumed by the
/// menu (navigation, accept, dismiss); `false` when the key should fall
/// through to text editing so filter chars are typed into the buffer.
fn handle_insert_slash_menu(app: &mut App, key: KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Up => {
            app.slash_step(false);
            true
        }
        KeyCode::Down => {
            app.slash_step(true);
            true
        }
        KeyCode::Char('n') if ctrl => {
            app.slash_step(true);
            true
        }
        KeyCode::Char('p') if ctrl => {
            app.slash_step(false);
            true
        }
        KeyCode::Tab | KeyCode::Enter => {
            app.slash_accept();
            true
        }
        KeyCode::Esc => {
            app.slash_cancel();
            true
        }
        _ => false,
    }
}

fn handle_insert_calendar(app: &mut App, key: KeyEvent) {
    // In auto-trigger mode (anchor set): digit, dash, and backspace are
    // forwarded to the draft buffer so the user can type the date directly.
    // The calendar grid tracks the typed date as it becomes valid.
    if app.calendar_state().is_some_and(|s| s.anchor.is_some()) {
        let is_date_char = matches!(key.code, KeyCode::Char(c) if c.is_ascii_digit() || c == '-');
        if is_date_char || matches!(key.code, KeyCode::Backspace) {
            apply_to_draft(app, key);
            app.calendar_sync_from_draft();
            return;
        }
    }
    match key.code {
        KeyCode::Char('h') | KeyCode::Left => app.calendar_move(-1, 0),
        KeyCode::Char('l') | KeyCode::Right => app.calendar_move(1, 0),
        KeyCode::Char('k') | KeyCode::Up => app.calendar_move(0, -1),
        KeyCode::Char('j') | KeyCode::Down => app.calendar_move(0, 1),
        KeyCode::Char('t') => app.calendar_set_relative(0),
        KeyCode::Char('T') => app.calendar_set_relative(1),
        KeyCode::Char('w') => app.calendar_set_relative(7),
        KeyCode::Char('m') => app.calendar_add_months(1),
        KeyCode::Char('M') => app.calendar_add_months(-1),
        KeyCode::Char('x') => app.calendar_clear(),
        KeyCode::Enter => app.calendar_accept(),
        KeyCode::Esc => app.calendar_cancel(),
        _ => {}
    }
}

fn handle_insert_rec_builder(app: &mut App, key: KeyEvent, keybinds: &KeyBindings) {
    // Custom `[recurrence]` bindings win over the built-ins, matching how
    // `[normal]` is layered in `resolve_normal_key`.
    if let Some(action) = keybinds.resolve_recurrence(key) {
        apply_rec_action(app, action);
        return;
    }
    let action = match key.code {
        // Horizontal keys change the focused field's *value*: both the unit
        // and the mode render as horizontal segmented controls, so Left/Right
        // moving along them is the affordance the layout already suggests.
        KeyCode::Char('h') | KeyCode::Left => RecAction::ValuePrev,
        KeyCode::Char('l') | KeyCode::Right => RecAction::ValueNext,
        // Vertical keys (and Tab) move *between* fields.
        KeyCode::Char('j') | KeyCode::Down | KeyCode::Tab => RecAction::FocusNext,
        KeyCode::Char('k') | KeyCode::Up | KeyCode::BackTab => RecAction::FocusPrev,
        // `=` is the unshifted `+` on US keyboards — accept both so users
        // don't have to chord Shift to bump the interval.
        KeyCode::Char('+') | KeyCode::Char('=') => RecAction::ValueNext,
        KeyCode::Char('-') | KeyCode::Char('_') => RecAction::ValuePrev,
        KeyCode::Enter => RecAction::Accept,
        KeyCode::Esc => RecAction::Cancel,
        _ => return,
    };
    apply_rec_action(app, action);
}

fn apply_rec_action(app: &mut App, action: RecAction) {
    match action {
        RecAction::FocusNext => app.recurrence_focus(1),
        RecAction::FocusPrev => app.recurrence_focus(-1),
        RecAction::ValueNext => app.recurrence_adjust(1),
        RecAction::ValuePrev => app.recurrence_adjust(-1),
        RecAction::Accept => app.recurrence_accept(),
        RecAction::Cancel => app.recurrence_cancel(),
    }
}

fn handle_insert_priority(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => app.priority_step(true),
        KeyCode::Char('k') | KeyCode::Up => app.priority_step(false),
        KeyCode::Enter => app.priority_accept(),
        KeyCode::Esc => app.priority_cancel(),
        _ => {}
    }
}

fn handle_search(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => {
            app.mode = Mode::Normal;
            app.draft_clear();
            app.clear_search();
        }
        KeyCode::Enter => {
            app.mode = Mode::Normal;
            app.cursor = 0;
        }
        _ => {
            if apply_to_draft(app, key) == DraftEffect::TextChanged {
                app.set_search(app.draft.text().to_string());
            }
        }
    }
}

fn handle_help(app: &mut App, key: KeyEvent) {
    if is_exit_key(key) || matches!(key.code, KeyCode::Esc | KeyCode::Char('?')) {
        app.mode = Mode::Normal;
    }
}

fn handle_settings(app: &mut App, key: KeyEvent) {
    if is_exit_key(key) {
        app.mode = Mode::Normal;
        return;
    }

    match key.code {
        KeyCode::Esc | KeyCode::Char(',') => app.mode = Mode::Normal,
        KeyCode::Char('T') => apply_action(app, Action::CycleTheme),
        KeyCode::Char('D') => apply_action(app, Action::CycleDensity),
        KeyCode::Char('L') => apply_action(app, Action::ToggleLineNum),
        KeyCode::Char('[') => apply_action(app, Action::ToggleLeftPane),
        KeyCode::Char(']') => apply_action(app, Action::ToggleRightPane),
        KeyCode::Char('H') => apply_action(app, Action::ToggleShowDone),
        KeyCode::Char('F') => apply_action(app, Action::ToggleShowFuture),
        KeyCode::Char('S') => apply_action(app, Action::CycleSort),
        _ => {}
    }
}

fn handle_pick(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => app.pick_step(true),
        KeyCode::Char('k') | KeyCode::Up => app.pick_step(false),
        KeyCode::Char('r') => match app.mode {
            Mode::PickProject => app.begin_rename_project(),
            Mode::PickContext => app.begin_rename_context(),
            _ => {}
        },
        KeyCode::Enter => app.pick_accept(),
        KeyCode::Esc => app.pick_cancel(),
        _ => {}
    }
}

fn handle_pick_theme(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down | KeyCode::Char('T') => app.pick_theme_step(true),
        KeyCode::Char('k') | KeyCode::Up => app.pick_theme_step(false),
        KeyCode::Enter => app.pick_theme_accept(),
        KeyCode::Esc => app.pick_theme_cancel(),
        _ => {}
    }
}

fn handle_command_palette(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    // List navigation. Plain j/k must type into the search box — the user
    // might be searching for "jump" — so navigation goes via arrows or
    // Ctrl-N/Ctrl-P (matches the autocomplete popup in handle_insert).
    match key.code {
        KeyCode::Esc => {
            app.mode = app.command_palette.take_prior();
            app.draft_clear();
            return;
        }
        KeyCode::Enter => {
            let chosen = app.command_palette.current_action();
            // Restore the prior mode (Normal or Visual) *before* dispatching
            // so visual-aware actions (ToggleComplete, Delete, ToggleSelected)
            // see the selection. The dispatched action may then set its own
            // mode (BeginAdd → Insert, etc.); we don't stomp it after.
            app.mode = app.command_palette.take_prior();
            app.draft_clear();
            if let Some(action) = chosen {
                apply_action(app, action);
            }
            return;
        }
        KeyCode::Down => {
            app.command_palette.step(1);
            return;
        }
        KeyCode::Up => {
            app.command_palette.step(-1);
            return;
        }
        KeyCode::Char('n') if ctrl => {
            app.command_palette.step(1);
            return;
        }
        KeyCode::Char('p') if ctrl => {
            app.command_palette.step(-1);
            return;
        }
        _ => {}
    }
    if apply_to_draft(app, key) == DraftEffect::TextChanged {
        // `refresh` resets the cursor when the needle actually changes; a
        // same-needle call (e.g. typed-and-deleted character) is a no-op.
        app.command_palette.refresh(app.draft.text());
    }
}

fn handle_autocomplete_keys(app: &mut App, key: KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Up => {
            app.autocomplete_step(false);
            true
        }
        KeyCode::Down => {
            app.autocomplete_step(true);
            true
        }
        KeyCode::Char('n') if ctrl => {
            app.autocomplete_step(true);
            true
        }
        KeyCode::Char('p') if ctrl => {
            app.autocomplete_step(false);
            true
        }
        KeyCode::Esc => {
            app.draft.suppress_autocomplete();
            true
        }
        _ => false,
    }
}

fn handle_prompt(app: &mut App, key: KeyEvent) {
    if app.autocomplete_visible() {
        match key.code {
            KeyCode::Tab => {
                app.autocomplete_accept();
                return;
            }
            _ => {
                if handle_autocomplete_keys(app, key) {
                    return;
                }
            }
        }
    }

    match key.code {
        KeyCode::Esc => {
            app.mode = Mode::Normal;
            app.draft_clear();
        }
        KeyCode::Enter => {
            let prev_mode = app.mode;
            let value = app.draft.text().to_string();
            app.draft_clear();
            app.mode = Mode::Normal;
            match prev_mode {
                Mode::PromptProject => app.add_project_to_current(&value),
                Mode::PromptContext => app.toggle_context_on_current(&value),
                Mode::PromptSaveFilter => app.save_current_filter_as(&value),
                Mode::PromptRenameProject => app.rename_current_project_as(&value),
                Mode::PromptRenameContext => app.rename_current_context_as(&value),
                _ => {}
            }
        }
        _ => {
            apply_to_draft(app, key);
        }
    }
}

// `Action` lives in `tuxedo::action` (see `src/action.rs`). Keeping it in the
// library lets the command palette enumerate every variant without pulling
// main.rs into the dependency graph.

/// Map a single keystroke to an `Action`. Returns `None` when the keystroke
/// is the *first* press of a chord (e.g. `g` of `gg`) or unknown — in both
/// cases there is no immediate behavior to apply.
///
/// Mutates the chord state because chord progress is part of interpreting
/// the key, not a separate concern.
fn resolve_normal_key(app: &mut App, key: KeyEvent, keybinds: &KeyBindings) -> Option<Action> {
    match keybinds.resolve_normal(key, &mut app.chord) {
        Some(ResolvedKey::Action(action)) => return Some(action),
        Some(ResolvedKey::Pending) => return None,
        None => {}
    }

    if is_exit_key(key) {
        return Some(Action::Quit);
    }

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if ctrl {
        return match key.code {
            KeyCode::Char('d') => Some(Action::HalfPageDown),
            KeyCode::Char('u') => Some(Action::HalfPageUp),
            KeyCode::Char('p') => Some(Action::OpenCommandPalette),
            _ => None,
        };
    }
    Some(match key.code {
        KeyCode::Char('j') | KeyCode::Down => Action::CursorDown,
        KeyCode::Char('k') | KeyCode::Up => Action::CursorUp,
        KeyCode::Char('J') => Action::MoveTaskDown,
        KeyCode::Char('K') => Action::MoveTaskUp,
        KeyCode::Char('G') => Action::CursorBottom,
        // First 'g' arms the chord; second 'g' fires CursorTop.
        KeyCode::Char('g') if app.chord.toggle('g') => Action::CursorTop,
        KeyCode::Char('n') => Action::BeginAdd,
        KeyCode::Char('r') => Action::Reschedule,
        KeyCode::Char('a') => Action::ToggleArchiveView,
        KeyCode::Char('l') => Action::GoList,
        KeyCode::Char('e') => Action::BeginEdit,
        KeyCode::Char('i') => Action::BeginEditInsert,
        KeyCode::Char('o') => Action::OpenNotes,
        KeyCode::Char('x') => Action::ToggleComplete,
        // 'dd' chord. First press arms; second fires.
        KeyCode::Char('d') if app.chord.toggle('d') => Action::Delete,
        // 'yy' chord copies the whole line; 'yb' (after 'y' is armed) copies
        // the body only. Plain 'y' just arms the leader.
        KeyCode::Char('y') if app.chord.toggle('y') => Action::CopyLine,
        KeyCode::Char('b') if app.chord.consume('y') => Action::CopyBody,
        KeyCode::Char('p') => {
            // After 'f' arms, 'fp' opens the project picker. Otherwise plain
            // 'p' cycles priority.
            if app.chord.consume('f') {
                Action::PickProject
            } else {
                Action::CyclePriority
            }
        }
        KeyCode::Char('c') => {
            if app.chord.consume('f') {
                Action::PickContext
            } else {
                Action::BeginPromptContext
            }
        }
        KeyCode::Char('/') => Action::BeginSearch,
        KeyCode::Char('?') => Action::OpenHelp,
        KeyCode::Char(',') => Action::OpenSettings,
        KeyCode::Char(':') => Action::OpenCommandPalette,
        KeyCode::Char('u') => Action::Undo,
        KeyCode::Char('v') => Action::ToggleVisual,
        KeyCode::Char(' ') => Action::ToggleSelected,
        KeyCode::Char('A') => Action::ArchiveCompleted,
        // First 'f' arms the leader; a second 'f' (`ff`) opens the saved-
        // search picker. Mirrors the `fp`/`fc` pattern below.
        KeyCode::Char('f') => {
            if app.chord.consume('f') {
                Action::PickSavedFilter
            } else {
                Action::ArmF
            }
        }
        KeyCode::Char('s') => {
            // `fs` saves the active search; plain 's' opens the share QR.
            if app.chord.consume('f') {
                Action::SaveCurrentFilter
            } else {
                Action::OpenShare
            }
        }
        KeyCode::Char('S') => Action::CycleSort,
        KeyCode::Char('+') => Action::BeginPromptProject,
        KeyCode::Char('[') => Action::ToggleLeftPane,
        KeyCode::Char(']') => Action::ToggleRightPane,
        KeyCode::Char('T') => Action::OpenThemePicker,
        KeyCode::Char('D') => Action::CycleDensity,
        KeyCode::Char('L') => Action::ToggleLineNum,
        KeyCode::Char('H') => Action::ToggleShowDone,
        KeyCode::Char('F') => Action::ToggleShowFuture,
        KeyCode::Esc => Action::EscapeStack,
        KeyCode::Char('W') => Action::ChangeWeekStart,
        // T11: tmux-pane-style pin/focus toggle for a note (`z`) and close
        // the pin entirely (`Z`) — both unused letters (confirmed by reading
        // this match before picking them), global rather than popup-internal
        // so they work from bare Mode::Normal and (via `handle_key`'s early
        // routing branch) even while the pinned note itself has focus.
        KeyCode::Char('z') => Action::TogglePinFocus,
        KeyCode::Char('Z') => Action::ClosePinnedNote,
        _ => return None,
    })
}

fn apply_action(app: &mut App, action: Action) {
    // Archive view is read-only with two exceptions: `x` un-archives the
    // row at the cursor, `dd` permanently removes it from done.txt. Other
    // mutating actions flash a hint and abort. Navigation, view-switch,
    // theme/density/layout toggles, and overlays (help/settings) fall
    // through to the normal handler below.
    if app.view() == View::Archive {
        match action {
            Action::ToggleComplete => {
                if let Some(idx) = app.cur_abs() {
                    app.unarchive(idx);
                }
                return;
            }
            Action::Delete => {
                if let Some(idx) = app.cur_abs() {
                    app.archive_delete(idx);
                }
                return;
            }
            Action::BeginAdd
            | Action::BeginEdit
            | Action::BeginEditInsert
            | Action::CyclePriority
            | Action::MoveTaskDown
            | Action::MoveTaskUp
            | Action::ToggleVisual
            | Action::ToggleSelected
            | Action::BeginSearch
            | Action::BeginPromptProject
            | Action::BeginPromptContext
            | Action::PickProject
            | Action::PickContext
            | Action::PickSavedFilter
            | Action::SaveCurrentFilter
            | Action::CycleSort
            | Action::ToggleShowDone
            | Action::ToggleShowFuture
            | Action::Undo => {
                app.flash("read-only in archive");
                return;
            }
            _ => {}
        }
    }
    let len = app.visible_indices().len();
    match action {
        Action::Quit => app.should_quit = true,
        Action::CursorDown => {
            if len > 0 {
                app.cursor = (app.cursor + 1).min(len - 1);
            }
        }
        Action::CursorUp => app.cursor = app.cursor.saturating_sub(1),
        Action::CursorTop => app.cursor = 0,
        Action::CursorBottom => {
            if len > 0 {
                app.cursor = len - 1;
            }
        }
        Action::HalfPageDown => {
            app.cursor = (app.cursor + 10).min(len.saturating_sub(1));
        }
        Action::HalfPageUp => app.cursor = app.cursor.saturating_sub(10),
        Action::BeginAdd => {
            app.mode = Mode::Insert;
            // Seed from the active filter: a task added under `+work` almost
            // always belongs to it, and without the tag it drops out of the
            // view the moment it saves. The cursor parks after the seed, so
            // an unwanted tag is a backspace away.
            let seed = app.filter().tag_seed();
            app.draft_set_insert(seed);
            app.selection.exit_edit();
        }
        Action::BeginEdit => {
            if let Some(abs) = app.cur_abs()
                && let Some(raw) = app.task_raw(abs)
            {
                app.selection.enter_edit(abs);
                app.draft_set(raw);
                app.mode = Mode::Insert;
            }
        }
        Action::BeginEditInsert => {
            if let Some(abs) = app.cur_abs()
                && let Some(raw) = app.task_raw(abs)
            {
                app.selection.enter_edit(abs);
                app.draft_set_insert(raw);
                app.mode = Mode::Insert;
            }
        }
        Action::ToggleComplete => {
            if app.mode == Mode::Visual && !app.selection.is_empty() {
                app.complete_selected();
            } else if let Some(abs) = app.cur_abs() {
                app.toggle_complete(abs);
            }
        }
        Action::Delete => {
            if app.mode == Mode::Visual && !app.selection.is_empty() {
                app.delete_selected();
            } else if let Some(abs) = app.cur_abs() {
                app.delete(abs);
            }
        }
        Action::CyclePriority => {
            if let Some(abs) = app.cur_abs() {
                app.cycle_priority(abs);
            }
        }
        Action::MoveTaskDown => app.move_tasks(true),
        Action::MoveTaskUp => app.move_tasks(false),
        Action::BeginSearch => {
            app.mode = Mode::Search;
            app.draft_clear();
            app.clear_search();
        }
        Action::OpenHelp => app.mode = Mode::Help,
        Action::OpenSettings => app.mode = Mode::Settings,
        Action::OpenCommandPalette => {
            // Snapshot the current mode (Normal or Visual) so cancel/run
            // can restore it — otherwise opening the palette from Visual
            // and cancelling silently exits Visual.
            let prior = app.mode;
            app.command_palette.open(prior);
            app.mode = Mode::CommandPalette;
            app.draft_clear();
        }
        Action::Undo => app.undo(),
        Action::ToggleVisual => {
            app.mode = if app.mode == Mode::Visual {
                Mode::Normal
            } else {
                Mode::Visual
            };
        }
        Action::ToggleSelected => {
            if app.mode == Mode::Visual
                && let Some(abs) = app.cur_abs()
            {
                app.selection.toggle(abs);
            }
        }
        Action::GoList => app.set_view(View::List),
        Action::ToggleArchiveView => {
            let next = if app.view() == View::Archive {
                View::List
            } else {
                View::Archive
            };
            app.set_view(next);
        }
        Action::ArchiveCompleted => {
            if app.view() == View::Archive {
                app.flash("already in archive");
            } else if app.has_completed_tasks() {
                app.archive_completed();
            } else {
                app.flash("no completed tasks to archive");
            }
        }
        Action::ArmF => app.chord.arm('f'),
        Action::PickProject => app.enter_pick_project(),
        Action::PickContext => app.enter_pick_context(),
        Action::PickSavedFilter => app.enter_pick_saved(),
        Action::SaveCurrentFilter => {
            if app.filter().search.is_empty() {
                app.flash("no active search to save");
            } else {
                app.mode = Mode::PromptSaveFilter;
                app.draft_clear();
            }
        }
        Action::CycleSort => app.cycle_sort(),
        Action::BeginPromptProject => {
            app.mode = Mode::PromptProject;
            app.draft_clear();
        }
        Action::BeginPromptContext => {
            app.mode = Mode::PromptContext;
            app.draft_clear();
        }
        Action::ToggleLeftPane => {
            app.prefs.toggle_left();
            app.save_prefs();
        }
        Action::ToggleRightPane => {
            app.prefs.toggle_right();
            app.save_prefs();
        }
        Action::CycleTheme => app.cycle_theme(),
        Action::CycleDensity => app.cycle_density(),
        Action::ToggleLineNum => {
            app.prefs.toggle_line_num();
            app.save_prefs();
        }
        Action::ToggleShowDone => {
            app.prefs.toggle_show_done();
            app.cursor = 0;
            app.recompute_visible();
            app.save_prefs();
        }
        Action::ToggleShowFuture => {
            app.prefs.toggle_show_future();
            app.cursor = 0;
            app.recompute_visible();
            app.save_prefs();
        }
        Action::CopyLine => copy_current_task(app, false),
        Action::CopyBody => copy_current_task(app, true),
        Action::OpenNotes => app.open_notes_for_current(),
        Action::OpenShare => match app.ensure_share_started() {
            Ok(_) => {
                app.mode = Mode::Share;
            }
            Err(e) => app.flash(format!("share unavailable: {e}")),
        },
        Action::OpenThemePicker => {
            if theme::all().len() <= 1 {
                app.flash("only one theme");
            } else {
                app.enter_pick_theme();
            }
        }
        Action::EscapeStack => {
            let has_pc = app.filter().project.is_some() || app.filter().context.is_some();
            let has_search = !app.filter().search.is_empty();
            if has_pc {
                app.set_project_filter(None);
                app.set_context_filter(None);
            } else if has_search {
                app.draft_clear();
                app.clear_search();
            } else if !app.selection.is_empty() {
                app.selection.clear();
            } else if app.mode == Mode::Visual {
                app.mode = Mode::Normal;
            } else if app.view() != View::List {
                app.set_view(View::List);
            }
        }
        // Opens to insert mode just like i/e but with the calendar open and the cursor on the calendar
        // If there is a due date, the cursor begins on the current due date
        // If there is no due date, the cursor begins on today
        // Enter/escape takes the user back to insert mode on the task
        Action::Reschedule => {
            if let Some(abs) = app.cur_abs()
                && let Some(raw) = app.task_raw(abs)
            {
                app.selection.enter_edit(abs);
                app.draft_set_insert(raw);
                app.mode = Mode::Insert;
                app.open_calendar(CalendarTarget::Due);
            }
        }
        Action::ChangeWeekStart => {
            app.toggle_week_start_date();
            app.recompute_visible();
        }
        Action::TogglePinFocus => app.toggle_pin_focus(),
        Action::ClosePinnedNote => app.close_pinned_note(),
    }
}

fn handle_normal(app: &mut App, key: KeyEvent, keybinds: &KeyBindings) {
    if let Some(action) = resolve_normal_key(app, key, keybinds) {
        apply_action(app, action);
    }
    app.clamp_cursor();
}

fn copy_current_task(app: &mut App, body_only: bool) {
    let multi = app.view() == View::List && app.mode == Mode::Visual && !app.selection.is_empty();
    let count = if multi { app.selection.len() } else { 1 };
    let Some(payload) = copy_payload(app, body_only) else {
        if multi {
            app.flash("selection includes hidden tasks");
        }
        return;
    };
    match clipboard::copy(&payload) {
        Ok(()) => app.flash(match (body_only, count) {
            (false, 1) => "copied".into(),
            (true, 1) => "copied (body)".into(),
            (false, count) => format!("copied {count}"),
            (true, count) => format!("copied {count} (body)"),
        }),
        Err(e) => app.flash(format!("copy failed: {e}")),
    }
}

fn copy_payload(app: &App, body_only: bool) -> Option<String> {
    let format = |raw: &str| {
        if body_only {
            todo::body_only(raw)
        } else {
            raw.to_string()
        }
    };
    if app.view() == View::List && app.mode == Mode::Visual && !app.selection.is_empty() {
        let lines: Vec<_> = app
            .visible_indices()
            .iter()
            .filter(|&&abs| app.selection.is_selected(abs))
            .map(|&abs| format(&app.tasks()[abs].raw))
            .collect();
        return (lines.len() == app.selection.len()).then(|| lines.join("\n"));
    }
    app.cur_task().map(|task| format(&task.raw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tuxedo::app::Sort;
    use tuxedo::config::Config;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn alt(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)
    }

    fn resolve(app: &mut App, key: KeyEvent) -> Option<Action> {
        resolve_normal_key(app, key, &KeyBindings::default())
    }

    fn task_lines(app: &App) -> Vec<&str> {
        app.tasks().iter().map(|task| task.raw.as_str()).collect()
    }

    fn welcome_app(name: &str) -> (App, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "tuxedo-welcome-{name}-{}-{:?}.txt",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut app = App::new(
            path.clone(),
            String::new(),
            "2026-05-07".into(),
            Config::default(),
        );
        app.mode = Mode::Welcome;
        (app, path)
    }

    #[test]
    fn welcome_c_creates_cwd_file_and_enters_normal() {
        let (mut app, path) = welcome_app("c");
        assert!(!path.exists(), "precondition: file must not exist yet");
        handle_welcome(&mut app, key('c'));
        assert!(path.exists(), "`c` must create the target file");
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.file_path, path, "`c` keeps the cwd target path");
        assert!(!app.should_quit);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn welcome_s_opens_sample_and_enters_normal() {
        let (mut app, path) = welcome_app("s");
        handle_welcome(&mut app, key('s'));
        assert_eq!(app.mode, Mode::Normal);
        assert_ne!(app.file_path, path, "`s` rebinds away from the cwd target");
        assert!(
            app.file_path.ends_with("tuxedo-sample.txt"),
            "`s` opens the bundled sample, got {:?}",
            app.file_path
        );
        assert!(!app.tasks().is_empty(), "sample must load tasks");
        assert!(!path.exists(), "`s` must not create the cwd file");
    }

    #[test]
    fn welcome_q_and_esc_quit_without_creating_anything() {
        let (mut app, path) = welcome_app("q");
        handle_welcome(&mut app, key('q'));
        assert!(app.should_quit, "`q` must quit");
        assert!(!path.exists(), "`q` must not create a file");

        let (mut app, path) = welcome_app("esc");
        handle_welcome(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.should_quit, "Esc must quit");
        assert!(!path.exists(), "Esc must not create a file");
    }

    #[test]
    fn settings_hinted_keys_apply_and_dialog_stays_open() {
        let mut app = build_app();
        app.mode = Mode::Settings;

        let theme_before = app.prefs.theme_idx();
        handle_settings(&mut app, key('T'));
        assert_ne!(
            app.prefs.theme_idx(),
            theme_before,
            "`T` must cycle the theme, per the settings screen's own hint"
        );
        assert_eq!(
            app.mode,
            Mode::Settings,
            "adjusting a setting must not close the dialog"
        );

        let show_done_before = app.prefs.show_done;
        handle_settings(&mut app, key('H'));
        assert_ne!(
            app.prefs.show_done, show_done_before,
            "`H` must toggle show-done, per the settings screen's own hint"
        );
        assert_eq!(app.mode, Mode::Settings);

        handle_settings(&mut app, key('q'));
        assert_eq!(app.mode, Mode::Normal, "`q` must still close the dialog");
    }

    #[test]
    fn pick_theme_t_cycles_like_j_and_esc_still_cancels() {
        let mut app = build_app();
        app.enter_pick_theme();
        assert_eq!(app.mode, Mode::PickTheme);

        let orig = app.prefs.theme_idx();
        handle_pick_theme(&mut app, key('T'));
        assert_ne!(
            app.prefs.theme_idx(),
            orig,
            "`T` must step to the next theme, same as `j`, not just close the dialog"
        );
        assert_eq!(app.mode, Mode::PickTheme, "`T` must not close the dialog");

        handle_pick_theme(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(
            app.prefs.theme_idx(),
            orig,
            "Esc must still revert to the theme active when the picker opened"
        );
    }

    #[test]
    fn config_reload_is_deferred_while_pick_theme_is_open() {
        let mut app = build_app();
        app.enter_pick_theme();
        app.pick_theme_step(true);
        let previewed = app.prefs.theme_idx();

        let (tx, rx) = mpsc::channel();
        tx.send(()).expect("test channel receiver is alive");
        let rx = Some(rx);

        assert!(
            !poll_config_reload(&mut app, &rx),
            "a reload must not be applied while the picker's live preview is unsaved"
        );
        assert_eq!(
            app.prefs.theme_idx(),
            previewed,
            "the deferred reload must not clobber the in-memory preview"
        );

        app.mode = Mode::Normal;
        assert!(
            poll_config_reload(&mut app, &rx),
            "the queued signal must still be applied once the picker closes"
        );
    }

    fn build_app() -> App {
        let path = std::env::temp_dir().join(format!(
            "tuxedo-bindings-{}-{:?}.txt",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::write(&path, "a\nb\nc\n");
        App::new(
            path,
            "a\nb\nc\n".into(),
            "2026-05-07".into(),
            Config::default(),
        )
    }

    fn build_app_with_due() -> App {
        let path = std::env::temp_dir().join(format!(
            "tuxedo-bindings-{}-{:?}.txt",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::write(&path, "Buy milk due:2026-06-30\n");
        App::new(
            path,
            "Buy milk due:2026-06-30\n".into(),
            "2026-05-07".into(),
            Config::default(),
        )
    }

    #[test]
    fn disconnected_config_watcher_is_ignored() {
        let mut app = build_app();
        let (tx, rx) = mpsc::channel();
        drop(tx);
        assert!(!poll_config_reload(&mut app, &Some(rx)));
    }

    #[test]
    fn plain_keys_resolve_to_their_actions() {
        let mut app = build_app();
        assert_eq!(resolve(&mut app, key('q')), Some(Action::Quit));
        assert_eq!(resolve(&mut app, key('j')), Some(Action::CursorDown),);
        assert_eq!(resolve(&mut app, key('J')), Some(Action::MoveTaskDown),);
        assert_eq!(resolve(&mut app, key('K')), Some(Action::MoveTaskUp),);
        assert_eq!(resolve(&mut app, key('?')), Some(Action::OpenHelp));
        assert_eq!(resolve(&mut app, ctrl('d')), Some(Action::HalfPageDown),);
        assert_eq!(resolve(&mut app, key('n')), Some(Action::BeginAdd),);
        assert_eq!(resolve(&mut app, key('a')), Some(Action::ToggleArchiveView),);
        assert_eq!(resolve(&mut app, key('A')), Some(Action::ArchiveCompleted),);
        assert_eq!(resolve(&mut app, key('S')), Some(Action::CycleSort),);
    }

    #[test]
    fn custom_keybinds_override_builtins() {
        let mut app = build_app();
        let keybinds = KeyBindings::parse("[normal]\nopen_help = \"q\"\n");
        assert_eq!(
            resolve_normal_key(&mut app, key('q'), &keybinds),
            Some(Action::OpenHelp)
        );
        assert_eq!(
            resolve_normal_key(&mut app, ctrl('d'), &keybinds),
            Some(Action::HalfPageDown),
        );
        assert_eq!(
            resolve_normal_key(&mut app, key('n'), &keybinds),
            Some(Action::BeginAdd),
        );
        assert_eq!(
            resolve_normal_key(&mut app, key('r'), &keybinds),
            Some(Action::Reschedule),
        );
        assert_eq!(
            resolve_normal_key(&mut app, key('a'), &keybinds),
            Some(Action::ToggleArchiveView),
        );
        assert_eq!(
            resolve_normal_key(&mut app, key('A'), &keybinds),
            Some(Action::ArchiveCompleted),
        );
        assert_eq!(
            resolve_normal_key(&mut app, key('S'), &keybinds),
            Some(Action::CycleSort),
        );
    }

    #[test]
    fn capital_a_archives_only_when_completed_tasks_exist() {
        // No completed tasks → flash, no archive write.
        let mut app = build_app_with_archive("a\nb\nc\n", None);
        apply_action(&mut app, Action::ArchiveCompleted);
        assert_eq!(app.flash_active(), Some("no completed tasks to archive"));
        assert_eq!(app.tasks().len(), 3);

        // One completed task → archive_completed runs.
        let mut app = build_app_with_archive("x 2026-05-08 done one\nb\n", None);
        apply_action(&mut app, Action::ArchiveCompleted);
        assert_eq!(app.tasks().len(), 1, "completed task must be archived");
    }

    #[test]
    fn lowercase_l_returns_to_list_from_any_view() {
        let mut app = build_app_with_archive("a\n", Some("x 2026-05-02 2026-04-02 done\n"));
        app.set_view(View::Archive);
        apply_action(&mut app, Action::GoList);
        assert_eq!(app.view(), View::List);
    }

    #[test]
    fn lowercase_a_toggles_archive_view() {
        let mut app = build_app_with_archive("a\n", Some("x 2026-05-02 2026-04-02 done\n"));
        assert_eq!(app.view(), View::List);
        apply_action(&mut app, Action::ToggleArchiveView);
        assert_eq!(app.view(), View::Archive);
        apply_action(&mut app, Action::ToggleArchiveView);
        assert_eq!(app.view(), View::List);
    }

    #[test]
    fn gg_chord_only_fires_on_second_press() {
        let mut app = build_app();
        // First 'g' arms the chord but produces no action.
        assert_eq!(resolve(&mut app, key('g')), None);
        // Second 'g' fires.
        assert_eq!(resolve(&mut app, key('g')), Some(Action::CursorTop));
    }

    #[test]
    fn fp_chord_routes_to_pick_project() {
        let mut app = build_app();
        // 'f' arms the leader.
        assert_eq!(resolve(&mut app, key('f')), Some(Action::ArmF));
        apply_action(&mut app, Action::ArmF);
        // 'p' after armed 'f' picks project, not cycles priority.
        assert_eq!(resolve(&mut app, key('p')), Some(Action::PickProject));
    }

    #[test]
    fn p_without_chord_cycles_priority() {
        let mut app = build_app();
        assert_eq!(resolve(&mut app, key('p')), Some(Action::CyclePriority),);
    }

    #[test]
    fn unknown_key_returns_none() {
        let mut app = build_app();
        let k = KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE);
        assert_eq!(resolve(&mut app, k), None);
    }

    #[test]
    fn yy_chord_only_fires_on_second_press() {
        let mut app = build_app();
        // First 'y' arms the chord but produces no action.
        assert_eq!(resolve(&mut app, key('y')), None);
        // Second 'y' fires the line copy.
        assert_eq!(resolve(&mut app, key('y')), Some(Action::CopyLine));
    }

    #[test]
    fn yb_chord_routes_to_copy_body() {
        let mut app = build_app();
        // 'y' arms the leader without firing.
        assert_eq!(resolve(&mut app, key('y')), None);
        // 'b' after armed 'y' copies the body.
        assert_eq!(resolve(&mut app, key('b')), Some(Action::CopyBody));
    }

    #[test]
    fn visual_copy_payload_uses_display_order() {
        let mut app = build_app_with_archive("(B) Second @phone\n(A) First +work\n", None);
        app.mode = Mode::Visual;
        app.selection.toggle(0);
        app.selection.toggle(1);

        assert_eq!(
            copy_payload(&app, false).as_deref(),
            Some("(A) First +work\n(B) Second @phone")
        );
        assert_eq!(copy_payload(&app, true).as_deref(), Some("First\nSecond"));
    }

    #[test]
    fn visual_copy_rejects_hidden_selections() {
        let mut app = build_app_with_archive("first +work\nhidden +home\n", None);
        app.prefs.sort = Sort::File;
        app.mode = Mode::Visual;
        app.selection.toggle(0);
        app.selection.toggle(1);
        app.set_project_filter(Some("work".into()));

        assert_eq!(copy_payload(&app, false), None);
        copy_current_task(&mut app, false);
        assert_eq!(app.flash_active(), Some("selection includes hidden tasks"));
    }

    #[test]
    fn plain_b_without_y_armed_is_unhandled() {
        let mut app = build_app();
        // No leader → 'b' is not bound to anything else, so nothing fires.
        assert_eq!(resolve(&mut app, key('b')), None);
    }

    #[test]
    fn cursor_actions_clamp_to_visible_range() {
        let mut app = build_app();
        // 3 visible tasks, cursor starts at 0.
        apply_action(&mut app, Action::CursorBottom);
        assert_eq!(app.cursor, 2);
        apply_action(&mut app, Action::CursorDown);
        assert_eq!(app.cursor, 2);
        apply_action(&mut app, Action::CursorTop);
        assert_eq!(app.cursor, 0);
        apply_action(&mut app, Action::CursorUp);
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn capital_j_and_k_reorder_within_priority_sort_ties() {
        let mut app = build_app_with_archive("(A) first\n(B) middle\n(A) second\n", None);
        apply_action(&mut app, Action::MoveTaskDown);
        assert_eq!(app.tasks()[0].raw, "(A) second");
        assert_eq!(app.tasks()[2].raw, "(A) first");
        assert_eq!(app.cursor, 1);

        apply_action(&mut app, Action::MoveTaskUp);
        assert_eq!(app.tasks()[0].raw, "(A) first");
        assert_eq!(app.tasks()[2].raw, "(A) second");
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn task_movement_preserves_priority_due_fallback() {
        let mut app = build_app_with_archive(
            "(A) later due:2026-06-20\n(A) sooner due:2026-06-01\n",
            None,
        );
        apply_action(&mut app, Action::MoveTaskDown);
        assert_eq!(
            task_lines(&app),
            ["(A) later due:2026-06-20", "(A) sooner due:2026-06-01"]
        );
        assert_eq!(app.cursor, 0);
        assert_eq!(app.flash_active(), Some("edge of priority/due group"));
    }

    #[test]
    fn task_movement_reorders_equal_due_dates() {
        let mut app = build_app_with_archive(
            "first due:2026-06-01\nundated\nsecond due:2026-06-01\n",
            None,
        );
        app.prefs.sort = Sort::Due;
        app.recompute_visible();
        apply_action(&mut app, Action::MoveTaskDown);
        assert_eq!(
            task_lines(&app),
            ["second due:2026-06-01", "undated", "first due:2026-06-01"]
        );
        assert_eq!(app.cursor, 1);

        apply_action(&mut app, Action::MoveTaskDown);
        assert_eq!(app.flash_active(), Some("edge of due-date group"));
    }

    #[test]
    fn task_movement_is_unrestricted_in_file_sort() {
        let mut app = build_app_with_archive(
            "(B) first due:2026-06-20\n(A) second due:2026-06-01\n",
            None,
        );
        app.prefs.sort = Sort::File;
        app.recompute_visible();
        apply_action(&mut app, Action::MoveTaskDown);
        assert_eq!(
            task_lines(&app),
            ["(A) second due:2026-06-01", "(B) first due:2026-06-20"]
        );
        assert_eq!(app.cursor, 1);
    }

    #[test]
    fn filtered_movement_swaps_visible_tasks_without_moving_hidden_tasks() {
        let mut app = build_app_with_archive("first +work\nhidden +home\nsecond +work\n", None);
        app.prefs.sort = Sort::File;
        app.set_project_filter(Some("work".into()));

        apply_action(&mut app, Action::MoveTaskDown);

        assert_eq!(
            task_lines(&app),
            ["second +work", "hidden +home", "first +work"]
        );
        assert_eq!(app.cursor, 1);
    }

    #[test]
    fn movement_rejects_selections_with_hidden_tasks() {
        let mut app = build_app_with_archive("first +work\nhidden +home\nsecond +work\n", None);
        app.prefs.sort = Sort::File;
        app.mode = Mode::Visual;
        app.selection.toggle(0);
        app.selection.toggle(1);
        app.set_project_filter(Some("work".into()));

        apply_action(&mut app, Action::MoveTaskDown);

        assert_eq!(
            task_lines(&app),
            ["first +work", "hidden +home", "second +work"]
        );
        assert_eq!(app.flash_active(), Some("selection includes hidden tasks"));
    }

    #[test]
    fn movement_rejects_selections_across_sort_groups() {
        let mut app =
            build_app_with_archive("(A) first\n(A) second\n(B) third\n(B) fourth\n", None);
        app.mode = Mode::Visual;
        app.selection.toggle(0);
        app.selection.toggle(2);

        apply_action(&mut app, Action::MoveTaskDown);

        assert_eq!(
            task_lines(&app),
            ["(A) first", "(A) second", "(B) third", "(B) fourth"]
        );
        assert_eq!(app.flash_active(), Some("selection spans sort groups"));
    }

    #[test]
    fn visual_selection_crosses_sort_groups_in_file_sort() {
        let mut app = build_app_with_archive("(A) first\n(B) second\n(C) third\n", None);
        app.prefs.sort = Sort::File;
        app.mode = Mode::Visual;
        app.selection.toggle(0);
        app.selection.toggle(1);

        apply_action(&mut app, Action::MoveTaskDown);

        assert_eq!(task_lines(&app), ["(C) third", "(A) first", "(B) second"]);
        assert!(app.selection.is_selected(1));
        assert!(app.selection.is_selected(2));
    }

    #[test]
    fn visual_selection_moves_as_one_undoable_block() {
        let mut app =
            build_app_with_archive("(A) first\n(A) second\n(A) third\n(A) fourth\n", None);
        app.mode = Mode::Visual;
        app.selection.toggle(0);
        app.selection.toggle(1);
        app.cursor = 1;

        apply_action(&mut app, Action::MoveTaskDown);
        assert_eq!(
            task_lines(&app),
            ["(A) third", "(A) first", "(A) second", "(A) fourth"]
        );
        assert!(app.selection.is_selected(1));
        assert!(app.selection.is_selected(2));
        assert_eq!(app.cursor, 2);

        apply_action(&mut app, Action::Undo);
        assert_eq!(
            task_lines(&app),
            ["(A) first", "(A) second", "(A) third", "(A) fourth"]
        );
        assert!(app.selection.is_empty());
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn disjoint_visual_selection_moves_independently() {
        let mut app =
            build_app_with_archive("(A) first\n(A) second\n(A) third\n(A) fourth\n", None);
        app.mode = Mode::Visual;
        app.selection.toggle(0);
        app.selection.toggle(2);
        app.cursor = 2;

        apply_action(&mut app, Action::MoveTaskDown);
        assert_eq!(
            task_lines(&app),
            ["(A) second", "(A) first", "(A) fourth", "(A) third"]
        );
        assert!(app.selection.is_selected(1));
        assert!(app.selection.is_selected(3));
        assert_eq!(app.cursor, 3);

        apply_action(&mut app, Action::MoveTaskUp);
        assert_eq!(
            task_lines(&app),
            ["(A) first", "(A) second", "(A) third", "(A) fourth"]
        );
        assert!(app.selection.is_selected(0));
        assert!(app.selection.is_selected(2));
        assert_eq!(app.cursor, 2);
    }

    /// Build an isolated App rooted in a fresh temp dir, optionally seeding
    /// done.txt and waiting for the startup loader to land.
    fn build_app_with_archive(todo_raw: &str, done_raw: Option<&str>) -> App {
        use std::time::{Duration, Instant};
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("tuxedo-bindings-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create test dir");
        let todo_path = dir.join("todo.txt");
        std::fs::write(&todo_path, todo_raw).expect("write todo.txt");
        if let Some(body) = done_raw {
            std::fs::write(dir.join("done.txt"), body).expect("write done.txt");
        }
        let mut app = App::new(
            todo_path,
            todo_raw.into(),
            "2026-05-06".into(),
            Config::default(),
        );
        if done_raw.is_some() {
            // Drain the startup archive loader so app.archive is populated.
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                let _ = app.poll_archive();
                if !app.archive().is_empty() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(!app.archive().is_empty(), "archive failed to load in time");
        }
        app
    }

    #[test]
    fn cursor_navigation_works_in_archive() {
        let mut app = build_app_with_archive(
            "a due:2026-05-04\nb due:2026-05-06\nc due:2026-05-08\n",
            Some("x 2026-05-01 2026-04-01 first\nx 2026-05-02 2026-04-02 second\n"),
        );
        app.set_view(View::Archive);
        assert_eq!(app.cursor, 0);
        apply_action(&mut app, Action::CursorDown);
        assert_eq!(app.cursor, 1, "Archive view must allow CursorDown");
        apply_action(&mut app, Action::CursorTop);
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn archive_x_unarchives_task_under_cursor() {
        let mut app = build_app_with_archive("a\n", Some("x 2026-05-02 2026-04-02 done one\n"));
        app.set_view(View::Archive);
        apply_action(&mut app, Action::ToggleComplete);
        assert_eq!(app.archive().len(), 0, "task must leave the archive");
        assert!(
            app.tasks()
                .iter()
                .any(|t| t.raw.contains("done one") && !t.done),
            "un-completed entry must rejoin live tasks"
        );
    }

    #[test]
    fn archive_dd_permanently_deletes_task_under_cursor() {
        let mut app = build_app_with_archive("a\n", Some("x 2026-05-02 2026-04-02 done one\n"));
        app.set_view(View::Archive);
        apply_action(&mut app, Action::Delete);
        assert_eq!(app.archive().len(), 0);
        assert_eq!(app.tasks().len(), 1, "todo.txt must be untouched");
    }

    #[test]
    fn archive_mutations_flash_readonly() {
        let mut app = build_app_with_archive("a\n", Some("x 2026-05-02 2026-04-02 done one\n"));
        app.set_view(View::Archive);
        apply_action(&mut app, Action::BeginEdit);
        assert_eq!(app.flash_active(), Some("read-only in archive"));
        apply_action(&mut app, Action::CyclePriority);
        assert_eq!(app.flash_active(), Some("read-only in archive"));
        apply_action(&mut app, Action::MoveTaskDown);
        assert_eq!(app.flash_active(), Some("read-only in archive"));
        assert!(app.archive().tasks()[0].done);
    }

    #[test]
    fn lowercase_r_reschedules_task_with_due_date() {
        let mut app = build_app_with_due();
        assert_eq!(app.tasks().len(), 1);
        assert_eq!(app.tasks()[0].due.as_deref(), Some("2026-06-30"));
        assert_eq!(app.mode, Mode::Normal);

        assert_eq!(resolve(&mut app, key('r')), Some(Action::Reschedule),);
        apply_action(&mut app, Action::Reschedule);
        assert_eq!(app.mode, Mode::Insert);

        let s = app.calendar_state().expect("calendar should be open");
        assert_eq!(
            s.focused,
            NaiveDate::from_ymd_opt(2026, 6, 30).expect("there should be a date set")
        );

        app.calendar_add_months(1);
        app.calendar_accept();
        assert!(app.draft.overlay().is_none());
        app.add_from_draft();
        let task = app.tasks().last().expect("task added");
        assert_eq!(task.due.as_deref(), Some("2026-07-30"));
    }

    #[test]
    fn lowercase_r_reschedules_task_without_due_date() {
        let mut app = build_app();
        assert_eq!(app.tasks().len(), 3);
        assert_eq!(app.tasks()[0].due.as_deref(), None);
        assert_eq!(app.mode, Mode::Normal);

        assert_eq!(resolve(&mut app, key('r')), Some(Action::Reschedule),);
        apply_action(&mut app, Action::Reschedule);
        assert_eq!(app.mode, Mode::Insert);

        let s = app.calendar_state().expect("calendar should be open");
        assert_eq!(
            s.focused,
            NaiveDate::from_ymd_opt(2026, 5, 7).expect("there should be a date set")
        );

        app.calendar_add_months(1);
        app.calendar_accept();
        app.add_from_draft();
        assert!(app.draft.overlay().is_none());
        let task = app.tasks().last().expect("task added");
        assert_eq!(task.due.as_deref(), Some("2026-06-07"));
    }

    #[test]
    fn begin_add_seeds_draft_from_active_filter() {
        let mut app = build_app();
        app.set_project_filter(Some("work".to_string()));
        app.set_context_filter(Some("home".to_string()));
        apply_action(&mut app, Action::BeginAdd);
        assert_eq!(app.mode, Mode::Insert);
        assert_eq!(app.draft.text(), "+work @home ");
        assert_eq!(
            app.draft.cursor(),
            "+work @home ".len(),
            "cursor parks after the seed so the body types straight in"
        );
        for c in "Buy milk".chars() {
            app.draft_insert_char(c);
        }
        assert_eq!(app.draft.text(), "+work @home Buy milk");
    }

    #[test]
    fn begin_add_leaves_draft_empty_without_a_filter() {
        let mut app = build_app();
        apply_action(&mut app, Action::BeginAdd);
        assert_eq!(app.mode, Mode::Insert);
        assert_eq!(app.draft.text(), "");
        assert_eq!(app.draft.cursor(), 0);
    }

    #[test]
    fn capital_w_toggles_week_start() {
        let mut app = build_app();
        assert_eq!(resolve(&mut app, key('W')), Some(Action::ChangeWeekStart));
    }

    #[test]
    fn rec_builder_hl_changes_value_and_jk_moves_focus() {
        use tuxedo::app::BuilderField;

        fn code(c: KeyCode) -> KeyEvent {
            KeyEvent::new(c, KeyModifiers::NONE)
        }

        // Regression: h/l/j/k all called `recurrence_focus`, so no navigation
        // key could change a field's value — only `+`/`-` did, and the hint
        // advertising them was clipped off the bottom of the popup.
        let binds = KeyBindings::default();
        let mut app = build_app();
        app.open_recurrence_builder();
        assert_eq!(
            app.recurrence_state().expect("builder open").field,
            BuilderField::Interval
        );

        // Horizontal keys change the value and leave focus put.
        handle_insert_rec_builder(&mut app, key('l'), &binds);
        let s = app.recurrence_state().expect("builder open");
        assert_eq!(s.field, BuilderField::Interval, "h/l must not move focus");
        assert_eq!(s.interval, 2, "l must increment the interval");
        handle_insert_rec_builder(&mut app, code(KeyCode::Left), &binds);
        assert_eq!(app.recurrence_state().expect("builder open").interval, 1);

        // Vertical keys move focus and leave the value put.
        handle_insert_rec_builder(&mut app, key('j'), &binds);
        let s = app.recurrence_state().expect("builder open");
        assert_eq!(s.field, BuilderField::Unit, "j must move focus");
        assert_eq!(s.interval, 1, "j must not change the interval");

        // With Unit focused, horizontal now cycles the unit.
        let before = app.recurrence_state().expect("builder open").unit;
        handle_insert_rec_builder(&mut app, key('l'), &binds);
        assert_ne!(
            app.recurrence_state().expect("builder open").unit,
            before,
            "l must cycle the unit"
        );

        // Tab / k also move focus.
        handle_insert_rec_builder(&mut app, code(KeyCode::Tab), &binds);
        assert_eq!(
            app.recurrence_state().expect("builder open").field,
            BuilderField::Mode
        );
        handle_insert_rec_builder(&mut app, key('k'), &binds);
        assert_eq!(
            app.recurrence_state().expect("builder open").field,
            BuilderField::Unit
        );
    }

    #[test]
    fn rec_builder_custom_binding_wins_over_builtin() {
        use tuxedo::app::BuilderField;

        // `[recurrence]` swaps the built-in meaning of `l`: it should move
        // focus rather than change the value, proving custom bindings are
        // consulted before the defaults.
        let binds = KeyBindings::parse("[recurrence]\nfocus_next = \"l\"\n");
        let mut app = build_app();
        app.open_recurrence_builder();
        handle_insert_rec_builder(&mut app, key('l'), &binds);
        let s = app.recurrence_state().expect("builder open");
        assert_eq!(s.field, BuilderField::Unit);
        assert_eq!(s.interval, 1, "rebound l must not touch the interval");

        // Keys the user did not rebind keep their built-in behavior.
        handle_insert_rec_builder(&mut app, key('k'), &binds);
        assert_eq!(
            app.recurrence_state().expect("builder open").field,
            BuilderField::Interval
        );
    }

    #[test]
    fn ctrl_emacs_keys_resolve_to_edit_actions() {
        assert_eq!(resolve_edit_key(ctrl('a')), Some(EditAction::MoveHome));
        assert_eq!(resolve_edit_key(ctrl('e')), Some(EditAction::MoveEnd));
        assert_eq!(resolve_edit_key(ctrl('b')), Some(EditAction::MoveLeft));
        assert_eq!(resolve_edit_key(ctrl('f')), Some(EditAction::MoveRight));
        assert_eq!(
            resolve_edit_key(ctrl('h')),
            Some(EditAction::DeleteBackward)
        );
        assert_eq!(resolve_edit_key(ctrl('d')), Some(EditAction::DeleteForward));
        assert_eq!(
            resolve_edit_key(ctrl('w')),
            Some(EditAction::DeleteWordBackward)
        );
        assert_eq!(resolve_edit_key(ctrl('u')), Some(EditAction::KillToStart));
        assert_eq!(resolve_edit_key(ctrl('k')), Some(EditAction::KillToEnd));
    }

    #[test]
    fn alt_word_keys_resolve_to_word_actions() {
        assert_eq!(
            resolve_edit_key(alt('b')),
            Some(EditAction::MoveWordBackward)
        );
        assert_eq!(
            resolve_edit_key(alt('f')),
            Some(EditAction::MoveWordForward)
        );
        assert_eq!(
            resolve_edit_key(alt('d')),
            Some(EditAction::DeleteWordForward)
        );
    }

    #[test]
    fn unmapped_ctrl_chord_is_swallowed_not_typed() {
        // The historical bug: Ctrl+H (and friends) inserted a literal letter.
        // Unmapped control chords must resolve to nothing, never an Insert.
        assert_eq!(resolve_edit_key(ctrl('g')), None);
        assert_eq!(resolve_edit_key(ctrl('z')), None);
    }

    #[test]
    fn plain_and_shifted_chars_insert() {
        assert_eq!(resolve_edit_key(key('x')), Some(EditAction::Insert('x')));
        let shifted = KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT);
        assert_eq!(resolve_edit_key(shifted), Some(EditAction::Insert('A')));
    }

    #[test]
    fn altgr_char_inserts_not_swallowed() {
        // AltGr is reported as CONTROL|ALT by crossterm for printable chars on
        // international layouts. It must insert text, not fire a Ctrl chord —
        // both a letter that collides with the ctrl table ('e') and one that
        // doesn't ('€') must reach Insert.
        let altgr = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL | KeyModifiers::ALT);
        assert_eq!(resolve_edit_key(altgr('e')), Some(EditAction::Insert('e')));
        assert_eq!(resolve_edit_key(altgr('€')), Some(EditAction::Insert('€')));
    }

    #[test]
    fn ctrl_h_deletes_instead_of_inserting_in_insert_mode() {
        // End-to-end through handle_insert: Ctrl+H must delete the char before
        // the cursor rather than typing 'h'.
        let mut app = build_app();
        app.mode = Mode::Insert;
        app.draft_clear();
        app.draft_insert_char('a');
        app.draft_insert_char('b');
        handle_insert(&mut app, ctrl('h'), &KeyBindings::default());
        assert_eq!(app.draft.text(), "a");
    }

    #[test]
    fn ctrl_u_clears_to_start_in_search_mode() {
        // Ctrl+U in the search box wipes back to the start and re-runs the
        // filter via the TextChanged effect.
        let mut app = build_app();
        app.mode = Mode::Search;
        app.draft_clear();
        for c in "abc".chars() {
            app.draft_insert_char(c);
        }
        app.set_search("abc".into());
        handle_search(&mut app, ctrl('u'));
        assert_eq!(app.draft.text(), "");
    }

    #[test]
    fn lowercase_o_resolves_to_open_notes_and_capital_o_is_freed() {
        let mut app = build_app();
        assert_eq!(resolve(&mut app, key('o')), Some(Action::OpenNotes));
        // `O` used to be CreateOrOpenNote; it is freed and now resolves to
        // nothing built-in.
        assert_eq!(resolve(&mut app, key('O')), None);
    }

    #[test]
    fn open_notes_action_enters_notes_mode() {
        let mut app = build_app();
        apply_action(&mut app, Action::OpenNotes);
        assert_eq!(app.mode, Mode::Notes);
    }

    #[test]
    fn handle_notes_up_down_moves_cursor_and_esc_returns_to_normal() {
        let mut app = build_app();
        app.notes_popup = tuxedo::app::NotesPopupState::new(vec![
            std::path::PathBuf::from("a.md"),
            std::path::PathBuf::from("b.md"),
        ]);
        app.mode = Mode::Notes;

        handle_notes(&mut app, key('j'));
        assert_eq!(app.notes_popup.cursor, 1);
        handle_notes(&mut app, key('k'));
        assert_eq!(app.notes_popup.cursor, 0);

        handle_notes(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn handle_notes_n_key_opens_prompt_and_esc_cancels_back_to_the_list() {
        let mut app = build_app();
        app.mode = Mode::Notes;

        handle_notes(&mut app, key('n'));
        assert_eq!(app.notes_popup.prompt, Some(String::new()));

        for c in "foo".chars() {
            handle_notes(&mut app, key(c));
        }
        assert_eq!(app.notes_popup.prompt.as_deref(), Some("foo"));

        handle_notes(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(
            app.notes_popup.prompt, None,
            "Esc cancels the prompt without creating anything"
        );
        assert_eq!(
            app.mode,
            Mode::Notes,
            "Esc from the prompt returns to the list, not Mode::Normal"
        );
    }

    #[test]
    fn handle_notes_prompt_enter_on_empty_input_is_a_noop() {
        let mut app = build_app();
        app.mode = Mode::Notes;

        handle_notes(&mut app, key('n'));
        handle_notes(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(
            app.notes_popup.prompt,
            Some(String::new()),
            "Enter on an empty name must stay in the prompt, not close it"
        );
        assert_eq!(app.mode, Mode::Notes);
    }

    #[test]
    fn handle_notes_prompt_backspace_removes_last_char() {
        let mut app = build_app();
        app.mode = Mode::Notes;

        handle_notes(&mut app, key('n'));
        handle_notes(&mut app, key('a'));
        handle_notes(&mut app, key('b'));
        handle_notes(
            &mut app,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        );

        assert_eq!(app.notes_popup.prompt.as_deref(), Some("a"));
    }

    /// Builds an `App` with a real `notes_dir` and a task already carrying a
    /// `notes:<id>/` token pointing at a folder with two `.md` files, then
    /// opens the notes popup — mirrors `app_with_two_notes` in
    /// `src/app/notes_popup.rs`'s own test module, but built from `App::new`
    /// directly since `main.rs`'s test module (a separate crate) can't reach
    /// that lib-internal `pub(crate)` test helper.
    fn build_notes_app_with_two_files(dir: &std::path::Path) -> App {
        let notes_folder = dir.join("tasks").join("abc123");
        std::fs::create_dir_all(&notes_folder).expect("create notes folder");
        std::fs::write(notes_folder.join("a.md"), "content a").expect("write a.md");
        std::fs::write(notes_folder.join("b.md"), "content b").expect("write b.md");
        let path = std::env::temp_dir().join(format!(
            "tuxedo-notes-{}-{:?}.txt",
            std::process::id(),
            std::thread::current().id()
        ));
        let raw = "Write PR summary +work notes:abc123/\n";
        std::fs::write(&path, raw).expect("write todo.txt");
        let cfg = Config {
            notes_dir: Some(dir.to_string_lossy().into_owned()),
            ..Config::default()
        };
        let mut app = App::new(path, raw.into(), "2026-05-07".into(), cfg);
        app.open_notes_for_current();
        app
    }

    #[test]
    fn handle_notes_r_key_opens_rename_prompt_prefilled_with_current_name() {
        let mut app = build_app();
        app.notes_popup = tuxedo::app::NotesPopupState::new(vec![
            std::path::PathBuf::from("foo.md"),
            std::path::PathBuf::from("bar.md"),
        ]);
        app.mode = Mode::Notes;

        handle_notes(&mut app, key('r'));

        assert_eq!(app.notes_popup.prompt.as_deref(), Some("foo.md"));
        assert_eq!(app.mode, Mode::Notes);
    }

    #[test]
    fn handle_notes_d_key_opens_delete_confirm_and_n_returns_to_browsing() {
        let mut app = build_app();
        app.notes_popup = tuxedo::app::NotesPopupState::new(vec![
            std::path::PathBuf::from("foo.md"),
            std::path::PathBuf::from("bar.md"),
        ]);
        app.mode = Mode::Notes;

        handle_notes(&mut app, key('d'));
        assert_eq!(app.notes_popup.pending_delete, Some(0));

        handle_notes(&mut app, key('n'));
        assert!(app.notes_popup.pending_delete.is_none());

        // Not stuck in the confirm state: ordinary browsing keys work again.
        handle_notes(&mut app, key('j'));
        assert_eq!(app.notes_popup.cursor, 1);
    }

    #[test]
    fn handle_notes_d_key_on_empty_list_does_not_enter_confirm_state() {
        let mut app = build_app();
        app.notes_popup = tuxedo::app::NotesPopupState::default();
        app.mode = Mode::Notes;

        handle_notes(&mut app, key('d'));

        assert!(app.notes_popup.pending_delete.is_none());
        assert_eq!(app.mode, Mode::Notes);
    }

    #[test]
    fn handle_notes_delete_confirm_y_deletes_selected_file_and_refreshes_list() {
        let dir = std::env::temp_dir().join(format!(
            "tuxedo-notes-delete-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut app = build_notes_app_with_two_files(&dir);
        let notes_folder = app.notes_popup.folder.clone().expect("folder").dir;

        handle_notes(&mut app, key('d'));
        handle_notes(&mut app, key('y'));

        assert!(!notes_folder.join("a.md").exists());
        assert_eq!(app.notes_popup.files, vec![notes_folder.join("b.md")]);
        assert!(app.notes_popup.pending_delete.is_none());
        assert_eq!(app.mode, Mode::Notes);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn handle_notes_u_key_unlinks_selected_file_into_unlinked_dir() {
        let dir = std::env::temp_dir().join(format!(
            "tuxedo-notes-unlink-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut app = build_notes_app_with_two_files(&dir);
        let notes_folder = app.notes_popup.folder.clone().expect("folder").dir;

        handle_notes(&mut app, key('u'));

        assert!(!notes_folder.join("a.md").exists());
        assert_eq!(
            std::fs::read_to_string(dir.join("unlinked").join("a.md")).expect("unlinked file"),
            "content a"
        );
        assert_eq!(app.notes_popup.files, vec![notes_folder.join("b.md")]);
        assert_eq!(app.mode, Mode::Notes, "u never leaves the popup");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn handle_notes_rename_prompt_enter_renames_file_via_full_key_routing() {
        let dir = std::env::temp_dir().join(format!(
            "tuxedo-notes-rename-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut app = build_notes_app_with_two_files(&dir);
        let notes_folder = app.notes_popup.folder.clone().expect("folder").dir;

        handle_notes(&mut app, key('r'));
        assert_eq!(app.notes_popup.prompt.as_deref(), Some("a.md"));
        for _ in 0..5 {
            handle_notes(
                &mut app,
                KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
            );
        }
        for c in "renamed".chars() {
            handle_notes(&mut app, key(c));
        }
        handle_notes(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(!notes_folder.join("a.md").exists());
        assert!(notes_folder.join("renamed.md").exists());
        assert!(app.notes_popup.prompt.is_none());
        assert_eq!(app.mode, Mode::Notes);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- T6+T7: embedded editor wiring ------------------------------------

    #[test]
    fn handle_notes_e_key_opens_editor_in_normal_submode() {
        let dir = std::env::temp_dir().join(format!(
            "tuxedo-notes-editor-e-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut app = build_notes_app_with_two_files(&dir);

        handle_notes(&mut app, key('e'));

        let editor = app
            .notes_popup
            .active_editor
            .as_ref()
            .expect("editor opened");
        assert_eq!(editor.mode(), NoteEditorMode::Normal);
        assert_eq!(editor.lines(), &["content a"]);
        assert_eq!(app.mode, Mode::Notes, "still nested inside Mode::Notes");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn handle_notes_i_key_opens_editor_in_insert_submode() {
        let dir = std::env::temp_dir().join(format!(
            "tuxedo-notes-editor-i-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut app = build_notes_app_with_two_files(&dir);

        handle_notes(&mut app, key('i'));

        let editor = app
            .notes_popup
            .active_editor
            .as_ref()
            .expect("editor opened");
        assert_eq!(editor.mode(), NoteEditorMode::Insert);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn while_editor_is_active_list_keys_do_not_leak_to_the_list() {
        let dir = std::env::temp_dir().join(format!(
            "tuxedo-notes-editor-noleak-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut app = build_notes_app_with_two_files(&dir);
        let notes_folder = app.notes_popup.folder.clone().expect("folder").dir;

        handle_notes(&mut app, key('e'));
        assert_eq!(app.notes_popup.cursor, 0);

        // These are list-mode mnemonics (next note, rename, delete, unlink);
        // while the editor is active they must be consumed/ignored by the
        // editor's Normal sub-mode instead of reaching the list.
        handle_notes(&mut app, key('n'));
        handle_notes(&mut app, key('r'));
        handle_notes(&mut app, key('d'));
        handle_notes(&mut app, key('u'));

        assert_eq!(
            app.notes_popup.cursor, 0,
            "list cursor must be untouched by editor-mode keys"
        );
        assert_eq!(
            app.notes_popup.files.len(),
            2,
            "no file was deleted/unlinked"
        );
        assert!(notes_folder.join("a.md").exists());
        assert!(notes_folder.join("b.md").exists());
        assert!(
            app.notes_popup.prompt.is_none(),
            "no rename/create prompt must have opened"
        );
        assert!(app.notes_popup.pending_delete.is_none());
        assert!(
            app.notes_popup.active_editor.is_some(),
            "still inside the editor"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_step_esc_leaves_editor_to_list_then_list_to_mode_normal() {
        let dir = std::env::temp_dir().join(format!(
            "tuxedo-notes-editor-esc-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut app = build_notes_app_with_two_files(&dir);

        handle_notes(&mut app, key('e'));
        assert!(app.notes_popup.active_editor.is_some());

        // First Esc (editor's Normal sub-mode): back to the list, Mode::Notes
        // unchanged.
        handle_notes(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(
            app.notes_popup.active_editor.is_none(),
            "first Esc leaves the editor"
        );
        assert_eq!(
            app.mode,
            Mode::Notes,
            "first Esc must not close the whole popup"
        );

        // Second Esc (bare list): closes the whole popup.
        handle_notes(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(
            app.mode,
            Mode::Normal,
            "second Esc from the bare list closes the popup"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn esc_from_insert_submode_returns_to_editor_normal_not_the_list() {
        let dir = std::env::temp_dir().join(format!(
            "tuxedo-notes-editor-insert-esc-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut app = build_notes_app_with_two_files(&dir);

        handle_notes(&mut app, key('i'));
        assert_eq!(
            app.notes_popup
                .active_editor
                .as_ref()
                .expect("editor open")
                .mode(),
            NoteEditorMode::Insert
        );

        handle_notes(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        let editor = app
            .notes_popup
            .active_editor
            .as_ref()
            .expect("Esc from Insert must not leave the editor");
        assert_eq!(editor.mode(), NoteEditorMode::Normal);
        assert_eq!(app.mode, Mode::Notes);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Same shape as `build_notes_app_with_two_files`, but `a.md` starts
    /// empty so editor typing/save tests aren't tangled up with tracing
    /// cursor math through pre-existing content.
    fn build_notes_app_with_one_empty_file(dir: &std::path::Path) -> App {
        let notes_folder = dir.join("tasks").join("abc123");
        std::fs::create_dir_all(&notes_folder).expect("create notes folder");
        std::fs::write(notes_folder.join("a.md"), "").expect("write empty a.md");
        let path = std::env::temp_dir().join(format!(
            "tuxedo-notes-empty-{}-{:?}.txt",
            std::process::id(),
            std::thread::current().id()
        ));
        let raw = "Write PR summary +work notes:abc123/\n";
        std::fs::write(&path, raw).expect("write todo.txt");
        let cfg = Config {
            notes_dir: Some(dir.to_string_lossy().into_owned()),
            ..Config::default()
        };
        let mut app = App::new(path, raw.into(), "2026-05-07".into(), cfg);
        app.open_notes_for_current();
        app
    }

    #[test]
    fn typing_in_insert_submode_edits_the_buffer_via_full_key_routing() {
        let dir = std::env::temp_dir().join(format!(
            "tuxedo-notes-editor-type-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut app = build_notes_app_with_one_empty_file(&dir);

        handle_notes(&mut app, key('i'));
        for c in "hi".chars() {
            handle_notes(&mut app, key(c));
        }
        handle_notes(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        for c in "there".chars() {
            handle_notes(&mut app, key(c));
        }
        handle_notes(
            &mut app,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        );

        let editor = app
            .notes_popup
            .active_editor
            .as_ref()
            .expect("still editing");
        assert_eq!(editor.lines(), &["hi", "ther"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ctrl_s_saves_the_editor_buffer_to_disk_in_either_submode() {
        let dir = std::env::temp_dir().join(format!(
            "tuxedo-notes-editor-save-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut app = build_notes_app_with_one_empty_file(&dir);
        let notes_folder = app.notes_popup.folder.clone().expect("folder").dir;

        handle_notes(&mut app, key('i'));
        for c in "saved!".chars() {
            handle_notes(&mut app, key(c));
        }
        handle_notes(&mut app, ctrl('s'));

        assert_eq!(
            std::fs::read_to_string(notes_folder.join("a.md")).expect("read back"),
            "saved!\n"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- T11: pin a note to the right-docked panel ------------------------

    #[test]
    fn resolve_z_and_shift_z_resolve_to_pin_actions() {
        let mut app = build_app();
        assert_eq!(resolve(&mut app, key('z')), Some(Action::TogglePinFocus));
        assert_eq!(resolve(&mut app, key('Z')), Some(Action::ClosePinnedNote));
    }

    fn build_notes_app_with_open_editor(dir: &std::path::Path) -> App {
        let mut app = build_notes_app_with_two_files(dir);
        handle_notes(&mut app, key('e'));
        assert!(
            app.notes_popup.active_editor.is_some(),
            "precondition: editor must be open"
        );
        app
    }

    #[test]
    fn handle_key_z_pins_the_active_editor_and_closes_the_popup() {
        let dir = std::env::temp_dir().join(format!(
            "tuxedo-pin-basic-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut app = build_notes_app_with_open_editor(&dir);

        handle_key(&mut app, key('z'), &KeyBindings::default());

        assert!(
            app.notes_popup.active_editor.is_none(),
            "moved out of the popup"
        );
        assert!(app.pinned_note.is_some(), "note now pinned");
        assert!(app.pinned_focus, "newly pinned note gets focus");
        assert_eq!(app.mode, Mode::Normal, "popup closes to Mode::Normal");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn handle_key_z_with_nothing_pinned_and_no_active_editor_is_noop() {
        let mut app = build_app();

        handle_key(&mut app, key('z'), &KeyBindings::default());

        assert!(app.pinned_note.is_none());
        assert!(!app.pinned_focus);
    }

    #[test]
    fn handle_key_shift_z_with_nothing_pinned_is_noop() {
        let mut app = build_app();

        handle_key(&mut app, key('Z'), &KeyBindings::default());

        assert!(app.pinned_note.is_none());
        assert!(!app.pinned_focus);
    }

    #[test]
    fn handle_key_z_toggles_focus_off_then_back_on_while_pinned() {
        let dir = std::env::temp_dir().join(format!(
            "tuxedo-pin-toggle-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut app = build_notes_app_with_open_editor(&dir);
        handle_key(&mut app, key('z'), &KeyBindings::default()); // pin, focused
        assert!(app.pinned_focus);

        // Focused editor is in its own Normal sub-mode; `z` there is
        // intercepted by handle_key's early branch (not typed as text).
        handle_key(&mut app, key('z'), &KeyBindings::default());
        assert!(!app.pinned_focus, "focus moves back to the main app");
        assert!(
            app.pinned_note.is_some(),
            "note stays pinned, just unfocused"
        );

        // Unfocused: `z` falls through the ordinary Mode::Normal dispatch
        // and resolves via resolve_normal_key -> Action::TogglePinFocus.
        handle_key(&mut app, key('z'), &KeyBindings::default());
        assert!(app.pinned_focus, "z re-focuses the pinned note");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn handle_key_shift_z_closes_pin_while_focused() {
        let dir = std::env::temp_dir().join(format!(
            "tuxedo-pin-close-focused-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut app = build_notes_app_with_open_editor(&dir);
        handle_key(&mut app, key('z'), &KeyBindings::default()); // pin, focused
        assert!(app.pinned_focus);

        handle_key(&mut app, key('Z'), &KeyBindings::default());

        assert!(app.pinned_note.is_none(), "pin removed entirely");
        assert!(!app.pinned_focus);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn handle_key_shift_z_closes_pin_while_unfocused() {
        let dir = std::env::temp_dir().join(format!(
            "tuxedo-pin-close-unfocused-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut app = build_notes_app_with_open_editor(&dir);
        handle_key(&mut app, key('z'), &KeyBindings::default()); // pin, focused
        app.pinned_focus = false; // unfocus without closing

        handle_key(&mut app, key('Z'), &KeyBindings::default());

        assert!(app.pinned_note.is_none(), "pin removed entirely");
        assert!(!app.pinned_focus);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn while_pinned_focus_true_ordinary_main_list_keys_do_not_leak_to_the_list() {
        let dir = std::env::temp_dir().join(format!(
            "tuxedo-pin-noleak-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut app = build_notes_app_with_open_editor(&dir);
        handle_key(&mut app, key('z'), &KeyBindings::default()); // pin, focused
        let cursor_before = app.cursor;

        // Main-list mnemonics: cursor-down, add-task, open-notes. None of
        // these should reach the main list's `Action` dispatch while
        // pinned_focus is true -- they're consumed by (or ignored by) the
        // pinned note's own Normal sub-mode instead.
        handle_key(&mut app, key('j'), &KeyBindings::default());
        handle_key(&mut app, key('n'), &KeyBindings::default());
        handle_key(&mut app, key('o'), &KeyBindings::default());

        assert_eq!(
            app.cursor, cursor_before,
            "main list cursor must be untouched"
        );
        assert_eq!(
            app.mode,
            Mode::Normal,
            "must not have opened Insert or Notes"
        );
        assert!(app.pinned_focus, "still focused on the pinned note");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn while_pinned_and_unfocused_ordinary_main_list_keys_work_normally() {
        let dir = std::env::temp_dir().join(format!(
            "tuxedo-pin-unfocused-works-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut app = build_notes_app_with_open_editor(&dir);
        handle_key(&mut app, key('z'), &KeyBindings::default()); // pin, focused
        handle_key(&mut app, key('z'), &KeyBindings::default()); // unfocus
        assert!(!app.pinned_focus);

        // 'n' (BeginAdd) is an unambiguous, count-independent signal that the
        // ordinary Mode::Normal dispatch ran: it always opens Mode::Insert.
        handle_key(&mut app, key('n'), &KeyBindings::default());

        assert_eq!(
            app.mode,
            Mode::Insert,
            "main list keys must work normally while pinned but unfocused"
        );
        assert!(app.pinned_note.is_some(), "note remains pinned");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn typing_saving_and_motions_work_identically_via_the_pinned_note() {
        // Reuses the exact same handle_note_editor_normal/_insert functions
        // the floating popup uses (see NoteEditorSignal's doc comment) --
        // proven here by driving the pinned note through full handle_key
        // routing and checking the same buffer/save behavior the T6+T7
        // floating-editor tests above already proved for the popup path.
        let dir = std::env::temp_dir().join(format!(
            "tuxedo-pin-typing-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut app = build_notes_app_with_one_empty_file(&dir);
        let notes_folder = app.notes_popup.folder.clone().expect("folder").dir;
        handle_notes(&mut app, key('e')); // open in Normal sub-mode
        handle_notes(&mut app, key('z')); // pin it (Mode::Notes -> pin/focus)
        assert!(app.pinned_focus);
        assert!(app.notes_popup.active_editor.is_none());
        assert_eq!(
            app.pinned_note.as_ref().expect("pinned").mode(),
            NoteEditorMode::Normal
        );

        // `i` while pinned+focused, in the editor's own Normal sub-mode,
        // reaches the same `enter_insert` the floating popup's `i` does.
        handle_key(&mut app, key('i'), &KeyBindings::default());
        assert_eq!(
            app.pinned_note.as_ref().expect("pinned").mode(),
            NoteEditorMode::Insert
        );

        for c in "hi".chars() {
            handle_key(&mut app, key(c), &KeyBindings::default());
        }
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &KeyBindings::default(),
        );
        for c in "there".chars() {
            handle_key(&mut app, key(c), &KeyBindings::default());
        }
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
            &KeyBindings::default(),
        );

        assert_eq!(
            app.pinned_note.as_ref().expect("still pinned").lines(),
            &["hi", "ther"]
        );

        handle_key(&mut app, ctrl('s'), &KeyBindings::default());

        assert_eq!(
            std::fs::read_to_string(notes_folder.join("a.md")).expect("read back"),
            "hi\nther\n"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
