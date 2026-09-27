//! Rendered markdown preview for notes: parses a note with `pulldown-cmark`
//! and lays it out as themed ratatui lines for a given width — headings in
//! color with an underline rule, bullets and checkboxes, quotes with a bar,
//! shaded code blocks, box-drawn tables. Terminals have one font size, so
//! headings stand out by color, weight and rule rather than size.
//!
//! Pure function of (text, width, theme): the editor re-renders it on every
//! frame, so the preview is always current with the buffer.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::theme::Theme;

/// The rendered preview: display lines plus, for each one, the buffer line
/// its block starts on (to open the preview scrolled to where the cursor
/// was).
#[derive(Debug, Default)]
pub struct Rendered {
    pub lines: Vec<Line<'static>>,
    pub source_lines: Vec<usize>,
}

impl Rendered {
    /// The first display row of the block containing buffer line `line`.
    pub fn row_for_source_line(&self, line: usize) -> usize {
        let Some(last) = self.source_lines.iter().rposition(|&s| s <= line) else {
            return 0;
        };
        let block = self.source_lines[last];
        self.source_lines
            .iter()
            .position(|&s| s == block)
            .unwrap_or(0)
    }
}

pub fn render(text: &str, width: usize, theme: &Theme) -> Rendered {
    let width = width.max(8);
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let line_of = |byte: usize| {
        line_starts
            .partition_point(|&s| s <= byte)
            .saturating_sub(1)
    };

    let mut b = Builder::new(width, theme);
    for (event, range) in Parser::new_ext(text, options).into_offset_iter() {
        if let Event::Start(tag) = &event
            && is_block(tag)
        {
            b.src = line_of(range.start);
        }
        b.event(event);
    }
    b.flush();
    b.out
}

fn is_block(tag: &Tag) -> bool {
    matches!(
        tag,
        Tag::Paragraph
            | Tag::Heading { .. }
            | Tag::BlockQuote(_)
            | Tag::CodeBlock(_)
            | Tag::HtmlBlock
            | Tag::List(_)
            | Tag::Item
            | Tag::Table(_)
    )
}

/// A run of text with one style — the unit inline content is collected in
/// before it gets wrapped.
type Piece = (String, Style);

#[derive(Default)]
struct Table {
    rows: Vec<Vec<String>>,
    header_rows: usize,
    in_head: bool,
}

struct Builder<'t> {
    theme: &'t Theme,
    width: usize,
    out: Rendered,
    /// Buffer line of the block being built.
    src: usize,
    /// Inline content of the current block, not yet laid out.
    pieces: Vec<Piece>,
    /// Inline styles in effect (emphasis inside a link inside…).
    styles: Vec<Style>,
    /// One entry per open list: the next number for ordered lists.
    lists: Vec<Option<u64>>,
    /// Width of each open list item's marker, outermost first: the hanging
    /// indent of everything inside it.
    item_indents: Vec<usize>,
    /// Marker waiting to start the current item's first line.
    pending_marker: Option<Piece>,
    quote_depth: usize,
    heading: Option<HeadingLevel>,
    code: Option<String>,
    table: Option<Table>,
    /// Target of the link being read, if inside one.
    link: Option<String>,
    /// Put a blank line before the next block.
    gap: bool,
}

impl<'t> Builder<'t> {
    fn new(width: usize, theme: &'t Theme) -> Self {
        Self {
            theme,
            width,
            out: Rendered::default(),
            src: 0,
            pieces: Vec::new(),
            styles: Vec::new(),
            lists: Vec::new(),
            item_indents: Vec::new(),
            pending_marker: None,
            quote_depth: 0,
            heading: None,
            code: None,
            table: None,
            link: None,
            gap: false,
        }
    }

    fn base(&self) -> Style {
        Style::default().fg(self.theme.fg)
    }

    fn style(&self) -> Style {
        self.styles.last().copied().unwrap_or_else(|| self.base())
    }

    fn push_style(&mut self, f: impl FnOnce(Style) -> Style) {
        let next = f(self.style());
        self.styles.push(next);
    }

