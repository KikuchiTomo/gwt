use std::path::{Path, PathBuf};

use gwt_core::status::WorktreeMetrics;
use gwt_core::{BranchKind, BranchRef, Worktree, WorktreeStatus};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use ratatui::Frame;

use super::state::{
    dirty_plain, path_name, remote_plain, App, BranchPurpose, ColWidths, Mode, NameStage, H_BRANCH,
    H_DIRTY, H_NAME, H_PATH, H_REMOTE, H_STASH,
};

const POINTER: &str = "▌ ";
const PAD: &str = "  ";
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn spinner(frame: usize) -> &'static str {
    SPINNER[frame % SPINNER.len()]
}

/// Per-row state while a delete batch is running, for styling the list.
#[derive(Clone, Copy)]
enum DelMark {
    Done,
    Active,
    Pending,
}
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
        Mode::List | Mode::ConfirmDelete { .. } | Mode::Deleting { .. } | Mode::Message { .. } => {
            let list_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Min(1)])
                .split(chunks[0]);
            draw_worktree_header(f, list_chunks[0], &app.cols);
            draw_worktrees(f, list_chunks[1], app);
            draw_prompt_list(f, chunks[1], app);
        }
        Mode::Branch { .. } => {
            draw_branches(f, chunks[0], app);
            draw_prompt_branch(f, chunks[1], app);
        }
        Mode::NewName {
            base,
            buf,
            dir_buf,
            customize_dir,
            stage,
        } => {
            draw_new_name(f, chunks[0], base, buf, dir_buf, *customize_dir, *stage);
            draw_prompt_new_name(f, chunks[1], buf, dir_buf, *stage);
        }
    }
}

