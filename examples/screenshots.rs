//! Generates the SVG screenshots embedded in the README.
//!
//! Usage:
//!     cargo run --example screenshots
//!
//! Renders each scene through ratatui's `TestBackend`, then walks the
//! resulting buffer and emits an SVG: one `<rect>` per horizontal bg run,
//! one `<text>` per non-blank cell. The themes use `Color::Rgb` exclusively
//! so colors come through faithfully.

use std::fs;
use std::path::{Path, PathBuf};

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};

use tuxedo::app::{App, Density, EditorKey, Mode, NoteEditorMode, NoteEditorState, View};
use tuxedo::config::Config;
use tuxedo::sample;
use tuxedo::theme;
use tuxedo::ui;

const COLS: u16 = 130;
const ROWS: u16 = 32;
// Cell size in SVG user units. 13px font with these dims aligns acceptably
// in the common monospace fallback chain set on the <svg> root.
const CW: f32 = 8.0;
const CH: f32 = 16.0;

/// The note shown in the notes scenes: headings, a task list with a nested
/// item, a long line that soft-wraps, a quote and a table.
const NOTE: &str = "# Q2 board deck

Draft due **Wednesday**, keep it to *ten slides*.

## Outline

- [x] Revenue and burn vs. plan
- [ ] Hiring: the senior eng rubric, both open roles and the new offer bands
  - [ ] Pipeline numbers from the recruiter
- [ ] Roadmap

> Ask Dana for the churn chart.

| Slide   | Owner |
|---------|-------|
| Revenue | Me    |
| Hiring  | Sam   |
";

fn main() -> std::io::Result<()> {
    let out = PathBuf::from("docs/screenshots");
    fs::create_dir_all(&out)?;
    // Register the ready-made themes in docs/themes (as if copied into the
    // user's themes dir), so the custom-theme scene can select one.
    let (user_themes, _) = theme::load_user_themes(Path::new("docs/themes"));
    theme::init(user_themes);
    let note_dir = std::env::temp_dir().join("tuxedo-screenshots-notes");
    fs::create_dir_all(&note_dir)?;
    let note_path = note_dir.join("deck.md");
    fs::write(&note_path, NOTE)?;

    // Render every scene at Compact density so the screenshots stay
    // consistent and pack the most content per frame.
    let make = || {
        let mut app = App::new(
            PathBuf::from("/tmp/tuxedo-screenshots.txt"),
            sample::TODO_RAW.to_string(),
            "2026-05-06".to_string(),
            Config::default(),
        );
        app.prefs.density = Density::Compact;
        app
    };

    // 1. Default list view, fresh sample data, cursor on first task.
    save(&make(), &out.join("list.svg"))?;

    // 2. Archive view — completed tasks grouped by completion date.
    let mut app = make();
    app.set_view(View::Archive);
    save(&app, &out.join("archive.svg"))?;

    // 3. Help overlay.
    let mut app = make();
    app.mode = Mode::Help;
    save(&app, &out.join("help.svg"))?;

    // 4. List with an active project filter — sidebar shows the selection.
    let mut app = make();
    app.set_project_filter(Some("work".to_string()));
    save(&app, &out.join("filter.svg"))?;

    // 5. Command palette — opened mid-list with "arch" typed, showing how
    // the ranker surfaces start-of-label hits first, then word-boundary,
    // then mid-word.
    let mut app = make();
    app.command_palette.open(Mode::Normal);
    app.mode = Mode::CommandPalette;
    app.draft_set("arch".to_string());
    app.command_palette.refresh("arch");
    save(&app, &out.join("command-palette.svg"))?;

    // 6. Empty state — fresh file, cell-bowtie logo and quick-start panel.
    // Sidebars hidden so the centered panel reads as the focal point.
    let mut app = App::new(
        PathBuf::from("/tmp/tuxedo-screenshots-empty.txt"),
        String::new(),
        "2026-05-06".to_string(),
        Config::default(),
    );
    app.prefs.density = Density::Compact;
    app.prefs.layout.left = false;
    app.prefs.layout.right = false;
    save(&app, &out.join("empty.svg"))?;

    // 7. List view in every built-in theme — for the README's themes section.
    // (Terminal is skipped: it draws with the terminal's own palette, which
    // an SVG can't know.)
    for (i, t) in theme::BUILT_IN.iter().enumerate() {
        if t.name == "Terminal" {
            continue;
        }
        let mut app = make();
        app.prefs.set_theme_idx(i);
        let slug = t.name.to_lowercase().replace(' ', "-");
        save(&app, &out.join(format!("theme-{slug}.svg")))?;
    }

    // 8. Notes: the list with two starred tasks, and a note pinned to the
    // right panel, open in the vim-style editor (Insert mode, mid-list).
    let starred = || {
        let raw = sample::TODO_RAW
            .replace(
                "Finish Q2 board deck +work @laptop",
                "Finish Q2 board deck +work @laptop star:1",
            )
            .replace(
                "Draft hiring rubric for senior eng +work @laptop",
                "Draft hiring rubric for senior eng +work @laptop star:1",
            );
        let mut app = App::new(
            PathBuf::from("/tmp/tuxedo-screenshots.txt"),
            raw,
            "2026-05-06".to_string(),
            Config::default(),
        );
        app.prefs.density = Density::Compact;
        app
    };
    let keys = |s: &str| s.chars().map(EditorKey::Char).collect::<Vec<_>>();
    let pin = |app: &mut App, keys: &[EditorKey]| -> NoteEditorState {
        let mut note = NoteEditorState::load(note_path.clone(), NoteEditorMode::Normal);
        for &k in keys {
            note.normal_key(k);
        }
        app.pinned_focus = true;
        note
    };
    // `10G A` on the "Roadmap" item, then Enter and some typing: the new
    // line continues the task list by itself. INSERT mode.
    let mut app = starred();
    let mut note = pin(&mut app, &keys("10GA"));
    note.newline();
    for c in "Q3 targets".chars() {
        note.insert_char(c);
    }
    app.pinned_notes.push(note);
    save(&app, &out.join("notes-editor.svg"))?;

    // 9. The same note rendered: `M` from the editor.
    let mut app = starred();
    let note = pin(&mut app, &keys("M"));
    app.pinned_notes.push(note);
    save(&app, &out.join("notes-preview.svg"))?;

    // 9b. Live capture: the add dialog mid-typing, every phrase detected.
    let mut app = starred();
    app.mode = Mode::Insert;
    app.draft_set_insert("call anna on friday at 6pm @calls every week".to_string());
    app.live_refresh();
    save(&app, &out.join("live-capture.svg"))?;

    // 10. The help overlay's notes page.
    let mut app = starred();
    app.help_notes_page = true;
    app.mode = Mode::Help;
    save(&app, &out.join("help-notes.svg"))?;

    // 11. A custom theme from docs/themes (Catppuccin Macchiato): the note
    // in the editor with a linewise Visual selection over the task list,
    // in the theme's own mauve.
    let mut app = starred();
    if let Some(i) = theme::all()
        .iter()
        .position(|t| t.name == "Catppuccin Macchiato")
    {
        app.prefs.set_theme_idx(i);
    }
    let note = pin(&mut app, &keys("7GVjj"));
    app.pinned_notes.push(note);
    save(&app, &out.join("theme-catppuccin-macchiato.svg"))?;

    println!("wrote screenshots to {}", out.display());
    Ok(())
}