    fn text(&mut self, s: &str) {
        if let Some(code) = self.code.as_mut() {
            code.push_str(s);
        } else if let Some(table) = self.table.as_mut() {
            if let Some(cell) = table.rows.last_mut().and_then(|r| r.last_mut()) {
                cell.push_str(s);
            }
        } else {
            let mut style = self.style();
            // Underlined cells become clickable OSC 8 links whose target is
            // the cell text itself (see `ui::hyperlinks`), so only a link
            // that displays its own URL gets the underline.
            if self.link.as_deref() == Some(s) {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
            self.pieces.push((s.to_string(), style));
        }
    }

    fn event(&mut self, event: Event) {
        let theme = self.theme;
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => self.text(&t),
            Event::Code(t) => {
                if let Some(table) = self.table.as_mut() {
                    if let Some(cell) = table.rows.last_mut().and_then(|r| r.last_mut()) {
                        cell.push_str(&t);
                    }
                } else {
                    let style = self.style().fg(theme.context).bg(theme.cursor);
                    self.pieces.push((t.to_string(), style));
                }
            }
            Event::SoftBreak => self.text(" "),
            Event::HardBreak => self.flush(),
            Event::Rule => {
                self.flush();
                self.blank_if_needed();
                let width = self.width.saturating_sub(self.indent_width());
                let rule = "─".repeat(width);
                self.emit(vec![(rule, Style::default().fg(theme.dim))]);
                self.gap = true;
            }
            Event::TaskListMarker(done) => {
                let (glyph, color) = if done {
                    ("☑ ", theme.pri_c)
                } else {
                    ("☐ ", theme.dim)
                };
                self.pending_marker = Some((glyph.to_string(), Style::default().fg(color)));
                if let Some(w) = self.item_indents.last_mut() {
                    *w = glyph.width();
                }
                if done {
                    // Finished tasks read as done: dimmed and struck through.
                    self.push_style(|_| {
                        Style::default()
                            .fg(theme.done)
                            .add_modifier(Modifier::CROSSED_OUT)
                    });
                }
            }
            Event::Html(t) | Event::InlineHtml(t) => {
                let style = Style::default().fg(theme.dim);
                self.pieces
                    .push((t.trim_end_matches('\n').to_string(), style));
                if event_kind_is_block(&t) {
                    self.flush();
                }
            }
            Event::FootnoteReference(t) => self.text(&format!("[^{t}]")),
            Event::InlineMath(t) | Event::DisplayMath(t) => self.text(&t),
        }
    }

