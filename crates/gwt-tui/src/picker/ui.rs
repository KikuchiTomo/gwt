use gwt_core::status::WorktreeMetrics;
use gwt_core::{BranchKind, BranchRef, Worktree, WorktreeStatus};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use ratatui::Frame;

use super::state::{App, BranchPurpose, Mode};

const POINTER: &str = "▌ ";
const PAD: &str = "  ";
const C_BORDER: Color = Color::DarkGray;
const C_TITLE: Color = Color::Magenta;
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
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(C_BORDER))
        .title(title_line(app))
        .title_bottom(help_line(app));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

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
}

fn title_line(app: &App) -> Line<'static> {
    let (label, detail) = match &app.mode {
        Mode::Branch { purpose, all } => {
            let name = match purpose {
                BranchPurpose::New => "new worktree",
                BranchPurpose::Review => "review",
            };
            (
                name.to_string(),
                format!("{}/{}", app.filtered_branches.len(), all.len()),
            )
        }
        _ => (
            "git wt".to_string(),
            format!("{}/{}", app.filtered_wt.len(), app.worktrees.len()),
        ),
    };
    Line::from(vec![
        Span::raw(" "),
        Span::styled(
            label,
            Style::default().fg(C_TITLE).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" · {} ", detail), Style::default().fg(C_DIM)),
    ])
}

fn help_line(app: &App) -> Line<'static> {
    let s = match &app.mode {
        Mode::List => {
            if app.filter_active {
                " type:filter  esc:exit filter  ↑↓/^p^n:nav  enter:cd "
            } else {
                " j/k ↑↓:nav  enter:cd  d:del  e:new  r:review  f //:filter  q:quit "
            }
        }
        Mode::ConfirmDelete(_) => " y: confirm   any: cancel ",
        Mode::Branch { purpose, .. } => match purpose {
            BranchPurpose::New => " type:filter  ↑↓/^p^n:nav  enter:checkout / create  esc:back ",
            BranchPurpose::Review => " type:filter  ↑↓/^p^n:nav  enter:create wt  esc:back ",
        },
        Mode::Message { .. } => " press any key ",
    };
    Line::from(Span::styled(s, Style::default().fg(C_DIM)))
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
            let m = app.metrics.get(scored.idx).and_then(Option::as_ref);
            worktree_line(w, m, i == app.wt_cursor)
        })
        .collect();
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn worktree_line<'a>(w: &'a Worktree, m: Option<&WorktreeMetrics>, selected: bool) -> Line<'a> {
    let mut spans = Vec::with_capacity(12);
    spans.push(if selected {
        Span::styled(
            POINTER,
            Style::default().fg(C_POINTER).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::raw(PAD)
    });
    spans.push(Span::styled(pad(&w.name(), 16), color_for_status(w.status)));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        pad(&w.short_branch(), 22),
        Style::default().fg(C_BRANCH),
    ));
    if let Some(m) = m {
        spans.push(Span::raw(" "));
        let (rt, rc) = remote_cell(m);
        spans.push(Span::styled(pad(&rt, 9), Style::default().fg(rc)));
        spans.push(Span::raw(" "));
        let (dt, dc) = dirty_cell(m);
        spans.push(Span::styled(pad(&dt, 4), Style::default().fg(dc)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            pad(&m.stash.to_string(), 3),
            Style::default().fg(if m.stash == 0 { C_DIM } else { C_BRANCH }),
        ));
    }
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        w.path.display().to_string(),
        Style::default().fg(C_PATH),
    ));
    Line::from(spans).style(if selected {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    })
}

fn remote_cell(m: &WorktreeMetrics) -> (String, Color) {
    match m.ahead_behind {
        None => ("—".into(), C_DIM),
        Some(ab) if ab.ahead == 0 && ab.behind == 0 => ("=".into(), Color::Green),
        Some(ab) => {
            let txt = format!("↑{} ↓{}", ab.ahead, ab.behind);
            let color = if ab.behind == 0 {
                C_LOCAL
            } else if ab.ahead == 0 {
                C_BRANCH
            } else {
                C_POINTER
            };
            (txt, color)
        }
    }
}

fn dirty_cell(m: &WorktreeMetrics) -> (String, Color) {
    match m.dirty {
        None => ("?".into(), C_DIM),
        Some(0) => ("0".into(), C_DIM),
        Some(n) => (n.to_string(), C_ERR),
    }
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
    let line = match &app.mode {
        Mode::ConfirmDelete(p) => Line::from(vec![
            Span::styled(
                " delete ",
                Style::default().fg(C_ERR).add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "'{}' ? y/N",
                p.file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            )),
        ]),
        Mode::Message { text, error } => Line::from(vec![
            Span::styled(
                if *error { " ! " } else { " · " },
                Style::default().fg(if *error { C_ERR } else { C_DIM }),
            ),
            Span::raw(text.clone()),
        ]),
        _ => {
            if app.filter_active {
                Line::from(vec![
                    Span::styled(
                        " filter ",
                        Style::default().fg(C_POINTER).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("› "),
                    Span::raw(app.filter.clone()),
                    Span::styled("▏", Style::default().fg(C_POINTER)),
                ])
            } else {
                Line::from(Span::styled(
                    " press f or / to filter ",
                    Style::default().fg(C_DIM),
                ))
            }
        }
    };
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
    Line::from(spans).style(if selected {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    })
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
    let label = match &app.mode {
        Mode::Branch { purpose, .. } => match purpose {
            BranchPurpose::New => "new branch",
            BranchPurpose::Review => "review",
        },
        _ => return,
    };
    let line = Line::from(vec![
        Span::styled(
            format!(" {label} "),
            Style::default().fg(C_POINTER).add_modifier(Modifier::BOLD),
        ),
        Span::raw("› "),
        Span::raw(app.branch_filter.clone()),
        Span::styled("▏", Style::default().fg(C_POINTER)),
    ]);
    f.render_widget(Paragraph::new(line), area);
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