fn save(app: &App, path: &Path) -> std::io::Result<()> {
    let backend = TestBackend::new(COLS, ROWS);
    let mut terminal = Terminal::new(backend).expect("terminal init");
    terminal.draw(|f| ui::draw(f, app)).expect("draw frame");
    let svg = render_svg(terminal.backend().buffer());
    fs::write(path, svg)
}

fn render_svg(buf: &Buffer) -> String {
    let cols = buf.area.width as usize;
    let rows = buf.area.height as usize;
    let total_w = cols as f32 * CW;
    let total_h = rows as f32 * CH;

    let mut out = String::new();
    out.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" \
         viewBox=\"0 0 {tw:.0} {th:.0}\" \
         width=\"{tw:.0}\" height=\"{th:.0}\" \
         font-family=\"ui-monospace, SFMono-Regular, Menlo, Consolas, monospace\" \
         font-size=\"13\">\n",
        tw = total_w,
        th = total_h,
    ));

    // Background pass: merge horizontal runs so we emit one <rect> per run.
    for y in 0..rows {
        let mut x = 0;
        while x < cols {
            let Some(bg) = bg_hex(buf[(x as u16, y as u16)].bg) else {
                x += 1;
                continue;
            };
            let start = x;
            x += 1;
            while x < cols && bg_hex(buf[(x as u16, y as u16)].bg).as_deref() == Some(bg.as_str()) {
                x += 1;
            }
            out.push_str(&format!(
                "<rect x=\"{rx:.1}\" y=\"{ry:.1}\" width=\"{rw:.1}\" height=\"{rh:.1}\" fill=\"{c}\"/>\n",
                rx = start as f32 * CW,
                ry = y as f32 * CH,
                rw = (x - start) as f32 * CW,
                rh = CH,
                c = bg,
            ));
        }
    }

    // Foreground pass: one <text> per non-blank cell. (Could batch by run
    // for size, but per-cell positioning is simpler and avoids monospace
    // metric guesswork.)
    for y in 0..rows {
        for x in 0..cols {
            let cell = &buf[(x as u16, y as u16)];
            let sym = cell.symbol();
            if sym.is_empty() || sym == " " {
                continue;
            }
            let fg = fg_hex(cell.fg).unwrap_or_else(|| "#cccccc".into());
            let mut attrs = String::new();
            if cell.modifier.contains(Modifier::BOLD) {
                attrs.push_str(" font-weight=\"bold\"");
            }
            if cell.modifier.contains(Modifier::ITALIC) {
                attrs.push_str(" font-style=\"italic\"");
            }
            if cell.modifier.contains(Modifier::CROSSED_OUT) {
                attrs.push_str(" text-decoration=\"line-through\"");
            }
            out.push_str(&format!(
                "<text x=\"{tx:.2}\" y=\"{ty:.2}\" fill=\"{fg}\"{attrs}>{ch}</text>\n",
                tx = x as f32 * CW,
                ty = (y as f32 + 0.78) * CH,
                fg = fg,
                attrs = attrs,
                ch = escape(sym),
            ));
        }
    }

    out.push_str("</svg>\n");
    out
}

fn bg_hex(c: Color) -> Option<String> {
    if let Color::Rgb(r, g, b) = c {
        Some(format!("#{:02x}{:02x}{:02x}", r, g, b))
    } else {
        None
    }
}

fn fg_hex(c: Color) -> Option<String> {
    if let Color::Rgb(r, g, b) = c {
        Some(format!("#{:02x}{:02x}{:02x}", r, g, b))
    } else {
        None
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
