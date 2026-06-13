use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use gwt_core::layout::BareLayout;
use gwt_core::status::{self, WorktreeMetrics};
use gwt_core::{ops, BranchKind, BranchRef, Repo, Worktree};

use crate::fuzzy;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchPurpose {
    NewBase,
    NewBaseWithPath,
    Review,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameStage {
    Branch,
    Dir,
}

pub enum Mode {
    List,
    ConfirmDelete {
        paths: Vec<PathBuf>,
        force: bool,
    },
    /// In-progress bulk delete: one worktree is removed every `DELETE_STEPS`
    /// ticks while a spinner animates, so the operation reads as live progress.
    Deleting {
        paths: Vec<PathBuf>,
        force: bool,
        index: usize,
        frame: usize,
        step: usize,
        errors: Vec<String>,
    },
    Branch {
        purpose: BranchPurpose,
        all: Vec<BranchRef>,
    },
    NewName {
        base: String,
        buf: String,
        dir_buf: String,
        customize_dir: bool,
        stage: NameStage,
    },
    Message {
        text: String,
        error: bool,
    },
}

#[derive(Default, Clone)]
pub struct Scored {
    pub idx: usize,
    pub score: i32,
    pub indices: Vec<usize>,
}

#[derive(Debug, Clone)]
pub struct ColWidths {
    pub name: usize,
    pub branch: usize,
    pub remote: usize,
    pub dirty: usize,
    pub stash: usize,
}

impl ColWidths {
    pub fn show_metrics(&self) -> bool {
        // 0 is the sentinel meaning "no metrics available for any row".
        self.remote > 0 || self.dirty > 0 || self.stash > 0
    }
}

pub struct App<'a> {
    pub repo: &'a Repo,
    pub layout: Option<BareLayout>,
    pub mode: Mode,

    pub worktrees: Vec<Worktree>,
    pub metrics: Vec<Option<WorktreeMetrics>>,
    pub cols: ColWidths,
    pub filter: String,
    pub filter_active: bool,
    pub filtered_wt: Vec<Scored>,
    pub wt_cursor: usize,
    /// Multi-select set, keyed by absolute index into `worktrees`.
    pub selected: HashSet<usize>,

    pub branch_filter: String,
    pub filtered_branches: Vec<Scored>,
    pub branch_cursor: usize,
}

impl<'a> App<'a> {
    pub fn new(repo: &'a Repo) -> Result<Self> {
        let worktrees = repo.list_worktrees()?;
        let layout = BareLayout::require(&repo.cwd).ok();
        let metrics = compute_metrics(layout.as_ref(), &worktrees);
        let cols = compute_col_widths(&worktrees, &metrics);
        let mut s = Self {
            repo,
            layout,
            mode: Mode::List,
            worktrees,
            metrics,
            cols,
            filter: String::new(),
            filter_active: false,
            filtered_wt: Vec::new(),
            wt_cursor: 0,
            selected: HashSet::new(),
            branch_filter: String::new(),
            filtered_branches: Vec::new(),
            branch_cursor: 0,
        };
        s.refilter_worktrees();
        Ok(s)
    }

    pub fn refresh_worktrees(&mut self) -> Result<()> {
        self.worktrees = self.repo.list_worktrees()?;
        self.metrics = compute_metrics(self.layout.as_ref(), &self.worktrees);
        self.cols = compute_col_widths(&self.worktrees, &self.metrics);
        // Indices are no longer valid after the list changes shape.
        self.selected.clear();
        self.refilter_worktrees();
        Ok(())
    }

    pub fn refilter_worktrees(&mut self) {
        let q = &self.filter;
        let mut scored: Vec<Scored> = self
            .worktrees
            .iter()
            .enumerate()
            .filter_map(|(idx, w)| {
                let hay = format!("{} {}", w.name(), w.short_branch());
                fuzzy::score(q, &hay).map(|m| Scored {
                    idx,
                    score: m.score,
                    indices: m.indices,
                })
            })
            .collect();
        scored.sort_by_key(|s| std::cmp::Reverse(s.score));
        self.filtered_wt = scored;
        self.clamp_wt_cursor();
    }

    pub fn move_cursor(&mut self, delta: isize) {
        if self.filtered_wt.is_empty() {
            return;
        }
        let len = self.filtered_wt.len() as isize;
        let cur = self.wt_cursor as isize;
        self.wt_cursor = (cur + delta).rem_euclid(len) as usize;
    }

    fn clamp_wt_cursor(&mut self) {
        if self.filtered_wt.is_empty() {
            self.wt_cursor = 0;
        } else if self.wt_cursor >= self.filtered_wt.len() {
            self.wt_cursor = self.filtered_wt.len() - 1;
        }
    }

