use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use gwt_core::{Repo, Worktree};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use crate::term::{enter_inline, leave_inline, Tui};

#[derive(Debug)]
pub enum PickerOutcome {
    Cancelled,
    ChangeDir(PathBuf),
}

enum Mode {
    List,
    ConfirmDelete(usize),
    CreateInput {
        buf: String,
    },
    ReviewSelect {
        branches: Vec<String>,
        index: usize,
        list_state: ListState,
    },
    Message {
        text: String,
        error: bool,
    },
}

struct App<'a> {
    repo: &'a Repo,
    items: Vec<Worktree>,
    state: ListState,
    mode: Mode,
}

impl<'a> App<'a> {
    fn new(repo: &'a Repo) -> Result<Self> {
        let items = repo.list_worktrees()?;
        let mut state = ListState::default();
        if !items.is_empty() {
            state.select(Some(0));
        }
        Ok(Self {
            repo,
            items,
            state,
            mode: Mode::List,
        })
    }

    fn refresh(&mut self) -> Result<()> {
        self.items = self.repo.list_worktrees()?;
        if self.items.is_empty() {
            self.state.select(None);
        } else {
            let i = self.state.selected().unwrap_or(0).min(self.items.len() - 1);
            self.state.select(Some(i));
        }
        Ok(())
    }

    fn move_cursor(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }
        let len = self.items.len() as isize;
        let cur = self.state.selected().unwrap_or(0) as isize;
        let next = (cur + delta).rem_euclid(len) as usize;
        self.state.select(Some(next));
    }

    fn selected(&self) -> Option<&Worktree> {
        self.state.selected().and_then(|i| self.items.get(i))
    }
}

pub fn run_picker(repo: &Repo, height: u16) -> Result<PickerOutcome> {
    let mut term = enter_inline(height)?;
    let result = (|| -> Result<PickerOutcome> {
        let mut app = App::new(repo)?;
        loop {
            term.draw(|f| draw(f, &mut app))?;
            if !event::poll(Duration::from_millis(250))? {
                continue;
            }
            if let Event::Key(key) = event::read()? {
                if let Some(out) = handle_key(&mut app, key, &mut term)? {
                    return Ok(out);
                }
            }
        }
    })();
    leave_inline(&mut term)?;
    result
}

fn handle_key(app: &mut App, key: KeyEvent, _term: &mut Tui) -> Result<Option<PickerOutcome>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match &mut app.mode {
        Mode::List => match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(Some(PickerOutcome::Cancelled)),
            KeyCode::Down | KeyCode::Char('j') => app.move_cursor(1),
            KeyCode::Up | KeyCode::Char('k') => app.move_cursor(-1),
            KeyCode::Char('n') if ctrl => app.move_cursor(1),
            KeyCode::Char('p') if ctrl => app.move_cursor(-1),
            KeyCode::Enter => {
                if let Some(wt) = app.selected() {
                    return Ok(Some(PickerOutcome::ChangeDir(wt.path.clone())));
                }
            }
            KeyCode::Char('d') => {
                if let Some(i) = app.state.selected() {
                    app.mode = Mode::ConfirmDelete(i);
                }
            }
            KeyCode::Char('e') => app.mode = Mode::CreateInput { buf: String::new() },
            KeyCode::Char('r') => match app.repo.remote_branches() {
                Ok(branches) => {
                    let mut list_state = ListState::default();
                    if !branches.is_empty() {
                        list_state.select(Some(0));
                    }
                    app.mode = Mode::ReviewSelect {
                        branches,
                        index: 0,
                        list_state,
                    };
                }
                Err(e) => app.mode = msg_err(e.to_string()),
            },
            _ => {}
        },
        Mode::ConfirmDelete(idx) => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                let path = app.items[*idx].path.clone();
                match app.repo.remove_worktree(&path, false) {
                    Ok(()) => {
                        app.refresh()?;
                        app.mode = Mode::List;
                    }
                    Err(e) => app.mode = msg_err(e.to_string()),
                }
            }
            _ => app.mode = Mode::List,
        },
        Mode::CreateInput { buf } => match key.code {
            KeyCode::Esc => app.mode = Mode::List,
            KeyCode::Enter => {
                let branch = buf.trim().to_string();
                if branch.is_empty() {
                    app.mode = Mode::List;
                } else {
                    let path = app.repo.worktree_root().join(&branch);
                    let res = app.repo.add_worktree(&path, &branch, true);
                    match res {
                        Ok(()) => {
                            app.refresh()?;
                            app.mode = Mode::List;
                        }
                        Err(e) => app.mode = msg_err(e.to_string()),
                    }
                }
            }
            KeyCode::Backspace => {
                buf.pop();
            }
            KeyCode::Char(c) => buf.push(c),
            _ => {}
        },
        Mode::ReviewSelect {
            branches,
            index,
            list_state,
        } => match key.code {
            KeyCode::Esc | KeyCode::Char('q') => app.mode = Mode::List,
            KeyCode::Down | KeyCode::Char('j') => {
                if !branches.is_empty() {
                    *index = (*index + 1) % branches.len();
                    list_state.select(Some(*index));
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if !branches.is_empty() {
                    *index = (*index + branches.len() - 1) % branches.len();
                    list_state.select(Some(*index));
                }
            }
            KeyCode::Char('n') if ctrl => {
                if !branches.is_empty() {
                    *index = (*index + 1) % branches.len();
                    list_state.select(Some(*index));
                }
            }
            KeyCode::Char('p') if ctrl => {
                if !branches.is_empty() {
                    *index = (*index + branches.len() - 1) % branches.len();
                    list_state.select(Some(*index));
                }
            }
            KeyCode::Enter => {
                if let Some(remote) = branches.get(*index).cloned() {
                    let local = remote.split_once('/').map(|(_, b)| b).unwrap_or(&remote);
                    let path = app.repo.worktree_root().join(local);
                    match app.repo.add_worktree_from_remote(&path, &remote) {
                        Ok(()) => {
                            app.refresh()?;
                            app.mode = Mode::List;
                        }
                        Err(e) => app.mode = msg_err(e.to_string()),
                    }
                }
            }
            _ => {}
        },
        Mode::Message { .. } => app.mode = Mode::List,
    }
    Ok(None)
}

