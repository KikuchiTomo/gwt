mod state;
mod ui;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use gwt_core::Repo;

use crate::term::{enter_inline, leave_inline};
use state::{App, BranchPurpose, Mode};

#[derive(Debug)]
pub enum PickerOutcome {
    Cancelled,
    ChangeDir(PathBuf),
}

pub fn run_picker(repo: &Repo, height: u16) -> Result<PickerOutcome> {
    let mut term = enter_inline(height)?;
    let result = (|| -> Result<PickerOutcome> {
        let mut app = App::new(repo)?;
        loop {
            term.draw(|f| ui::draw(f, &mut app))?;
            if !event::poll(Duration::from_millis(250))? {
                continue;
            }
            if let Event::Key(key) = event::read()? {
                if let Some(out) = handle_key(&mut app, key)? {
                    return Ok(out);
                }
            }
        }
    })();
    leave_inline(&mut term)?;
    result
}

fn handle_key(app: &mut App, key: KeyEvent) -> Result<Option<PickerOutcome>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match &mut app.mode {
        Mode::List => match key.code {
            KeyCode::Esc => return Ok(Some(PickerOutcome::Cancelled)),
            KeyCode::Char('c') if ctrl => return Ok(Some(PickerOutcome::Cancelled)),
            KeyCode::Char('q') if app.filter.is_empty() => {
                return Ok(Some(PickerOutcome::Cancelled));
            }
            KeyCode::Down | KeyCode::Char('n') if ctrl => app.move_cursor(1),
            KeyCode::Up | KeyCode::Char('p') if ctrl => app.move_cursor(-1),
            KeyCode::Down => app.move_cursor(1),
            KeyCode::Up => app.move_cursor(-1),
            KeyCode::Enter => {
                if let Some(wt) = app.selected_worktree() {
                    return Ok(Some(PickerOutcome::ChangeDir(wt.path.clone())));
                }
            }
            KeyCode::Char('d') if app.filter.is_empty() => {
                if let Some(wt) = app.selected_worktree() {
                    app.mode = Mode::ConfirmDelete(wt.path.clone());
                }
            }
            KeyCode::Char('e') if app.filter.is_empty() => {
                app.enter_branch_mode(BranchPurpose::New)?
            }
            KeyCode::Char('r') if app.filter.is_empty() => {
                app.enter_branch_mode(BranchPurpose::Review)?
            }
            KeyCode::Backspace => {
                app.filter.pop();
                app.refilter_worktrees();
            }
            KeyCode::Char(c) => {
                app.filter.push(c);
                app.refilter_worktrees();
            }
            _ => {}
        },
        Mode::ConfirmDelete(path) => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                let p = path.clone();
                match app.repo.remove_worktree(&p, false) {
                    Ok(()) => {
                        app.refresh_worktrees()?;
                        app.mode = Mode::List;
                    }
                    Err(e) => app.set_error(e.to_string()),
                }
            }
            _ => app.mode = Mode::List,
        },
        Mode::Branch { .. } => match key.code {
            KeyCode::Esc => app.mode = Mode::List,
            KeyCode::Char('c') if ctrl => app.mode = Mode::List,
            KeyCode::Down | KeyCode::Char('n') if ctrl => app.branch_cursor(1),
            KeyCode::Up | KeyCode::Char('p') if ctrl => app.branch_cursor(-1),
            KeyCode::Down => app.branch_cursor(1),
            KeyCode::Up => app.branch_cursor(-1),
            KeyCode::Enter => match app.commit_branch_selection() {
                Ok(true) => {}
                Ok(false) => app.set_error("nothing to create".into()),
                Err(e) => app.set_error(e.to_string()),
            },
            KeyCode::Backspace => app.edit_branch_filter(|s| {
                s.pop();
            }),
            KeyCode::Char(c) => app.edit_branch_filter(|s| s.push(c)),
            _ => {}
        },
        Mode::Message { .. } => app.mode = Mode::List,
    }
    Ok(None)
}