    pub fn selected_worktree(&self) -> Option<&Worktree> {
        let s = self.filtered_wt.get(self.wt_cursor)?;
        self.worktrees.get(s.idx)
    }

    pub fn is_selected(&self, idx: usize) -> bool {
        self.selected.contains(&idx)
    }

    /// Toggle multi-select for the row under the cursor.
    pub fn toggle_select_current(&mut self) {
        if let Some(s) = self.filtered_wt.get(self.wt_cursor) {
            let idx = s.idx;
            if !self.selected.remove(&idx) {
                self.selected.insert(idx);
            }
        }
    }

    /// Select every currently-visible row, or clear them if all are already on.
    pub fn toggle_select_all(&mut self) {
        let visible: Vec<usize> = self.filtered_wt.iter().map(|s| s.idx).collect();
        let all_on = !visible.is_empty() && visible.iter().all(|i| self.selected.contains(i));
        if all_on {
            for i in visible {
                self.selected.remove(&i);
            }
        } else {
            for i in visible {
                self.selected.insert(i);
            }
        }
    }

    /// The worktrees a delete should act on: the multi-selection if any,
    /// otherwise just the row under the cursor.
    pub fn delete_targets(&self) -> Vec<PathBuf> {
        if self.selected.is_empty() {
            return self
                .selected_worktree()
                .map(|w| vec![w.path.clone()])
                .unwrap_or_default();
        }
        // Keep worktree order so the progress display counts up tidily.
        self.worktrees
            .iter()
            .enumerate()
            .filter(|(i, _)| self.selected.contains(i))
            .map(|(_, w)| w.path.clone())
            .collect()
    }

    pub fn start_delete(&mut self, paths: Vec<PathBuf>, force: bool) {
        if paths.is_empty() {
            self.mode = Mode::List;
            return;
        }
        self.mode = Mode::Deleting {
            paths,
            force,
            index: 0,
            frame: 0,
            step: 0,
            errors: Vec::new(),
        };
    }

    /// Advance the delete animation by one tick. Removes the next worktree once
    /// its warm-up frames have elapsed. Returns `true` when the batch is done.
    pub fn tick_delete(&mut self) -> bool {
        // The spinner advances on every tick regardless of work done.
        if let Mode::Deleting { frame, .. } = &mut self.mode {
            *frame = frame.wrapping_add(1);
        }
        let (index, len, step) = match &self.mode {
            Mode::Deleting {
                paths, index, step, ..
            } => (*index, paths.len(), *step),
            _ => return true,
        };
        if index >= len {
            return self.finish_delete();
        }
        // Show the spinner on the target for a few frames before acting.
        if step + 1 < DELETE_STEPS {
            if let Mode::Deleting { step, .. } = &mut self.mode {
                *step += 1;
            }
            return false;
        }
        let (path, force) = match &self.mode {
            Mode::Deleting { paths, force, .. } => (paths[index].clone(), *force),
            _ => return true,
        };
        let res = self.repo.remove_worktree(&path, force);
        if let Mode::Deleting {
            index,
            step,
            errors,
            ..
        } = &mut self.mode
        {
            if let Err(e) = res {
                errors.push(format!("{}: {}", path_name(&path), e));
            }
            *index += 1;
            *step = 0;
        }
        false
    }

    fn finish_delete(&mut self) -> bool {
        let errors = match &self.mode {
            Mode::Deleting { errors, .. } => errors.clone(),
            _ => Vec::new(),
        };
        // refresh_worktrees also drops the now-stale selection.
        let _ = self.refresh_worktrees();
        if errors.is_empty() {
            self.mode = Mode::List;
        } else {
            self.set_error(format!(
                "{} delete(s) failed — {}",
                errors.len(),
                errors.join("; ")
            ));
        }
        true
    }

    pub fn enter_branch_mode(&mut self, purpose: BranchPurpose) -> Result<()> {
        let mut all = self.repo.branches()?;
        match purpose {
            BranchPurpose::Review => {
                // Review picks remote PR branches; hide already-checked-out ones.
                all.retain(|b| matches!(b.kind, BranchKind::Remote { .. }));
                all.retain(|b| !b.is_checked_out());
            }
            BranchPurpose::NewBase | BranchPurpose::NewBaseWithPath => {
                // The user can branch off anything that resolves (local or remote).
            }
        }

        // Local first, then alphabetical so `develop`/`main` sit at the top.
        all.sort_by(|a, b| match (&a.kind, &b.kind) {
            (BranchKind::Local, BranchKind::Remote { .. }) => std::cmp::Ordering::Less,
            (BranchKind::Remote { .. }, BranchKind::Local) => std::cmp::Ordering::Greater,
            _ => a.short.cmp(&b.short),
        });

        self.branch_filter.clear();
        self.branch_cursor = 0;
        self.mode = Mode::Branch { purpose, all };
        self.refilter_branches();
        Ok(())
    }