fn draw_new_name(
    f: &mut Frame,
    area: Rect,
    base: &str,
    _buf: &str,
    _dir_buf: &str,
    customize_dir: bool,
    _stage: NameStage,
) {
    let hint = if customize_dir {
        "  → enter branch name, then worktree dir name"
    } else {
        "  → new branch name will also be the worktree dir name"
    };
    let line = Line::from(vec![
        Span::raw(PAD),
        Span::styled("branching from ", Style::default().fg(C_DIM)),
        Span::styled(
            base.to_string(),
            Style::default().fg(C_BRANCH).add_modifier(Modifier::BOLD),
        ),
        Span::styled(hint, Style::default().fg(C_DIM)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn draw_prompt_new_name(f: &mut Frame, area: Rect, buf: &str, dir_buf: &str, stage: NameStage) {
    let (label, value) = match stage {
        NameStage::Branch => (" branch ", buf),
        NameStage::Dir => (" dir ", dir_buf),
    };
    let line = Line::from(vec![
        Span::styled(
            label,
            Style::default().fg(C_POINTER).add_modifier(Modifier::BOLD),
        ),
        Span::raw("› "),
        Span::raw(value.to_string()),
        Span::styled("▏", Style::default().fg(C_POINTER)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn draw_worktree_header(f: &mut Frame, area: Rect, cols: &ColWidths) {
    let mut spans = Vec::with_capacity(12);
    spans.push(Span::raw(PAD));
    spans.push(header_span(H_NAME, cols.name));
    spans.push(Span::raw(" "));
    spans.push(header_span(H_BRANCH, cols.branch));
    if cols.show_metrics() {
        spans.push(Span::raw(" "));
        spans.push(header_span(H_REMOTE, cols.remote));
        spans.push(Span::raw(" "));
        spans.push(header_span(H_DIRTY, cols.dirty));
        spans.push(Span::raw(" "));
        spans.push(header_span(H_STASH, cols.stash));
    }
    spans.push(Span::raw(" "));
    spans.push(header_span(H_PATH, 0));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn header_span<'a>(label: &'a str, width: usize) -> Span<'a> {
    let text = if width == 0 {
        label.to_string()
    } else {
        pad(label, width)
    };
    Span::styled(
        text,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
    )
}

fn title_line(app: &App) -> Line<'static> {
    let (label, detail) = match &app.mode {
        Mode::Branch { purpose, all } => {
            let name = match purpose {
                BranchPurpose::NewBase => "new · pick base branch",
                BranchPurpose::NewBaseWithPath => "new+dir · pick base branch",
                BranchPurpose::Review => "review",
            };
            (
                name.to_string(),
                format!("{}/{}", app.filtered_branches.len(), all.len()),
            )
        }
        Mode::NewName { base, .. } => (format!("new · from {base}"), String::new()),
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
                " type:filter  esc:exit  ↑↓/^p^n/^j^k:nav  tab:select  enter:cd "
            } else if !app.selected.is_empty() {
                " tab/space:select  a:all  d:del  D:force-del  esc:clear  enter:cd "
            } else {
                " j/k ↑↓ ^j^k:nav  tab:select  a:all  enter:cd  d:del  D:force-del  e/n:new  r:review  f:filter  q:quit "
            }
        }
        Mode::ConfirmDelete { paths, force } => match (paths.len() > 1, *force) {
            (true, true) => " y: FORCE delete ALL selected   any: cancel ",
            (true, false) => " y: delete ALL selected   any: cancel ",
            (false, true) => " y: FORCE confirm   any: cancel ",
            (false, false) => " y: confirm   any: cancel ",
        },
        Mode::Deleting { .. } => " deleting… ",
        Mode::Branch { purpose, .. } => match purpose {
            BranchPurpose::NewBase => {
                " type:filter  ↑↓/^p^n:nav  enter:choose base → name  esc:back "
            }
            BranchPurpose::NewBaseWithPath => {
                " type:filter  ↑↓/^p^n:nav  enter:choose base → name → dir  esc:back "
            }
            BranchPurpose::Review => " type:filter  ↑↓/^p^n:nav  enter:create wt  esc:back ",
        },
        Mode::NewName {
            customize_dir,
            stage,
            ..
        } => match (*customize_dir, *stage) {
            (true, NameStage::Branch) => " type:branch name  enter:next (dir)  esc:cancel ",
            (true, NameStage::Dir) => " type:dir name  enter:create worktree  esc:cancel ",
            _ => " type:name  enter:create worktree  esc:cancel ",
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
    let path_budget = path_budget(area.width as usize, &app.cols);
    // During a delete batch, rows are styled by progress rather than cursor.
    let del = match &app.mode {
        Mode::Deleting {
            paths,
            index,
            frame,
            ..
        } => Some((paths.as_slice(), *index, *frame)),
        _ => None,
    };
    let lines: Vec<Line> = (start..end)
        .map(|i| {
            let scored = &app.filtered_wt[i];
            let w = &app.worktrees[scored.idx];
            let m = app.metrics.get(scored.idx).and_then(Option::as_ref);
            let mark = del.and_then(|(paths, idx, _)| del_mark(&w.path, paths, idx));
            let frame = del.map(|(_, _, fr)| fr).unwrap_or(0);
            worktree_line(
                w,
                m,
                &app.cols,
                path_budget,
                i == app.wt_cursor && del.is_none(),
                app.is_selected(scored.idx),
                mark,
                frame,
            )
        })
        .collect();
    // No wrap — overflow gets clipped, alignment stays intact.
    f.render_widget(Paragraph::new(lines), area);
}

/// Where `path` sits relative to the delete cursor, or `None` if not a target.
fn del_mark(path: &Path, paths: &[PathBuf], index: usize) -> Option<DelMark> {
    let pos = paths.iter().position(|p| p == path)?;
    Some(if pos < index {
        DelMark::Done
    } else if pos == index {
        DelMark::Active
    } else {
        DelMark::Pending
    })
}

#[allow(clippy::too_many_arguments)]
fn worktree_line(
    w: &Worktree,
    m: Option<&WorktreeMetrics>,
    cols: &ColWidths,
    path_budget: usize,
    cursor: bool,
    checked: bool,
    del: Option<DelMark>,
    frame: usize,
) -> Line<'static> {
    let mut spans = Vec::with_capacity(14);
    // Two 1-char lead columns: cursor pointer + select/delete marker.
    let (ptr, ptr_style, mark, mark_style) = lead_glyphs(cursor, checked, del, frame);
    spans.push(Span::styled(ptr, ptr_style));
    spans.push(Span::styled(mark, mark_style));
    spans.push(Span::styled(
        fit(&w.name(), cols.name),
        color_for_status(w.status),
    ));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        fit(&w.short_branch(), cols.branch),
        Style::default().fg(C_BRANCH),
    ));
    if cols.show_metrics() {
        spans.push(Span::raw(" "));
        let (rt, rc) = remote_cell(m);
        spans.push(Span::styled(fit(&rt, cols.remote), Style::default().fg(rc)));
        spans.push(Span::raw(" "));
        let (dt, dc) = dirty_cell(m);
        spans.push(Span::styled(fit(&dt, cols.dirty), Style::default().fg(dc)));
        spans.push(Span::raw(" "));
        let stash = m.map(|m| m.stash).unwrap_or(0);
        spans.push(Span::styled(
            fit(&stash.to_string(), cols.stash),
            Style::default().fg(if stash == 0 { C_DIM } else { C_BRANCH }),
        ));
    }
    spans.push(Span::raw(" "));
    let path_str = trunc_left(&w.path.display().to_string(), path_budget);
    spans.push(Span::styled(path_str, Style::default().fg(C_PATH)));
    let line_style = match del {
        Some(DelMark::Done) => Style::default()
            .fg(C_DIM)
            .add_modifier(Modifier::CROSSED_OUT),
        Some(DelMark::Active) => Style::default().add_modifier(Modifier::BOLD),
        Some(DelMark::Pending) => Style::default().fg(C_DIM),
        None if cursor => Style::default().add_modifier(Modifier::BOLD),
        None => Style::default(),
    };
    Line::from(spans).style(line_style)
}

/// The two leading glyphs: cursor pointer column, then a select/delete marker.
fn lead_glyphs(
    cursor: bool,
    checked: bool,
    del: Option<DelMark>,
    frame: usize,
) -> (&'static str, Style, &'static str, Style) {
    if let Some(mark) = del {
        let dim = Style::default().fg(C_DIM);
        return match mark {
            DelMark::Done => (" ", dim, "✓", Style::default().fg(C_CREATE)),
            DelMark::Active => (
                " ",
                dim,
                spinner(frame),
                Style::default().fg(C_ERR).add_modifier(Modifier::BOLD),
            ),
            DelMark::Pending => (" ", dim, "·", dim),
        };
    }
    let ptr = if cursor { "▌" } else { " " };
    let ptr_style = Style::default().fg(C_POINTER).add_modifier(Modifier::BOLD);
    let mark = if checked { "●" } else { " " };
    let mark_style = Style::default().fg(C_CREATE).add_modifier(Modifier::BOLD);
    (ptr, ptr_style, mark, mark_style)
}

fn remote_cell(m: Option<&WorktreeMetrics>) -> (String, Color) {
    let Some(m) = m else {
        return ("—".into(), C_DIM);
    };
    let text = remote_plain(m);
    let color = match m.ahead_behind {
        None => C_DIM,
        Some(ab) if ab.ahead == 0 && ab.behind == 0 => Color::Green,
        Some(ab) if ab.behind == 0 => C_LOCAL,
        Some(ab) if ab.ahead == 0 => C_BRANCH,
        Some(_) => C_POINTER,
    };
    (text, color)
}

fn dirty_cell(m: Option<&WorktreeMetrics>) -> (String, Color) {
    let Some(m) = m else {
        return ("?".into(), C_DIM);
    };
    let text = dirty_plain(m);
    let color = match m.dirty {
        None => C_DIM,
        Some(0) => C_DIM,
        Some(_) => C_ERR,
    };
    (text, color)
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
        Mode::ConfirmDelete { paths, force } => {
            let target = if paths.len() == 1 {
                format!("'{}'", path_name(&paths[0]))
            } else {
                format!("{} worktrees", paths.len())
            };
            Line::from(vec![
                Span::styled(
                    if *force { " FORCE delete " } else { " delete " },
                    Style::default().fg(C_ERR).add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("{target} ? y/N")),
            ])
        }
        Mode::Deleting {
            paths,
            force,
            index,
            frame,
            ..
        } => {
            let total = paths.len();
            let done = (*index).min(total);
            let cur = paths.get(*index).or_else(|| paths.last());
            let name = cur.map(|p| path_name(p)).unwrap_or_default();
            Line::from(vec![
                Span::styled(
                    format!(" {} ", spinner(*frame)),
                    Style::default().fg(C_ERR).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    if *force {
                        "FORCE deleting "
                    } else {
                        "deleting "
                    },
                    Style::default().fg(C_ERR).add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("{done}/{total}  ")),
                Span::styled(name, Style::default().fg(C_DIM)),
                Span::raw("  "),
                Span::styled(progress_bar(done, total, 12), Style::default().fg(C_CREATE)),
            ])
        }
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
            BranchPurpose::NewBase | BranchPurpose::NewBaseWithPath => "base",
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

fn progress_bar(done: usize, total: usize, width: usize) -> String {
    // total == 0 (empty batch) divides to None → treat as a full bar.
    let filled = (done * width).checked_div(total).unwrap_or(width);
    let mut bar = String::with_capacity(width + 2);
    bar.push('[');
    for i in 0..width {
        bar.push(if i < filled { '█' } else { '░' });
    }
    bar.push(']');
    bar
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

/// Width-aware: truncate `s` to `n` chars with a trailing ellipsis, then right-pad.
fn fit(s: &str, n: usize) -> String {
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
fn trunc_left(s: &str, n: usize) -> String {
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

fn path_budget(area_width: usize, cols: &ColWidths) -> usize {
    // pointer (2) + name + " " + branch + (metrics? + " " each) + " " before path.
    let mut used = 2 + cols.name + 1 + cols.branch + 1;
    if cols.show_metrics() {
        used += cols.remote + 1 + cols.dirty + 1 + cols.stash + 1;
    }
    area_width.saturating_sub(used).max(8)
}