fn msg_err(text: String) -> Mode {
    Mode::Message { text, error: true }
}

fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    match &app.mode {
        Mode::List | Mode::ConfirmDelete(_) | Mode::Message { .. } => {
            draw_list(f, chunks[0], app);
        }
        Mode::CreateInput { buf } => draw_input(f, chunks[0], "new branch", buf),
        Mode::ReviewSelect {
            branches,
            list_state,
            ..
        } => draw_branches(f, chunks[0], branches, &mut list_state.clone()),
    }

    draw_status(f, chunks[1], app);
}

fn draw_list(f: &mut Frame, area: Rect, app: &mut App) {
    let items: Vec<ListItem> = app
        .items
        .iter()
        .map(|w| {
            let branch = w.short_branch();
            let path = w.path.display().to_string();
            let line = Line::from(vec![
                Span::styled(
                    format!("{:<20}", w.name()),
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw(" "),
                Span::styled(
                    format!("{:<24}", branch),
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw(" "),
                Span::styled(path, Style::default().fg(Color::DarkGray)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let title = match &app.mode {
        Mode::ConfirmDelete(i) => format!(
            " delete '{}' ? (y/N) ",
            app.items.get(*i).map(|w| w.name()).unwrap_or_default()
        ),
        Mode::Message { text, error } => {
            if *error {
                format!(" ! {} (any key) ", text)
            } else {
                format!(" {} ", text)
            }
        }
        _ => " worktrees ".to_string(),
    };

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, area, &mut app.state);
}

fn draw_input(f: &mut Frame, area: Rect, label: &str, buf: &str) {
    let p = Paragraph::new(format!("{}: {}_", label, buf))
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" new worktree (Enter=create, Esc=cancel) "),
        );
    f.render_widget(p, area);
}

fn draw_branches(f: &mut Frame, area: Rect, branches: &[String], state: &mut ListState) {
    let items: Vec<ListItem> = branches.iter().map(|b| ListItem::new(b.clone())).collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" remote branches (Enter=create wt, Esc=back) "),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, area, state);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let help = match app.mode {
        Mode::List => "↑/k ↓/j  enter:cd  d:del  e:new  r:review  q:quit",
        Mode::ConfirmDelete(_) => "y: confirm   any: cancel",
        Mode::CreateInput { .. } => "type branch name, Enter to create, Esc to cancel",
        Mode::ReviewSelect { .. } => "↑/k ↓/j  enter:create wt   q/esc:back",
        Mode::Message { .. } => "press any key",
    };
    let p = Paragraph::new(Span::styled(help, Style::default().fg(Color::DarkGray)));
    f.render_widget(p, area);
}
