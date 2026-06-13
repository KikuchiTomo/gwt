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
            // The delete animation is self-driven, not key-driven: keep ticking
            // (and redrawing) on a timer until the batch finishes.
            if matches!(app.mode, Mode::Deleting { .. }) {
                app.tick_delete();
                std::thread::sleep(Duration::from_millis(70));
                continue;
            }
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
        Mode::List => handle_list(app, key, ctrl),
        Mode::ConfirmDelete { .. } => {
            handle_confirm_delete(app, key);
            Ok(None)
        }
        // Deletion is animated from the main loop; swallow stray keypresses.
        Mode::Deleting { .. } => Ok(None),
        Mode::Branch { .. } => handle_branch(app, key, ctrl),
        Mode::NewName { .. } => {
            handle_new_name(app, key, ctrl);
            Ok(None)
        }
        Mode::Message { .. } => {
            app.mode = Mode::List;
            Ok(None)
        }
    }
}

fn handle_list(app: &mut App, key: KeyEvent, ctrl: bool) -> Result<Option<PickerOutcome>> {
    // Navigation keys always work, even in filter mode (arrow + ctrl).
    match key.code {
        KeyCode::Down => {
            app.move_cursor(1);
            return Ok(None);
        }
        KeyCode::Up => {
            app.move_cursor(-1);
            return Ok(None);
        }
        KeyCode::Char('n') if ctrl => {
            app.move_cursor(1);
            return Ok(None);
        }
        KeyCode::Char('p') if ctrl => {
            app.move_cursor(-1);
            return Ok(None);
        }
        KeyCode::Char('j') if ctrl => {
            app.move_cursor(1);
            return Ok(None);
        }
        KeyCode::Char('k') if ctrl => {
            app.move_cursor(-1);
            return Ok(None);
        }
        // Multi-select toggle — available even while filtering (Tab isn't text).
        KeyCode::Tab => {
            app.toggle_select_current();
            app.move_cursor(1);
            return Ok(None);
        }
        KeyCode::BackTab => {
            app.toggle_select_current();
            app.move_cursor(-1);
            return Ok(None);
        }
        KeyCode::Char('c') if ctrl => return Ok(Some(PickerOutcome::Cancelled)),
        KeyCode::Enter => {
            if let Some(wt) = app.selected_worktree() {
                return Ok(Some(PickerOutcome::ChangeDir(wt.path.clone())));
            }
            return Ok(None);
        }
        _ => {}
    }

    if app.filter_active {
        match key.code {
            KeyCode::Esc => {
                app.filter.clear();
                app.filter_active = false;
                app.refilter_worktrees();
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
        }
        return Ok(None);
    }

    // NAV mode: single-letter commands.
    match key.code {
        // Esc clears the multi-selection first, then quits on a second press.
        KeyCode::Esc => {
            if app.selected.is_empty() {
                return Ok(Some(PickerOutcome::Cancelled));
            }
            app.selected.clear();
        }
        KeyCode::Char('q') => return Ok(Some(PickerOutcome::Cancelled)),
        KeyCode::Char('j') => app.move_cursor(1),
        KeyCode::Char('k') => app.move_cursor(-1),
        KeyCode::Char(' ') => {
            app.toggle_select_current();
            app.move_cursor(1);
        }
        KeyCode::Char('a') => app.toggle_select_all(),
        KeyCode::Char('g') => app.go_top(),
        KeyCode::Char('G') => app.go_bottom(),
        KeyCode::Char('d') => {
            let targets = app.delete_targets();
            if !targets.is_empty() {
                app.mode = Mode::ConfirmDelete {
                    paths: targets,
                    force: false,
                };
            }
        }
        KeyCode::Char('D') => {
            let targets = app.delete_targets();
            if !targets.is_empty() {
                app.mode = Mode::ConfirmDelete {
                    paths: targets,
                    force: true,
                };
            }
        }
        KeyCode::Char('e') | KeyCode::Char('n') => app.enter_branch_mode(BranchPurpose::NewBase)?,
        KeyCode::Char('E') | KeyCode::Char('N') => {
            app.enter_branch_mode(BranchPurpose::NewBaseWithPath)?
        }
        KeyCode::Char('r') => app.enter_branch_mode(BranchPurpose::Review)?,
        KeyCode::Char('f') | KeyCode::Char('/') => {
            app.filter_active = true;
        }
        _ => {}
    }
    Ok(None)
}

fn handle_confirm_delete(app: &mut App, key: KeyEvent) {
    let Mode::ConfirmDelete { paths, force } = &app.mode else {
        return;
    };
    let paths = paths.clone();
    let force = *force;
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => app.start_delete(paths, force),
        _ => app.mode = Mode::List,
    }
}

fn handle_branch(app: &mut App, key: KeyEvent, ctrl: bool) -> Result<Option<PickerOutcome>> {
    // Branch picker is filter-first (fzf style); typing always edits the query.
    match key.code {
        KeyCode::Esc => {
            app.mode = Mode::List;
            return Ok(None);
        }
        KeyCode::Char('c') if ctrl => {
            app.mode = Mode::List;
            return Ok(None);
        }
        KeyCode::Down => app.branch_move(1),
        KeyCode::Up => app.branch_move(-1),
        KeyCode::Char('n') if ctrl => app.branch_move(1),
        KeyCode::Char('p') if ctrl => app.branch_move(-1),
        KeyCode::Char('j') if ctrl => app.branch_move(1),
        KeyCode::Char('k') if ctrl => app.branch_move(-1),
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
    }
    Ok(None)
}

fn handle_new_name(app: &mut App, key: KeyEvent, ctrl: bool) {
    match key.code {
        KeyCode::Esc => app.back_or_cancel_new_name(),
        KeyCode::Char('c') if ctrl => app.mode = Mode::List,
        KeyCode::Enter => match app.commit_new_name() {
            Ok(true) => {}
            Ok(false) => app.set_error("name is required".into()),
            Err(e) => app.set_error(e.to_string()),
        },
        KeyCode::Backspace => app.edit_new_name(|s| {
            s.pop();
        }),
        KeyCode::Char(c) => app.edit_new_name(|s| s.push(c)),
        _ => {}
    }
}
