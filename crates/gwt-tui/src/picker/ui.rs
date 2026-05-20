use gwt_core::{BranchKind, BranchRef, Worktree, WorktreeStatus};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use super::state::{App, BranchPurpose, Mode};

const POINTER: &str = "▌ ";
const PAD: &str = "  ";
const C_POINTER: Color = Color::Magenta;
const C_MATCH: Color = Color::LightYellow;
const C_BRANCH: Color = Color::Yellow;
const C_LOCAL: Color = Color::Cyan;
const C_REMOTE: Color = Color::Blue;
const C_PATH: Color = Color::DarkGray;
const C_DIM: Color = Color::DarkGray;
const C_CREATE: Color = Color::Green;
const C_ERR: Color = Color::Red;

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

    match &app.mode {
        Mode::List | Mode::ConfirmDelete(_) | Mode::Message { .. } => {
            draw_worktrees(f, chunks[0], app);
            draw_prompt_list(f, chunks[1], app);
        }
        Mode::Branch { .. } => {
            draw_branches(f, chunks[0], app);
            draw_prompt_branch(f, chunks[1], app);
        }
    }
    draw_help(f, chunks[2], app);
}

fn visible_window(len: usize, cursor: usize, capacity: usize) -> (usize, usize) {
    if len <= capacity {
        return (0, len);
    }
    let half = capacity / 2;
    let start = cursor.saturating_sub(half).min(len - capacity);
    (start, start + capacity)
}

fn draw_worktrees(f: &mut Frame, area: Rect, app: &App) {
    let cap = area.height as usize;
    let (start, end) = visible_window(app.filtered_wt.len(), app.wt_cursor, cap);
    let lines: Vec<Line> = (start..end)
        .map(|i| {
            let scored = &app.filtered_wt[i];
            let w = &app.worktrees[scored.idx];
            let selected = i == app.wt_cursor;
            worktree_line(w, &scored.indices, selected)
        })
        .collect();
    let p = Paragraph::new(lines).wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn worktree_line<'a>(w: &'a Worktree, _hit_idx: &[usize], selected: bool) -> Line<'a> {
    let mut spans = Vec::with_capacity(8);
    if selected {
        spans.push(Span::styled(
            POINTER,
            Style::default().fg(C_POINTER).add_modifier(Modifier::BOLD),
        ));
    } else {
        spans.push(Span::raw(PAD));
    }
    spans.push(Span::styled(pad(&w.name(), 18), color_for_status(w.status)));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        pad(&w.short_branch(), 24),
        Style::default().fg(C_BRANCH),
    ));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        w.path.display().to_string(),
        Style::default().fg(C_PATH),
    ));
    let base = if selected {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    Line::from(spans).style(base)
}

fn color_for_status(s: WorktreeStatus) -> Style {
    match s {
        WorktreeStatus::Bare => Style::default().fg(C_DIM),
        WorktreeStatus::Detached => Style::default().fg(C_BRANCH),
        WorktreeStatus::Locked | WorktreeStatus::Prunable => Style::default().fg(C_ERR),
        WorktreeStatus::Normal => Style::default().fg(C_LOCAL),
    }
}

