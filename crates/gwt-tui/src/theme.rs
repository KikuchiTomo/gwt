// Shared visual language for every TUI in the crate: one palette, one spinner,
// one set of text-fitting helpers. Screens that look alike should be built from
// the same pieces rather than each keeping a private copy.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use unicode_width::UnicodeWidthStr;

/// Terminal columns `s` occupies. Japanese text is double-width, so column
/// alignment has to be computed in cells, never in `chars().count()`.
pub fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

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
    let w = width(s);
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

/// Truncate `s` to `n` terminal columns with a trailing ellipsis, then right-pad
/// so the next column starts exactly at `n`.
pub fn fit(s: &str, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    let w = width(s);
    if w == n {
        return s.to_string();
    }
    if w < n {
        return pad(s, n);
    }
    if n == 1 {
        return "…".into();
    }
    // Stop one cell short of the budget so the ellipsis fits; a double-width
    // char that would straddle the edge is dropped, then space-padded.
    let mut out = String::new();
    let mut used = 0usize;
    for c in s.chars() {
        let cw = width(c.encode_utf8(&mut [0u8; 4]));
        if used + cw > n - 1 {
            break;
        }
        out.push(c);
        used += cw;
    }
    out.push('…');
    pad(&out, n)
}

/// Truncate from the **left**, prepending `…` when material was dropped.
/// Paths are more recognizable by their tail than their head.
pub fn trunc_left(s: &str, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    if width(s) <= n {
        return s.to_string();
    }
    if n == 1 {
        return "…".into();
    }
    let mut tail: Vec<char> = Vec::new();
    let mut used = 0usize;
    for c in s.chars().rev() {
        let cw = width(c.encode_utf8(&mut [0u8; 4]));
        if used + cw > n - 1 {
            break;
        }
        tail.push(c);
        used += cw;
    }
    tail.reverse();
    let mut out = String::from("…");
    out.extend(tail);
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
        Span::styled(
            if detail.is_empty() {
                " ".to_string()
            } else {
                format!(" · {detail} ")
            },
            Style::default().fg(C_DIM),
        ),
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

/// One row of the `?` help overlay.
pub struct KeyRow {
    pub keys: &'static str,
    pub desc: String,
}

pub struct KeySection {
    pub title: String,
    pub rows: Vec<KeyRow>,
}

/// Render the shared `?` overlay: sections of `keys — description`.
///
/// Key names stay in ASCII in every language; they are what you physically
/// press, not prose to translate.
pub fn draw_keys(
    f: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    sections: &[KeySection],
    scroll: u16,
) {
    use ratatui::text::Line;
    use ratatui::widgets::Paragraph;

    let key_w = sections
        .iter()
        .flat_map(|s| s.rows.iter())
        .map(|r| width(r.keys))
        .max()
        .unwrap_or(10);

    let mut lines: Vec<Line> = Vec::new();
    for (i, sec) in sections.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(Span::raw("")));
        }
        lines.push(Line::from(vec![
            Span::raw(PAD),
            Span::styled(
                sec.title.clone(),
                Style::default()
                    .fg(C_TITLE)
                    .add_modifier(Modifier::UNDERLINED),
            ),
        ]));
        for r in &sec.rows {
            lines.push(Line::from(vec![
                Span::raw(PAD),
                Span::styled(pad(r.keys, key_w), Style::default().fg(C_CREATE)),
                Span::raw("  "),
                Span::styled(r.desc.clone(), Style::default().fg(C_TEXT)),
            ]));
        }
    }
    f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pads_to_terminal_columns_not_char_count() {
        // Japanese is double-width: 3 chars = 6 columns.
        assert_eq!(width("ソース"), 6);
        assert_eq!(width(pad("ソース", 10).as_str()), 10);
        assert_eq!(width(pad("abc", 10).as_str()), 10);
    }

    #[test]
    fn fit_never_exceeds_its_column_budget() {
        for s in ["ソース (<リポジトリルート>/…)", "secrets/.env", "あ", "abc"] {
            for n in 1..20 {
                let out = fit(s, n);
                assert_eq!(
                    width(&out),
                    n,
                    "fit({s:?}, {n}) produced {out:?} of width {}",
                    width(&out)
                );
            }
        }
    }

    #[test]
    fn fit_drops_a_wide_char_rather_than_splitting_it() {
        // Budget 4 = "…" plus 3 columns, but each kana costs 2, so only one
        // fits and the result is space-padded back out to 4.
        let out = fit("あいうえ", 4);
        assert_eq!(width(&out), 4);
        assert!(out.starts_with("あ"), "got {out:?}");
        assert!(out.contains('…'), "got {out:?}");
    }

    #[test]
    fn trunc_left_keeps_the_tail_within_budget() {
        for n in 1..24 {
            let out = trunc_left("/repo/日本語ディレクトリ/file.env", n);
            assert!(width(&out) <= n, "{out:?} is wider than {n}");
        }
        assert_eq!(trunc_left("short", 20), "short");
    }
}