    fn start(&mut self, tag: Tag) {
        let theme = self.theme;
        match tag {
            Tag::Paragraph => {
                self.flush();
                self.blank_if_needed();
            }
            Tag::Heading { level, .. } => {
                self.flush();
                self.blank_if_needed();
                self.heading = Some(level);
                let color = match level {
                    HeadingLevel::H1 => theme.accent,
                    HeadingLevel::H2 => theme.project,
                    HeadingLevel::H3 => theme.context,
                    _ => theme.fg,
                };
                self.push_style(|_| Style::default().fg(color).add_modifier(Modifier::BOLD));
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.blank_if_needed();
                self.quote_depth += 1;
                self.push_style(|s| s.fg(theme.dim).add_modifier(Modifier::ITALIC));
            }
            Tag::CodeBlock(kind) => {
                self.flush();
                self.blank_if_needed();
                if let CodeBlockKind::Fenced(lang) = kind
                    && !lang.is_empty()
                {
                    self.emit(vec![(format!(" {lang}"), Style::default().fg(theme.dim))]);
                }
                self.code = Some(String::new());
            }
            Tag::List(start) => {
                self.flush();
                if self.lists.is_empty() {
                    self.blank_if_needed();
                }
                self.lists.push(start);
                // A nested list doesn't inherit its parent item's text style
                // (a ticked task's strike-through), only a quote's.
                let style = if self.quote_depth > 0 {
                    self.base().fg(theme.dim).add_modifier(Modifier::ITALIC)
                } else {
                    self.base()
                };
                self.styles.push(style);
            }
            Tag::Item => {
                self.flush();
                let depth = self.lists.len().saturating_sub(1);
                let marker = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        let m = format!("{n}. ");
                        *n += 1;
                        (m, Style::default().fg(theme.accent))
                    }
                    _ => {
                        let bullet = ["• ", "◦ ", "▪ "][depth % 3];
                        (bullet.to_string(), Style::default().fg(theme.accent))
                    }
                };
                self.item_indents.push(marker.0.width());
                self.pending_marker = Some(marker);
                // A style slot for this item, so a checked task's strike-
                // through ends with the item.
                self.styles.push(self.style());
            }
            Tag::Table(_) => {
                self.flush();
                self.blank_if_needed();
                self.table = Some(Table::default());
            }
            Tag::TableHead => {
                if let Some(t) = self.table.as_mut() {
                    t.in_head = true;
                    t.rows.push(Vec::new());
                }
            }
            Tag::TableRow => {
                if let Some(t) = self.table.as_mut() {
                    t.rows.push(Vec::new());
                }
            }
            Tag::TableCell => {
                if let Some(row) = self.table.as_mut().and_then(|t| t.rows.last_mut()) {
                    row.push(String::new());
                }
            }
            Tag::Emphasis => self.push_style(|s| s.add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.push_style(|s| s.add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => self.push_style(|s| s.add_modifier(Modifier::CROSSED_OUT)),
            Tag::Link { dest_url, .. } => {
                self.link = Some(dest_url.to_string());
                self.push_style(|s| s.fg(theme.accent));
            }
            Tag::Image { .. } => {
                self.pieces
                    .push(("▣ ".to_string(), Style::default().fg(theme.dim)));
                self.push_style(|_| {
                    Style::default()
                        .fg(theme.dim)
                        .add_modifier(Modifier::ITALIC)
                });
            }
            Tag::HtmlBlock => {
                self.flush();
                self.blank_if_needed();
            }
            _ => self.styles.push(self.style()),
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush();
                self.gap = true;
            }
            TagEnd::Heading(level) => {
                self.styles.pop();
                let text_width: usize = self.pieces.iter().map(|(t, _)| t.width()).sum();
                self.flush();
                let rule = match level {
                    HeadingLevel::H1 => Some('━'),
                    HeadingLevel::H2 => Some('─'),
                    _ => None,
                };
                if let Some(c) = rule {
                    let avail = self.width.saturating_sub(self.indent_width());
                    let len = text_width.clamp(1, avail.max(1));
                    let color = if level == HeadingLevel::H1 {
                        self.theme.accent
                    } else {
                        self.theme.dim
                    };
                    self.emit(vec![(
                        c.to_string().repeat(len),
                        Style::default().fg(color),
                    )]);
                }
                self.heading = None;
                self.gap = true;
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.styles.pop();
                self.quote_depth = self.quote_depth.saturating_sub(1);
                self.gap = true;
            }
            TagEnd::CodeBlock => {
                let code = self.code.take().unwrap_or_default();
                self.emit_code(&code);
                self.gap = true;
            }
            TagEnd::List(_) => {
                self.flush();
                self.styles.pop();
                self.lists.pop();
                if self.lists.is_empty() {
                    self.gap = true;
                }
            }
            TagEnd::Item => {
                self.flush();
                self.styles.pop();
                self.item_indents.pop();
                self.pending_marker = None;
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    self.emit_table(table);
                }
                self.gap = true;
            }
            TagEnd::TableHead => {
                if let Some(t) = self.table.as_mut() {
                    t.in_head = false;
                    t.header_rows = t.rows.len();
                }
            }
            TagEnd::TableRow | TagEnd::TableCell => {}
            TagEnd::HtmlBlock => {
                self.flush();
                self.gap = true;
            }
            TagEnd::Link => {
                self.link = None;
                self.styles.pop();
            }
            _ => {
                self.styles.pop();
            }
        }
    }

    /// Before a new block at the top level (not between the items of a
    /// list), leave one blank line after the previous block.
    fn blank_if_needed(&mut self) {
        if self.gap && !self.out.lines.is_empty() && self.item_indents.is_empty() {
            // The gap belongs to the block above it, not the one it precedes.
            let prev = self.out.source_lines.last().copied().unwrap_or(0);
            let quote_bars = (0..self.quote_depth)
                .map(|_| Span::styled("│ ", Style::default().fg(self.theme.accent)))
                .collect::<Vec<_>>();
            self.out.lines.push(Line::from(quote_bars));
            self.out.source_lines.push(prev);
        }
        self.gap = false;
    }

    /// Columns taken by list nesting (not counting quote bars).
    fn indent_width(&self) -> usize {
        self.item_indents.iter().sum::<usize>() + self.quote_depth * 2
    }

    /// The prefix of the next output line: quote bars, then list
    /// indentation — or, on an item's first line, its marker.
    fn prefix(&mut self) -> Vec<Piece> {
        let mut prefix = Vec::new();
        for _ in 0..self.quote_depth {
            prefix.push(("│ ".to_string(), Style::default().fg(self.theme.accent)));
        }
        let total: usize = self.item_indents.iter().sum();
        match self.pending_marker.take() {
            Some(marker) => {
                let outer = total - self.item_indents.last().copied().unwrap_or(0);
                prefix.push((" ".repeat(outer), Style::default()));
                prefix.push(marker);
            }
            None => prefix.push((" ".repeat(total), Style::default())),
        }
        prefix
    }

    fn emit(&mut self, pieces: Vec<Piece>) {
        let mut spans: Vec<Span<'static>> = self.prefix().into_iter().map(to_span).collect();
        spans.extend(pieces.into_iter().map(to_span));
        self.out.lines.push(Line::from(spans));
        self.out.source_lines.push(self.src);
    }

    /// Lay out the pending inline content, word-wrapped to the width left
    /// after the prefix.
    fn flush(&mut self) {
        if self.pieces.is_empty() {
            return;
        }
        let pieces = std::mem::take(&mut self.pieces);
        let avail = self.width.saturating_sub(self.indent_width()).max(4);
        for line in wrap(&pieces, avail) {
            self.emit(line);
        }
    }

    fn emit_code(&mut self, code: &str) {
        let style = Style::default().fg(self.theme.fg).bg(self.theme.cursor);
        let avail = self.width.saturating_sub(self.indent_width()).max(4);
        let inner = avail.saturating_sub(2).max(1);
        for line in code.trim_end_matches('\n').split('\n') {
            let line = line.replace('\t', "    ");
            for chunk in hard_split(&line, inner) {
                let pad = inner.saturating_sub(chunk.width());
                self.emit(vec![(format!(" {chunk}{} ", " ".repeat(pad)), style)]);
            }
        }
    }

    fn emit_table(&mut self, table: Table) {
        let theme = self.theme;
        let cols = table.rows.iter().map(Vec::len).max().unwrap_or(0);
        if cols == 0 {
            return;
        }
        let mut widths = vec![1usize; cols];
        for row in &table.rows {
            for (i, cell) in row.iter().enumerate() {
                widths[i] = widths[i].max(cell.trim().width());
            }
        }
        // Borders and one space of padding each side of every cell.
        let chrome = cols * 3 + 1;
        let avail = self
            .width
            .saturating_sub(self.indent_width())
            .saturating_sub(chrome)
            .max(cols);
        while widths.iter().sum::<usize>() > avail {
            let Some(widest) = (0..cols).max_by_key(|&i| widths[i]) else {
                break;
            };
            if widths[widest] <= 1 {
                break;
            }
            widths[widest] -= 1;
        }
        let border = Style::default().fg(theme.dim);
        let rule = |left: &str, mid: &str, right: &str| -> Vec<Piece> {
            let mut s = left.to_string();
            for (i, w) in widths.iter().enumerate() {
                s.push_str(&"─".repeat(w + 2));
                s.push_str(if i + 1 == cols { right } else { mid });
            }
            vec![(s, border)]
        };
        self.emit(rule("┌", "┬", "┐"));
        for (r, row) in table.rows.iter().enumerate() {
            let header = r < table.header_rows;
            let cell_style = if header {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.fg)
            };
            let mut pieces: Vec<Piece> = vec![("│".to_string(), border)];
            for (i, w) in widths.iter().enumerate() {
                let text = row.get(i).map_or("", |c| c.trim());
                let text = truncate(text, *w);
                let pad = w - text.width();
                pieces.push((format!(" {text}{} ", " ".repeat(pad)), cell_style));
                pieces.push(("│".to_string(), border));
            }
            self.emit(pieces);
            if header && r + 1 == table.header_rows {
                self.emit(rule("├", "┼", "┤"));
            }
        }
        self.emit(rule("└", "┴", "┘"));
    }
}

