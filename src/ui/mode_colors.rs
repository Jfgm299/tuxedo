//! Per-mode colors for the note editor (border and status-bar chip), picked
//! automatically from whatever theme is active so they stay in the theme's
//! own palette yet are as far apart from each other as that palette allows.
//!
//! Normal keeps the theme's accent. Insert, Visual and Preview are each
//! chosen from the theme's other hued colors (priorities, project, context,
//! due…) by maximizing the perceptual distance (OKLab) to the colors
//! already taken, nudged toward the hue vim statuslines use for that mode —
//! green for Insert, purple for Visual, orange for Preview — and skipping
//! grays and colors that barely stand out from the panel background.

use ratatui::style::Color;

use crate::app::NoteEditorMode;
use crate::theme::Theme;

/// The four editor mode colors of a theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModePalette {
    pub normal: Color,
    pub insert: Color,
    pub visual: Color,
    pub preview: Color,
}

impl ModePalette {
    pub fn get(&self, mode: NoteEditorMode) -> Color {
        match mode {
            NoteEditorMode::Normal => self.normal,
            NoteEditorMode::Insert => self.insert,
            NoteEditorMode::Visual | NoteEditorMode::VisualLine => self.visual,
            NoteEditorMode::Preview => self.preview,
        }
    }
}

/// Preferred hues (OKLab hue angle, degrees) for Insert, Visual, Preview.
const INSERT_HUE: f32 = 142.0; // green
const VISUAL_HUE: f32 = 310.0; // purple / magenta
const PREVIEW_HUE: f32 = 60.0; // orange / amber

/// How much landing on each mode's preferred hue weighs against raw
/// distinctness. Small, so a clearly more distinct trio wins over one with
/// the "right" hues.
const HUE_WEIGHT: f32 = 0.06;

pub fn mode_palette(theme: &Theme) -> ModePalette {
    let accent = oklab(theme.accent);
    // `Reset` means the terminal's own background, which can't be known;
    // the terminal's named colors are made to read on it, so trust them.
    let panel = (theme.panel != Color::Reset).then(|| oklab(theme.panel));
    let mut candidates: Vec<(Color, Lab)> = Vec::new();
    for c in [
        theme.pri_c,
        theme.pri_other,
        theme.context,
        theme.pri_b,
        theme.pri_d,
        theme.project,
        theme.pri_a,
        theme.due,
        theme.overdue,
        theme.today,
        theme.matched,
    ] {
        let lab = oklab(c);
        // Hued enough to read as a color, visible on the panel, and not a
        // repeat of the accent or of another candidate.
        if lab.chroma() >= 0.04
            && panel.is_none_or(|p| lab.distance(p) >= 0.2)
            && lab.distance(accent) >= 0.02
            && !candidates.iter().any(|(_, o)| o.distance(lab) < 0.02)
        {
            candidates.push((c, lab));
        }
    }

    // Every ordered trio (Insert, Visual, Preview): keep the one whose four
    // colors, with the accent, are furthest apart — its closest pair as far
    // apart as possible — lightly steered toward each mode's usual hue.
    let n = candidates.len();
    let mut best: Option<([usize; 3], f32)> = None;
    for i in 0..n {
        for v in 0..n {
            for p in 0..n {
                if i == v || i == p || v == p {
                    continue;
                }
                let labs = [accent, candidates[i].1, candidates[v].1, candidates[p].1];
                let mut spread = f32::INFINITY;
                for x in 0..4 {
                    for y in x + 1..4 {
                        spread = spread.min(labs[x].distance(labs[y]));
                    }
                }
                let off_hue = (hue_gap(labs[1].hue(), INSERT_HUE)
                    + hue_gap(labs[2].hue(), VISUAL_HUE)
                    + hue_gap(labs[3].hue(), PREVIEW_HUE))
                    / (3.0 * 180.0);
                let score = spread - HUE_WEIGHT * off_hue;
                if best.is_none_or(|(_, s)| score > s) {
                    best = Some(([i, v, p], score));
                }
            }
        }
    }
    // A theme with fewer than three usable colors falls back to the accent;
    // the chip label still names the mode.
    let color = |k: usize| best.map_or(theme.accent, |(idx, _)| candidates[idx[k]].0);
    ModePalette {
        normal: theme.accent,
        insert: color(0),
        visual: color(1),
        preview: color(2),
    }
}

/// Smallest angle between two hues, in degrees (0..=180).
fn hue_gap(a: f32, b: f32) -> f32 {
    let d = (a - b).abs() % 360.0;
    d.min(360.0 - d)
}

