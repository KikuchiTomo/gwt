mod state;
mod ui;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use gwt_core::Repo;

use crate::term::{enter_inline, leave_inline, released};
use state::{App, BaseNote, BranchPurpose, Mode, SyncOp};

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
            // A recipe with a command in it gets the terminal to itself: the
            // picker steps out of the way, the command prints where it can be
            // read, and the viewport is built again on the way back.
            if let Some(fg) = app.foreground.take() {
                released(&mut term, height, |survives| {
                    app.run_foreground(fg, survives)
                })?;
                continue;
            }
            // Counts for the metric columns land while the list is already on
            // screen; take whatever arrived before painting this frame.
            app.poll_metrics();
            term.draw(|f| ui::draw(f, &mut app))?;
            // The delete/sync animations are self-driven, not key-driven: keep
            // ticking (and redrawing) on a timer until the work finishes.
            if matches!(app.mode, Mode::Deleting { .. }) {
                app.tick_delete();
                std::thread::sleep(Duration::from_millis(70));
                continue;
            }
            if matches!(app.mode, Mode::Syncing { .. }) {
                app.tick_sync();
                std::thread::sleep(Duration::from_millis(70));
                continue;
            }
            if matches!(app.mode, Mode::Creating { .. }) {
                app.tick_create();
                std::thread::sleep(Duration::from_millis(70));
                continue;
            }
            if matches!(app.mode, Mode::CheckingBase { .. }) {
                app.tick_base_check();
                std::thread::sleep(Duration::from_millis(70));
                continue;
            }
            if matches!(app.mode, Mode::UpdatingBase { .. }) {
                app.tick_base_update();
                std::thread::sleep(Duration::from_millis(70));
                continue;
            }
            // A frame is only redrawn when something happens, so while counts
            // are still arriving we look up more often — for the few hundred
            // milliseconds that takes, and not a moment longer.
            let idle = if app.metrics_loading() { 40 } else { 250 };
            if !event::poll(Duration::from_millis(idle))? {
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
        // Deletion and sync are animated from the main loop; swallow stray keys.
        Mode::Deleting { .. }
        | Mode::Syncing { .. }
        | Mode::Creating { .. }
        | Mode::CheckingBase { .. }
        | Mode::UpdatingBase { .. } => Ok(None),
        Mode::ConfirmBasePull { .. } => {
            handle_confirm_base_pull(app, key, ctrl);
            Ok(None)
        }
        Mode::ConfirmSync { .. } => {
            handle_confirm_sync(app, key);
            Ok(None)
        }
        Mode::Conflict { .. } => handle_conflict(app, key, ctrl),
        Mode::ConfirmAction { .. } => handle_confirm_action(app, key),
        Mode::Branch { .. } => handle_branch(app, key, ctrl),
        Mode::NewName { .. } => {
            handle_new_name(app, key, ctrl);
            Ok(None)
        }
        Mode::Keys { .. } => {
            handle_keys_overlay(app, key);
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
        // Ctrl-P is nav and was consumed above; a bare p/P is pull/push.
        KeyCode::Char('p') => app.begin_sync(SyncOp::Pull),
        KeyCode::Char('P') => app.begin_sync(SyncOp::Push),
        KeyCode::Char('f') | KeyCode::Char('/') => {
            app.filter_active = true;
        }
        KeyCode::Char('?') => app.mode = Mode::Keys { scroll: 0 },
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

/// The `?` overlay is a read-only page: scroll it, or close it.
fn handle_keys_overlay(app: &mut App, key: KeyEvent) {
    let Mode::Keys { scroll } = &mut app.mode else {
        return;
    };
    match key.code {
        KeyCode::Down | KeyCode::Char('j') => *scroll = scroll.saturating_add(1),
        KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
        KeyCode::Home | KeyCode::Char('g') => *scroll = 0,
        _ => app.mode = Mode::List,
    }
}

fn handle_confirm_sync(app: &mut App, key: KeyEvent) {
    let Mode::ConfirmSync { op, path, branch } = &app.mode else {
        return;
    };
    let (op, path, branch) = (*op, path.clone(), branch.clone());
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => app.start_sync(op, path, branch),
        _ => app.mode = Mode::List,
    }
}

/// The one prompt here that defaults to *yes*: you asked for a worktree off this
/// branch, and a fast-forward is what you almost always meant. `n` still gets
/// you the branch exactly as it stands, and esc backs out of the whole thing.
fn handle_confirm_base_pull(app: &mut App, key: KeyEvent, ctrl: bool) {
    let Mode::ConfirmBasePull {
        base,
        customize_dir,
        status,
    } = &app.mode
    else {
        return;
    };
    let (base, customize_dir, branch) = (base.clone(), *customize_dir, status.branch.clone());
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
            app.begin_base_update(base, customize_dir)
        }
        KeyCode::Esc => app.mode = Mode::List,
        KeyCode::Char('c') if ctrl => app.mode = Mode::List,
        _ => app.enter_name_input(
            base,
            customize_dir,
            Some(BaseNote {
                text: gwt_core::t::base_pull_skipped(&branch),
                error: false,
            }),
        ),
    }
}

fn handle_conflict(app: &mut App, key: KeyEvent, ctrl: bool) -> Result<Option<PickerOutcome>> {
    match key.code {
        KeyCode::Esc => {
            app.mode = Mode::List;
            return Ok(None);
        }
        KeyCode::Char('c') if ctrl => {
            app.mode = Mode::List;
            return Ok(None);
        }
        KeyCode::Down | KeyCode::Tab => app.conflict_move(1),
        KeyCode::Up | KeyCode::BackTab => app.conflict_move(-1),
        KeyCode::Char('n') if ctrl => app.conflict_move(1),
        KeyCode::Char('p') if ctrl => app.conflict_move(-1),
        KeyCode::Char('j') if ctrl => app.conflict_move(1),
        KeyCode::Char('k') if ctrl => app.conflict_move(-1),
        KeyCode::Enter => return commit_conflict(app, None),
        // Every choice also has a mnemonic key, so the menu is one keystroke.
        KeyCode::Char(c) => return commit_conflict(app, Some(c)),
        _ => {}
    }
    Ok(None)
}

fn commit_conflict(app: &mut App, key: Option<char>) -> Result<Option<PickerOutcome>> {
    match app.conflict_pick(key) {
        Ok(Some(path)) => Ok(Some(PickerOutcome::ChangeDir(path))),
        Ok(None) => Ok(None),
        Err(e) => {
            app.set_error(e.to_string());
            Ok(None)
        }
    }
}

fn handle_confirm_action(app: &mut App, key: KeyEvent) -> Result<Option<PickerOutcome>> {
    let Mode::ConfirmAction {
        pending, action, ..
    } = &app.mode
    else {
        return Ok(None);
    };
    let (pending, action) = (pending.clone(), *action);
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            match app.apply_conflict_action(&pending, action) {
                Ok(Some(path)) => return Ok(Some(PickerOutcome::ChangeDir(path))),
                Ok(None) => {}
                Err(e) => app.set_error(e.to_string()),
            }
        }
        _ => app.mode = Mode::List,
    }
    Ok(None)
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
            Ok(false) => app.set_error(gwt_core::t::nothing_to_create().into()),
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
            Ok(false) => app.set_error(gwt_core::t::name_required().into()),
            Err(e) => app.set_error(e.to_string()),
        },
        KeyCode::Backspace => app.edit_new_name(|s| {
            s.pop();
        }),
        KeyCode::Char(c) => app.edit_new_name(|s| s.push(c)),
        _ => {}
    }
}