    pub fn enter_name_input(&mut self, base: String, customize_dir: bool) {
        self.mode = Mode::NewName {
            base,
            buf: String::new(),
            dir_buf: String::new(),
            customize_dir,
            stage: NameStage::Branch,
        };
    }

    /// Returns Ok(true) on completion, Ok(false) on no-op (empty input or
    /// advanced from branch to dir stage).
    pub fn commit_new_name(&mut self) -> Result<bool> {
        let (base, branch, dir, customize, stage) = match &self.mode {
            Mode::NewName {
                base,
                buf,
                dir_buf,
                customize_dir,
                stage,
            } => (
                base.clone(),
                buf.trim().to_string(),
                dir_buf.trim().to_string(),
                *customize_dir,
                *stage,
            ),
            _ => return Ok(false),
        };
        if branch.is_empty() {
            return Ok(false);
        }
        // Two-step flow: first Enter advances to dir stage; default dir = branch.
        if customize && stage == NameStage::Branch {
            if let Mode::NewName {
                stage, dir_buf, ..
            } = &mut self.mode
            {
                if dir_buf.is_empty() {
                    *dir_buf = branch.clone();
                }
                *stage = NameStage::Dir;
            }
            return Ok(false);
        }
        let dir = if customize && !dir.is_empty() {
            dir
        } else {
            branch.clone()
        };
        if let Some(layout) = &self.layout {
            ops::new(layout, &base, &branch, &dir)?;
        } else {
            let path = self.repo.worktree_root().join(&dir);
            self.repo.add_worktree(&path, &branch, true)?;
        }
        self.refresh_worktrees()?;
        self.mode = Mode::List;
        Ok(true)
    }

    pub fn back_or_cancel_new_name(&mut self) {
        if let Mode::NewName {
            customize_dir,
            stage,
            ..
        } = &mut self.mode
        {
            if *customize_dir && *stage == NameStage::Dir {
                *stage = NameStage::Branch;
                return;
            }
        }
        self.mode = Mode::List;
    }

    pub fn edit_new_name(&mut self, f: impl FnOnce(&mut String)) {
        if let Mode::NewName {
            buf, dir_buf, stage, ..
        } = &mut self.mode
        {
            match stage {
                NameStage::Branch => f(buf),
                NameStage::Dir => f(dir_buf),
            }
        }
    }

    pub fn edit_branch_filter(&mut self, f: impl FnOnce(&mut String)) {
        f(&mut self.branch_filter);
        self.refilter_branches();
    }

    pub fn refilter_branches(&mut self) {
        let Mode::Branch { all, .. } = &self.mode else {
            return;
        };
        let q = &self.branch_filter;
        let mut scored: Vec<Scored> = all
            .iter()
            .enumerate()
            .filter_map(|(idx, b)| {
                fuzzy::score(q, &b.short).map(|m| Scored {
                    idx,
                    score: m.score,
                    indices: m.indices,
                })
            })
            .collect();
        scored.sort_by_key(|s| std::cmp::Reverse(s.score));
        self.filtered_branches = scored;
        if self.branch_cursor >= self.filtered_branches.len() {
            self.branch_cursor = self.filtered_branches.len().saturating_sub(1);
        }
    }

    pub fn go_top(&mut self) {
        self.wt_cursor = 0;
    }

    pub fn go_bottom(&mut self) {
        if !self.filtered_wt.is_empty() {
            self.wt_cursor = self.filtered_wt.len() - 1;
        }
    }

    pub fn branch_move(&mut self, delta: isize) {
        // The "+1" accounts for the virtual "[+ create]" entry rendered after the list.
        let total = self.branch_total();
        if total == 0 {
            return;
        }
        let len = total as isize;
        let cur = self.branch_cursor as isize;
        self.branch_cursor = (cur + delta).rem_euclid(len) as usize;
    }

    pub fn branch_total(&self) -> usize {
        let base = self.filtered_branches.len();
        if self.show_create_entry() {
            base + 1
        } else {
            base
        }
    }

    pub fn show_create_entry(&self) -> bool {
        // The `[+ create]` synthetic entry is gone; new branches now flow through
        // an explicit "pick base → type name" two-step.
        false
    }