#[derive(Debug, Clone, Copy)]
struct Lab {
    l: f32,
    a: f32,
    b: f32,
}

impl Lab {
    fn distance(self, o: Lab) -> f32 {
        ((self.l - o.l).powi(2) + (self.a - o.a).powi(2) + (self.b - o.b).powi(2)).sqrt()
    }

    fn chroma(self) -> f32 {
        self.a.hypot(self.b)
    }

    fn hue(self) -> f32 {
        self.b.atan2(self.a).to_degrees().rem_euclid(360.0)
    }
}

/// sRGB → OKLab (Björn Ottosson's reference conversion).
fn oklab(color: Color) -> Lab {
    let (r, g, b) = rgb(color);
    let lin = |c: u8| {
        let c = f32::from(c) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (lin(r), lin(g), lin(b));
    let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    Lab {
        l: 0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        a: 1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        b: 0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    }
}

/// Approximate RGB for any ratatui color: exact for `Rgb`, the common xterm
/// defaults for named/indexed ones (themes may use the terminal's palette).
fn rgb(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => (0, 0, 0),
        Color::Red => (205, 49, 49),
        Color::Green => (13, 188, 121),
        Color::Yellow => (229, 229, 16),
        Color::Blue => (36, 114, 200),
        Color::Magenta => (188, 63, 188),
        Color::Cyan => (17, 168, 205),
        Color::Gray => (204, 204, 204),
        Color::DarkGray => (118, 118, 118),
        Color::LightRed => (241, 76, 76),
        Color::LightGreen => (35, 209, 139),
        Color::LightYellow => (245, 245, 67),
        Color::LightBlue => (59, 142, 234),
        Color::LightMagenta => (214, 112, 214),
        Color::LightCyan => (41, 184, 219),
        Color::White => (229, 229, 229),
        Color::Indexed(i) => indexed(i),
        Color::Reset => (128, 128, 128),
    }
}

/// The xterm 256-color palette.
fn indexed(i: u8) -> (u8, u8, u8) {
    const BASE: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (205, 0, 0),
        (0, 205, 0),
        (205, 205, 0),
        (0, 0, 238),
        (205, 0, 205),
        (0, 205, 205),
        (229, 229, 229),
        (127, 127, 127),
        (255, 0, 0),
        (0, 255, 0),
        (255, 255, 0),
        (92, 92, 255),
        (255, 0, 255),
        (0, 255, 255),
        (255, 255, 255),
    ];
    match i {
        0..=15 => BASE[usize::from(i)],
        16..=231 => {
            let i = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            (level(i / 36), level((i / 6) % 6), level(i % 6))
        }
        _ => {
            let v = 8 + (i - 232) * 10;
            (v, v, v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;

    /// On every built-in theme the four mode colors are clearly different
    /// from each other and visible on the panel.
    #[test]
    fn every_builtin_theme_gets_four_distinct_visible_mode_colors() {
        for t in theme::all() {
            let p = mode_palette(t);
            let colors = [p.normal, p.insert, p.visual, p.preview];
            for (i, a) in colors.iter().enumerate() {
                for b in &colors[i + 1..] {
                    let d = oklab(*a).distance(oklab(*b));
                    assert!(d >= 0.08, "{}: {a:?} vs {b:?} too close ({d:.3})", t.name);
                }
                if i > 0 && t.panel != Color::Reset {
                    assert!(
                        oklab(*a).distance(oklab(t.panel)) >= 0.2,
                        "{}: {a:?} hard to see on the panel",
                        t.name
                    );
                }
            }
        }
    }

    #[test]
    fn normal_is_the_accent_and_insert_prefers_green() {
        let p = mode_palette(&theme::MUTED);
        assert_eq!(p.normal, theme::MUTED.accent);
        assert_eq!(p.insert, theme::MUTED.pri_c, "Muted's green");
    }

    #[test]
    fn a_theme_whose_accent_is_green_still_gets_a_distinct_insert() {
        let p = mode_palette(&theme::MATRIX);
        assert_ne!(p.insert, theme::MATRIX.accent);
        assert_ne!(p.insert, theme::MATRIX.pri_c, "same green as the accent");
    }

    #[test]
    fn known_conversions() {
        let white = oklab(Color::Rgb(255, 255, 255));
        assert!((white.l - 1.0).abs() < 1e-3 && white.chroma() < 1e-3);
        let red = oklab(Color::Rgb(255, 0, 0));
        assert!((red.hue() - 29.2).abs() < 1.0, "{}", red.hue());
        assert_eq!(indexed(196), (255, 0, 0));
    }
}
