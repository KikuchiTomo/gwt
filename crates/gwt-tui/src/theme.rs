// Shared visual language for every TUI in the crate: one palette, one spinner,
// one set of text-fitting helpers. Screens that look alike should be built from
// the same pieces rather than each keeping a private copy.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

pub const POINTER: &str = "▌ ";
pub const PAD: &str = "  ";

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn spinner(frame: usize) -> &'static str {
    SPINNER[frame % SPINNER.len()]
}

pub const C_BORDER: Color = Color::DarkGray;
pub const C_TITLE: Color = Color::Magenta;
pub const C_POINTER: Color = Color::Magenta;
pub const C_MATCH: Color = Color::LightYellow;
pub const C_BRANCH: Color = Color::Yellow;
pub const C_LOCAL: Color = Color::Cyan;
pub const C_REMOTE: Color = Color::Blue;
pub const C_CREATE: Color = Color::Green;
pub const C_ERR: Color = Color::Red;

/// Result messages and other things the user is meant to actually read.
pub const C_TEXT: Color = Color::White;
/// Secondary text: help lines, hints, column filler. `Gray` rather than
/// `DarkGray` — on most themes DarkGray sits so close to the background that
/// hints become unreadable, and these lines are still meant to be read.
pub const C_DIM: Color = Color::Gray;
pub const C_PATH: Color = Color::Gray;

pub fn pad(s: &str, n: usize) -> String {
    let w = s.chars().count();
    if w >= n {
        s.to_string()
    } else {
        let mut out = String::with_capacity(n);
        out.push_str(s);
        for _ in 0..(n - w) {
            out.push(' ');
        }
        out
    }
}

/// Width-aware: truncate `s` to `n` chars with a trailing ellipsis, then right-pad.
pub fn fit(s: &str, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    let w = s.chars().count();
    if w == n {
        return s.to_string();
    }
    if w < n {
        return pad(s, n);
    }
    if n == 1 {
        return "…".into();
    }
    let mut out: String = s.chars().take(n - 1).collect();
    out.push('…');
    out
}

/// Truncate from the **left**, prepending `…` when material was dropped.
/// Paths are more recognizable by their tail than their head.
pub fn trunc_left(s: &str, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n {
        return s.to_string();
    }
    if n == 1 {
        return "…".into();
    }
    let start = chars.len() - (n - 1);
    let mut out = String::with_capacity(n);
    out.push('…');
    out.extend(&chars[start..]);
    out
}

/// Split `text` into spans so fuzzy-match positions render highlighted.
pub fn highlighted<'a>(text: &'a str, hit: &[usize], base: Color) -> Vec<Span<'a>> {
    let hit_style = Style::default()
        .fg(C_MATCH)
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    let mut spans = Vec::new();
    let mut buf = String::new();
    let mut in_hit = false;
    for (i, c) in text.chars().enumerate() {
        let now = hit.contains(&i);
        if now != in_hit && !buf.is_empty() {
            spans.push(Span::styled(
                std::mem::take(&mut buf),
                if in_hit {
                    hit_style
                } else {
                    Style::default().fg(base)
                },
            ));
        }
        in_hit = now;
        buf.push(c);
    }
    if !buf.is_empty() {
        spans.push(Span::styled(
            buf,
            if in_hit {
                hit_style
            } else {
                Style::default().fg(base)
            },
        ));
    }
    spans
}

/// Standard bordered frame used by every screen.
pub fn frame<'a>(
    title: ratatui::text::Line<'a>,
    help: ratatui::text::Line<'a>,
) -> ratatui::widgets::Block<'a> {
    use ratatui::widgets::{Block, BorderType, Borders};
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(C_BORDER))
        .title(title)
        .title_bottom(help)
}

/// `label · detail` title, as used across screens.
pub fn title_line(label: &str, detail: &str) -> ratatui::text::Line<'static> {
    ratatui::text::Line::from(vec![
        Span::raw(" "),
        Span::styled(
            label.to_string(),
            Style::default().fg(C_TITLE).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" · {detail} "), Style::default().fg(C_DIM)),
    ])
}

/// The scrolling window for a cursor inside a list of `len` rows.
pub fn visible_window(len: usize, cursor: usize, capacity: usize) -> (usize, usize) {
    if len <= capacity || capacity == 0 {
        return (0, len);
    }
    let half = capacity / 2;
    let start = cursor.saturating_sub(half).min(len - capacity);
    (start, start + capacity)
}