/// HTML blocks end with a newline; inline HTML doesn't.
fn event_kind_is_block(html: &str) -> bool {
    html.ends_with('\n')
}

fn to_span((text, style): Piece) -> Span<'static> {
    Span::styled(text, style)
}

/// Word-wrap styled pieces into lines at most `width` columns wide. Breaks
/// at spaces; a word wider than a whole line is split. Spaces at the start
/// of a wrapped line are dropped.
fn wrap(pieces: &[Piece], width: usize) -> Vec<Vec<Piece>> {
    // Split every piece into words and runs of spaces, keeping styles.
    let mut tokens: Vec<(String, Style, bool)> = Vec::new();
    for (text, style) in pieces {
        let mut current = String::new();
        let mut current_space = false;
        for c in text.chars() {
            let space = c == ' ';
            if !current.is_empty() && space != current_space {
                tokens.push((std::mem::take(&mut current), *style, current_space));
            }
            current_space = space;
            current.push(c);
        }
        if !current.is_empty() {
            tokens.push((current, *style, current_space));
        }
    }

    let mut lines: Vec<Vec<Piece>> = vec![Vec::new()];
    let mut used = 0;
    // Consecutive non-space tokens (possibly differently styled) form one
    // word for wrapping purposes.
    let mut i = 0;
    while i < tokens.len() {
        if tokens[i].2 {
            let (text, style, _) = &tokens[i];
            if used > 0 && used + text.width() <= width {
                lines
                    .last_mut()
                    .expect("non-empty")
                    .push((text.clone(), *style));
                used += text.width();
            }
            i += 1;
            continue;
        }
        let end = (i..tokens.len())
            .find(|&j| tokens[j].2)
            .unwrap_or(tokens.len());
        let word_width: usize = tokens[i..end].iter().map(|t| t.0.width()).sum();
        if used > 0 && used + word_width > width {
            // Drop trailing spaces on the line being closed.
            if let Some(last) = lines.last_mut() {
                while last.last().is_some_and(|(t, _)| t.trim().is_empty()) {
                    last.pop();
                }
            }
            lines.push(Vec::new());
            used = 0;
        }
        for (text, style, _) in &tokens[i..end] {
            if used + text.width() <= width {
                lines
                    .last_mut()
                    .expect("non-empty")
                    .push((text.clone(), *style));
                used += text.width();
                continue;
            }
            // Only reachable for a word wider than a whole line.
            for chunk in hard_split_from(text, width, used) {
                if used >= width {
                    lines.push(Vec::new());
                    used = 0;
                }
                used += chunk.width();
                lines.last_mut().expect("non-empty").push((chunk, *style));
            }
        }
        i = end;
    }
    lines
}