    pub fn commit_branch_selection(&mut self) -> Result<bool> {
        let purpose = match &self.mode {
            Mode::Branch { purpose, .. } => *purpose,
            _ => return Ok(false),
        };
        let s = match self.filtered_branches.get(self.branch_cursor) {
            Some(s) => s.clone(),
            None => return Ok(false),
        };
        let Mode::Branch { all, .. } = &self.mode else {
            return Ok(false);
        };
        let b = all[s.idx].clone();

        match purpose {
            BranchPurpose::NewBase => {
                // Step 1 done — store base, advance to name input. ops::new runs on commit.
                self.enter_name_input(b.short.clone(), false);
                Ok(true)
            }
            BranchPurpose::NewBaseWithPath => {
                self.enter_name_input(b.short.clone(), true);
                Ok(true)
            }
            BranchPurpose::Review => {
                let plain = b
                    .short
                    .strip_prefix("origin/")
                    .unwrap_or(&b.short)
                    .to_string();
                if let Some(layout) = &self.layout {
                    ops::add(layout, &plain, &plain)?;
                } else {
                    let path = self.repo.worktree_root().join(&plain);
                    self.repo.add_worktree_from_remote(&path, &b.short)?;
                }
                self.refresh_worktrees()?;
                self.mode = Mode::List;
                Ok(true)
            }
        }
    }

    pub fn set_error(&mut self, text: String) {
        self.mode = Mode::Message { text, error: true };
    }
}

/// Ticks each worktree lingers on the spinner before it is actually removed.
/// Gives the delete a visible, animated "working…" beat even when git is fast.
pub const DELETE_STEPS: usize = 3;

/// The trailing path component (the worktree dir name), for compact display.
pub fn path_name(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

pub const H_NAME: &str = "NAME";
pub const H_BRANCH: &str = "BRANCH";
pub const H_REMOTE: &str = "REMOTE";
pub const H_DIRTY: &str = "DIRTY";
pub const H_STASH: &str = "STASH";
pub const H_PATH: &str = "PATH";

// Per-column caps keep one screaming-long branch name from blowing out the
// whole row; values longer than this get truncated with `…` at render time.
pub const MAX_NAME: usize = 22;
pub const MAX_BRANCH: usize = 30;
pub const MAX_REMOTE: usize = 9; // "↑99 ↓99"
pub const MAX_DIRTY: usize = 5;
pub const MAX_STASH: usize = 4;

fn compute_col_widths(worktrees: &[Worktree], metrics: &[Option<WorktreeMetrics>]) -> ColWidths {
    let mut name = H_NAME.chars().count();
    let mut branch = H_BRANCH.chars().count();
    let mut remote = 0usize;
    let mut dirty = 0usize;
    let mut stash = 0usize;
    let any_metrics = metrics.iter().any(|m| m.is_some());
    if any_metrics {
        remote = H_REMOTE.chars().count();
        dirty = H_DIRTY.chars().count();
        stash = H_STASH.chars().count();
    }
    for (w, m) in worktrees.iter().zip(metrics.iter()) {
        name = name.max(w.name().chars().count());
        branch = branch.max(w.short_branch().chars().count());
        if let Some(m) = m {
            remote = remote.max(remote_plain(m).chars().count());
            dirty = dirty.max(dirty_plain(m).chars().count());
            stash = stash.max(m.stash.to_string().chars().count());
        }
    }
    ColWidths {
        name: name.min(MAX_NAME),
        branch: branch.min(MAX_BRANCH),
        remote: remote.min(MAX_REMOTE),
        dirty: dirty.min(MAX_DIRTY),
        stash: stash.min(MAX_STASH),
    }
}

pub fn remote_plain(m: &WorktreeMetrics) -> String {
    match m.ahead_behind {
        None => "—".into(),
        Some(ab) if ab.ahead == 0 && ab.behind == 0 => "=".into(),
        Some(ab) => format!("↑{} ↓{}", ab.ahead, ab.behind),
    }
}

pub fn dirty_plain(m: &WorktreeMetrics) -> String {
    match m.dirty {
        None => "?".into(),
        Some(n) => n.to_string(),
    }
}

fn compute_metrics(
    layout: Option<&BareLayout>,
    worktrees: &[Worktree],
) -> Vec<Option<WorktreeMetrics>> {
    let Some(layout) = layout else {
        return vec![None; worktrees.len()];
    };
    let stashes: HashMap<String, u32> = status::stash_map(layout).unwrap_or_default();
    worktrees
        .iter()
        .map(|w| {
            let branch = w.short_branch();
            let b = if branch.starts_with('(') {
                None
            } else {
                Some(branch.as_str())
            };
            Some(status::collect(layout, &w.path, b, &stashes))
        })
        .collect()
}
