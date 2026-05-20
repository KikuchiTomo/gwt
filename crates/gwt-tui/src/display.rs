use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode};
use gwt_core::{Repo, Worktree, WorktreeStatus};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};
use ratatui::Frame;

use crate::term::{enter_fullscreen, leave_fullscreen};

pub fn run_display(repo: &Repo, refresh_every: Duration) -> Result<()> {
    let mut term = enter_fullscreen()?;
    let result = (|| -> Result<()> {
        let mut last = Instant::now() - refresh_every;
        let mut worktrees: Vec<Worktree> = Vec::new();
        let mut error: Option<String> = None;
        loop {
            if last.elapsed() >= refresh_every {
                match repo.list_worktrees() {
                    Ok(v) => {
                        worktrees = v;
                        error = None;
                    }
                    Err(e) => error = Some(e.to_string()),
                }
                last = Instant::now();
            }
            term.draw(|f| draw(f, repo, &worktrees, error.as_deref()))?;

            if event::poll(Duration::from_millis(200))? {
                if let Event::Key(k) = event::read()? {
                    if matches!(k.code, KeyCode::Char('q') | KeyCode::Esc) {
                        return Ok(());
                    }
                }
            }
        }
    })();
    leave_fullscreen(&mut term)?;
    result
}

fn draw(f: &mut Frame, repo: &Repo, worktrees: &[Worktree], error: Option<&str>) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);

    let header = Paragraph::new(vec![
        Line::from(vec![
            Span::styled("repo  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                repo.common_dir.display().to_string(),
                Style::default().fg(Color::Cyan),
            ),
        ]),
        Line::from(vec![
            Span::styled("here  ", Style::default().fg(Color::DarkGray)),
            Span::raw(
                repo.current_worktree
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<bare>".into()),
            ),
        ]),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" git wt ─ display "),
    );
    f.render_widget(header, chunks[0]);

    let rows = worktrees.iter().map(|w| {
        let here = repo
            .current_worktree
            .as_ref()
            .map(|c| c == &w.path)
            .unwrap_or(false);
        let marker = if here { "●" } else { " " };
        Row::new(vec![
            Cell::from(marker).style(Style::default().fg(Color::Green)),
            Cell::from(w.name()),
            Cell::from(w.short_branch()).style(Style::default().fg(Color::Yellow)),
            Cell::from(status_label(w.status)).style(status_style(w.status)),
            Cell::from(w.path.display().to_string()).style(Style::default().fg(Color::DarkGray)),
        ])
    });

    let table = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Length(20),
            Constraint::Length(28),
            Constraint::Length(10),
            Constraint::Min(20),
        ],
    )
    .header(
        Row::new(vec!["", "name", "branch", "status", "path"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .block(Block::default().borders(Borders::ALL).title(" worktrees "));
    f.render_widget(table, chunks[1]);

    let footer = match error {
        Some(e) => Paragraph::new(Span::styled(
            format!("error: {}  (q to quit)", e),
            Style::default().fg(Color::Red),
        )),
        None => Paragraph::new(Span::styled(
            "q/esc: quit   (auto-refresh)",
            Style::default().fg(Color::DarkGray),
        )),
    };
    f.render_widget(footer, chunks[2]);
}

fn status_label(s: WorktreeStatus) -> &'static str {
    match s {
        WorktreeStatus::Normal => "ok",
        WorktreeStatus::Bare => "bare",
        WorktreeStatus::Detached => "detached",
        WorktreeStatus::Locked => "locked",
        WorktreeStatus::Prunable => "prunable",
    }
}

fn status_style(s: WorktreeStatus) -> Style {
    match s {
        WorktreeStatus::Normal => Style::default().fg(Color::Green),
        WorktreeStatus::Bare => Style::default().fg(Color::Blue),
        WorktreeStatus::Detached => Style::default().fg(Color::Yellow),
        WorktreeStatus::Locked => Style::default().fg(Color::Magenta),
        WorktreeStatus::Prunable => Style::default().fg(Color::Red),
    }
}