/// Split `s` into chunks of at most `width` columns.
fn hard_split(s: &str, width: usize) -> Vec<String> {
    hard_split_from(s, width, 0)
}

/// Like [`hard_split`], but the first chunk only gets `width - used`.
fn hard_split_from(s: &str, width: usize, used: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut room = width.saturating_sub(used).max(1);
    for c in s.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if current.width() + w > room && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
            room = width.max(1);
        }
        current.push(c);
    }
    if !current.is_empty() || chunks.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// Cut `s` to `width` columns, ending in `…` when anything was cut.
fn truncate(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    let mut out = String::new();
    for c in s.chars() {
        if out.width() + unicode_width::UnicodeWidthChar::width(c).unwrap_or(0) + 1 > width {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::MUTED;

    fn plain(r: &Rendered) -> Vec<String> {
        r.lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn headings_get_rules_and_no_hash_marks() {
        let r = render("# Title\n\n## Sub\n\ntext", 40, &MUTED);
        assert_eq!(plain(&r), ["Title", "━━━━━", "", "Sub", "───", "", "text"]);
        let title = &r.lines[0].spans[1];
        assert!(title.style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(title.style.fg, Some(MUTED.accent));
    }

    #[test]
    fn lists_tasks_and_nesting() {
        let md = "- [ ] open\n- [x] done\n  - child\n1. one\n2. two\n";
        let r = render(md, 40, &MUTED);
        assert_eq!(
            plain(&r),
            ["☐ open", "☑ done", "  ◦ child", "", "1. one", "2. two"]
        );
        let done_text = r.lines[1]
            .spans
            .iter()
            .find(|s| s.content.contains("done"))
            .expect("done span");
        assert!(done_text.style.add_modifier.contains(Modifier::CROSSED_OUT));
        let child = r.lines[2]
            .spans
            .iter()
            .find(|s| s.content.contains("child"));
        assert!(
            !child
                .expect("child")
                .style
                .add_modifier
                .contains(Modifier::CROSSED_OUT),
            "a ticked item's sub-items aren't ticked themselves"
        );
    }

    #[test]
    fn paragraphs_wrap_with_hanging_list_indent() {
        let r = render("- one two three four five six", 16, &MUTED);
        assert_eq!(plain(&r), ["• one two three", "  four five six"]);
    }

    #[test]
    fn inline_styles() {
        let r = render("**bold** *it* ~~no~~ `code` [link](http://x)", 60, &MUTED);
        let spans = &r.lines[0].spans;
        let find = |t: &str| spans.iter().find(|s| s.content == t).expect(t).style;
        assert!(find("bold").add_modifier.contains(Modifier::BOLD));
        assert!(find("it").add_modifier.contains(Modifier::ITALIC));
        assert!(find("no").add_modifier.contains(Modifier::CROSSED_OUT));
        assert_eq!(find("code").bg, Some(MUTED.cursor));
        assert_eq!(find("link").fg, Some(MUTED.accent));
        assert!(
            !find("link").add_modifier.contains(Modifier::UNDERLINED),
            "underline means 'this text is a URL' to the OSC 8 overlay"
        );
        let autolink = render("<https://example.com>", 60, &MUTED);
        let url = autolink.lines[0]
            .spans
            .iter()
            .find(|s| s.content == "https://example.com");
        assert!(
            url.expect("url")
                .style
                .add_modifier
                .contains(Modifier::UNDERLINED)
        );
        assert_eq!(
            plain(&r),
            ["bold it no code link"],
            "no markdown syntax left"
        );
    }

    #[test]
    fn quotes_code_and_rules() {
        let md = "> quoted text\n\n```rust\nfn main() {}\n```\n\n---\n";
        let r = render(md, 20, &MUTED);
        let p = plain(&r);
        assert_eq!(p[0], "│ quoted text");
        assert_eq!(p[2], " rust");
        assert_eq!(p[3], " fn main() {}");
        assert_eq!(p[5], "─".repeat(20));
    }

    #[test]
    fn tables_are_box_drawn_and_fit_the_width() {
        let md = "| Día | Plan |\n|---|---|\n| Viernes | Alfama y miradores |\n";
        let r = render(md, 24, &MUTED);
        let p = plain(&r);
        assert!(p[0].starts_with('┌') && p[0].ends_with('┐'), "{p:?}");
        assert!(p[1].contains("Día") && p[1].contains("Plan"));
        assert!(p[2].starts_with('├'));
        assert!(p[3].contains("Viernes") && p[3].contains('…'), "{p:?}");
        assert!(p.iter().all(|l| l.width() <= 24), "{p:?}");
    }

    #[test]
    fn source_lines_map_rows_back_to_blocks() {
        let md = "# T\n\npara one\nstill para\n\n- item";
        let r = render(md, 40, &MUTED);
        // "T", rule, "", "para one still para", "", "• item"
        assert_eq!(r.row_for_source_line(0), 0);
        assert_eq!(
            r.row_for_source_line(3),
            3,
            "mid-paragraph → paragraph start"
        );
        assert_eq!(r.row_for_source_line(5), 5);
    }

    #[test]
    fn wrap_splits_overlong_words() {
        let lines = wrap(&[("abcdefghij".to_string(), Style::default())], 4);
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.iter().map(|(t, _)| t.as_str()).collect())
            .collect();
        assert_eq!(text, ["abcd", "efgh", "ij"]);
    }
}