fn draw_prompt_list(f: &mut Frame, area: Rect, app: &App) {
    let count = format!("{}/{}", app.filtered_wt.len(), app.worktrees.len());
    let (label, query, style) = match &app.mode {
        Mode::ConfirmDelete(p) => (
            "delete".to_string(),
            format!(
                "'{}' ? y/N",
                p.file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ),
            Style::default().fg(C_ERR).add_modifier(Modifier::BOLD),
        ),
        Mode::Message { text, error } => (
            if *error {
                "!".to_string()
            } else {
                "·".to_string()
            },
            text.clone(),
            Style::default().fg(if *error { C_ERR } else { C_DIM }),
        ),
        _ => (
            "filter".to_string(),
            format!("{}{}", app.filter, "▏"),
            Style::default().fg(C_POINTER),
        ),
    };
    let line = Line::from(vec![
        Span::styled(format!("{:>6} ", count), Style::default().fg(C_DIM)),
        Span::styled(format!("{}: ", label), style),
        Span::raw(query),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn draw_branches(f: &mut Frame, area: Rect, app: &App) {
    let cap = area.height as usize;
    let total = app.branch_total();
    let (start, end) = visible_window(total, app.branch_cursor, cap);
    let mut lines = Vec::with_capacity(end - start);

    for i in start..end {
        let selected = i == app.branch_cursor;
        if i < app.filtered_branches.len() {
            let scored = &app.filtered_branches[i];
            let Mode::Branch { all, .. } = &app.mode else {
                continue;
            };
            let b = &all[scored.idx];
            lines.push(branch_line(b, &scored.indices, selected));
        } else {
            lines.push(create_line(&app.branch_filter, selected));
        }
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn branch_line<'a>(b: &'a BranchRef, hit: &[usize], selected: bool) -> Line<'a> {
    let mut spans = Vec::with_capacity(4);
    spans.push(Span::styled(
        if selected { POINTER } else { PAD },
        Style::default().fg(C_POINTER).add_modifier(Modifier::BOLD),
    ));
    spans.extend(highlighted(&b.short, hit, branch_base_color(&b.kind)));
    spans.push(Span::raw("  "));
    spans.push(Span::styled(
        kind_label(&b.kind),
        Style::default().fg(C_DIM),
    ));
    let base = if selected {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    Line::from(spans).style(base)
}

fn highlighted<'a>(text: &'a str, hit: &[usize], base: Color) -> Vec<Span<'a>> {
    let mut spans = Vec::new();
    let mut buf = String::new();
    let mut in_hit = false;
    for (i, c) in text.chars().enumerate() {
        let now = hit.contains(&i);
        if now != in_hit && !buf.is_empty() {
            spans.push(Span::styled(
                std::mem::take(&mut buf),
                if in_hit {
                    Style::default()
                        .fg(C_MATCH)
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
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
                Style::default()
                    .fg(C_MATCH)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else {
                Style::default().fg(base)
            },
        ));
    }
    spans
}

fn create_line(query: &str, selected: bool) -> Line<'_> {
    Line::from(vec![
        Span::styled(
            if selected { POINTER } else { PAD },
            Style::default().fg(C_CREATE).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("[+ create '{}' ]", query.trim()),
            Style::default()
                .fg(C_CREATE)
                .add_modifier(Modifier::ITALIC | Modifier::BOLD),
        ),
    ])
}

fn branch_base_color(k: &BranchKind) -> Color {
    match k {
        BranchKind::Local => C_LOCAL,
        BranchKind::Remote { .. } => C_REMOTE,
    }
}

fn kind_label(k: &BranchKind) -> String {
    match k {
        BranchKind::Local => "local".into(),
        BranchKind::Remote { remote } => format!("remote · {remote}"),
    }
}

fn draw_prompt_branch(f: &mut Frame, area: Rect, app: &App) {
    let count = format!("{}/{}", app.filtered_branches.len(), branch_count(app));
    let purpose = match &app.mode {
        Mode::Branch { purpose, .. } => *purpose,
        _ => return,
    };
    let label = match purpose {
        BranchPurpose::New => "new branch",
        BranchPurpose::Review => "review",
    };
    let line = Line::from(vec![
        Span::styled(format!("{:>6} ", count), Style::default().fg(C_DIM)),
        Span::styled(
            format!("{label}: "),
            Style::default().fg(C_POINTER).add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("{}{}", app.branch_filter, "▏")),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn branch_count(app: &App) -> usize {
    match &app.mode {
        Mode::Branch { all, .. } => all.len(),
        _ => 0,
    }
}

fn draw_help(f: &mut Frame, area: Rect, app: &App) {
    let help = match &app.mode {
        Mode::List => "↑/↓ ^p/^n   enter:cd   d:del   e:new   r:review   esc:quit",
        Mode::ConfirmDelete(_) => "y: confirm   any: cancel",
        Mode::Branch { purpose, .. } => match purpose {
            BranchPurpose::New => "↑/↓ ^p/^n   enter:create wt   type to filter / new   esc:back",
            BranchPurpose::Review => "↑/↓ ^p/^n   enter:checkout for review   esc:back",
        },
        Mode::Message { .. } => "press any key",
    };
    f.render_widget(
        Paragraph::new(Span::styled(help, Style::default().fg(C_DIM))),
        area,
    );
}

fn pad(s: &str, n: usize) -> String {
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
