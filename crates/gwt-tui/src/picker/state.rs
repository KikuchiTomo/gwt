use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};

use anyhow::Result;
use gwt_core::layout::{BareLayout, DEFAULT_WT_NAME};
use gwt_core::status::{self, WorktreeMetrics};
use gwt_core::sync;
use gwt_core::{ops, t, BranchKind, BranchRef, Repo, Worktree, WorktreeStatus};

use crate::fuzzy;
use crate::theme::{KeyRow, KeySection};

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

/// What happened to the base branch on the way to this screen, carried along so
/// the name prompt can say so instead of the news being lost behind a spinner.
#[derive(Debug, Clone)]
pub struct BaseNote {
    pub text: String,
    pub error: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncOp {
    Pull,
    Push,
}

impl SyncOp {
    pub fn verb(self) -> &'static str {
        match self {
            SyncOp::Pull => "pull",
            SyncOp::Push => "push",
        }
    }
    pub fn gerund(self) -> &'static str {
        match self {
            SyncOp::Pull => "pulling",
            SyncOp::Push => "pushing",
        }
    }
}

/// What the user was trying to create when a conflict interrupted them, kept
/// whole so any resolution can pick the work back up.
#[derive(Debug, Clone)]
pub struct PendingCreate {
    /// Base ref for a fresh branch; `None` when adopting `origin/<branch>`.
    pub base: Option<String>,
    pub branch: String,
    pub dir: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictAction {
    /// Put the existing local branch into the new worktree as-is.
    UseExistingBranch,
    /// Delete the local branch, re-create it from origin.
    RecreateBranchFromRemote,
    /// cd into whatever already occupies the path.
    GoToWorktree,
    /// Delete the existing worktree, build it again.
    RecreateWorktree,
    Cancel,
}

impl ConflictAction {
    /// Anything that throws away commits or working-tree state must be confirmed.
    pub fn is_destructive(self) -> bool {
        matches!(
            self,
            ConflictAction::RecreateBranchFromRemote | ConflictAction::RecreateWorktree
        )
    }
}

#[derive(Debug, Clone)]
pub struct ConflictChoice {
    pub key: char,
    pub label: String,
    pub detail: String,
    pub action: ConflictAction,
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
        note: Option<BaseNote>,
    },
    /// Asking origin whether the chosen base branch is still current. This talks
    /// to the network, so it runs on a worker thread like every other git call
    /// that might not come back promptly.
    CheckingBase {
        base: String,
        customize_dir: bool,
        frame: usize,
        rx: Receiver<std::result::Result<Option<ops::BaseStatus>, String>>,
    },
    /// The base branch is behind origin — offer to fast-forward it first, so a
    /// new branch does not start life already out of date.
    ConfirmBasePull {
        base: String,
        customize_dir: bool,
        status: Box<ops::BaseStatus>,
    },
    UpdatingBase {
        base: String,
        customize_dir: bool,
        frame: usize,
        rx: Receiver<std::result::Result<String, String>>,
    },
    Message {
        text: String,
        error: bool,
    },
    /// The `?` overlay. Remembers where it was opened from so `?` toggles back.
    Keys {
        scroll: u16,
    },
    /// A create request hit an existing branch or path — offer ways out instead
    /// of just failing.
    Conflict {
        title: String,
        reason: String,
        pending: PendingCreate,
        choices: Vec<ConflictChoice>,
        cursor: usize,
    },
    /// The y/N gate in front of every destructive conflict resolution.
    ConfirmAction {
        pending: PendingCreate,
        action: ConflictAction,
        prompt: String,
    },
    /// The y/N gate in front of a push (it publishes to the remote).
    ConfirmSync {
        op: SyncOp,
        path: PathBuf,
        branch: String,
    },
    /// Creating a worktree, on a worker thread. Creation used to be a blocking
    /// call, which was fine while it was only `git worktree add` — a recipe with
    /// an `npm ci` in it turns the same call into minutes of frozen terminal.
    Creating {
        label: String,
        /// The most recent line the recipe produced, so the wait shows progress
        /// rather than a spinner over nothing.
        last: String,
        /// A step that failed. The worktree still exists, so this is reported
        /// at the end rather than treated as the creation failing.
        warn: Option<String>,
        frame: usize,
        rx: Receiver<CreateMsg>,
    },
    /// pull/push in flight on a worker thread. git talks to the network here, so
    /// running it on the UI thread would freeze the spinner for the whole
    /// round-trip — the result arrives over `rx` while the frame keeps ticking.
    Syncing {
        op: SyncOp,
        path: PathBuf,
        branch: String,
        frame: usize,
        rx: Receiver<std::result::Result<String, String>>,
    },
}

/// What the worker thread sends back while a worktree is being built.
pub enum CreateMsg {
    Line(String),
    Warn(String),
    Done(std::result::Result<PathBuf, String>),
}

/// The ways a worktree gets created, named so the worker can run one without
/// borrowing anything from the picker.
pub enum CreateJob {
    New {
        base: String,
        branch: String,
        dir: String,
    },
    Adopt {
        branch: String,
        dir: String,
    },
    AdoptExisting {
        branch: String,
        dir: String,
    },
    RecreateFromRemote {
        branch: String,
        dir: String,
    },
    Recreate {
        dir: String,
        branch: String,
        base: Option<String>,
    },
    /// No bare-style layout: plain `git worktree add`, no recipe to run.
    PlainNew {
        path: PathBuf,
        branch: String,
    },
    PlainRemote {
        path: PathBuf,
        remote_ref: String,
    },
}

#[derive(Default, Clone)]
pub struct Scored {
    pub idx: usize,
    pub score: i32,
    /// Match positions inside the primary field (worktree name / branch ref).
    pub indices: Vec<usize>,
    /// Match positions inside the branch column, when the query hit there.
    pub branch_indices: Vec<usize>,
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
    /// The repo's trunk. Worth knowing once: it is the branch most new work is
    /// cut from, so it leads the base-branch list instead of being hunted for.
    pub default_branch: Option<String>,

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
        // From anywhere in the repo, not just its root: standing in a worktree
        // is the normal case, and without the layout the picker loses its
        // metrics columns, its base-branch check, and — worst — creates
        // worktrees with a plain `git worktree add` that never runs the recipe.
        let layout = BareLayout::discover(&repo.cwd).ok();
        let worktrees = visible_worktrees(repo, layout.as_ref())?;
        let metrics = compute_metrics(layout.as_ref(), &worktrees);
        let cols = compute_col_widths(&worktrees, &metrics);
        // A bare-style clone copies the remote's HEAD into its own, and never
        // grows an `origin/HEAD` to read; a plain checkout is the other way
        // round. Ask whichever one this is.
        let default_branch = layout
            .as_ref()
            .and_then(|l| l.default_branch().ok())
            .filter(|b| !b.is_empty())
            .or_else(|| repo.default_branch());
        let mut s = Self {
            repo,
            layout,
            mode: Mode::List,
            default_branch,
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
        self.worktrees = visible_worktrees(self.repo, self.layout.as_ref())?;
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
                let m = score_worktree(q, &w.name(), &w.short_branch())?;
                Some(Scored {
                    idx,
                    score: m.0,
                    indices: m.1,
                    branch_indices: m.2,
                })
            })
            .collect();
        // Fuzzy score wins while filtering; with no query every score is equal,
        // so the tiebreaker is what actually orders the list: `default` first
        // (it is the one you return to), then alphabetical.
        scored.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| self.rank(a.idx).cmp(&self.rank(b.idx)))
        });
        self.filtered_wt = scored;
        self.clamp_wt_cursor();
    }

    fn rank(&self, idx: usize) -> (bool, String) {
        let name = self.worktrees[idx].name();
        (name != DEFAULT_WT_NAME, name)
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

        // The default branch first when one is being picked to branch off —
        // it is the answer often enough that hunting for it is the common case.
        // Then local before remote, then alphabetical.
        let default = match purpose {
            BranchPurpose::NewBase | BranchPurpose::NewBaseWithPath => self.default_branch.clone(),
            // Review is for picking up someone else's branch; the trunk at the
            // top of that list would only be in the way.
            BranchPurpose::Review => None,
        };
        all.sort_by_key(|b| (branch_rank(b, default.as_deref()), b.short.clone()));

        self.branch_filter.clear();
        self.branch_cursor = 0;
        self.mode = Mode::Branch { purpose, all };
        self.refilter_branches();
        Ok(())
    }

    pub fn enter_name_input(&mut self, base: String, customize_dir: bool, note: Option<BaseNote>) {
        self.mode = Mode::NewName {
            base,
            buf: String::new(),
            dir_buf: String::new(),
            customize_dir,
            stage: NameStage::Branch,
            note,
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
                ..
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
            if let Mode::NewName { stage, dir_buf, .. } = &mut self.mode {
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
        let pending = PendingCreate {
            base: Some(base.clone()),
            branch: branch.clone(),
            dir: dir.clone(),
            path: self.root_dir().join(&dir),
        };
        // An existing branch or directory is a question, not a dead end.
        if self.check_conflicts(&pending) {
            return Ok(true);
        }
        let job = if self.layout.is_some() {
            CreateJob::New {
                base,
                branch: branch.clone(),
                dir,
            }
        } else {
            CreateJob::PlainNew {
                path: pending.path,
                branch: branch.clone(),
            }
        };
        self.start_create(format!("{} {branch}", t::creating()), job);
        Ok(true)
    }

    /// Root the worktree directories hang off — the bare root when we have one.
    pub fn root_dir(&self) -> PathBuf {
        self.layout
            .as_ref()
            .map(|l| l.root.clone())
            .unwrap_or_else(|| self.repo.worktree_root())
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
            buf,
            dir_buf,
            stage,
            ..
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
                score_branch(q, b).map(|(score, indices)| Scored {
                    idx,
                    score,
                    indices,
                    ..Default::default()
                })
            })
            .collect();
        // A stable sort, so branches that score the same keep the order
        // `enter_branch_mode` put them in: default, then local, then remote.
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
                // Step 1 done — the base is settled, so ask origin whether it is
                // still current before the name prompt. ops::new runs on commit.
                self.begin_base_check(b.short.clone(), false);
                Ok(true)
            }
            BranchPurpose::NewBaseWithPath => {
                self.begin_base_check(b.short.clone(), true);
                Ok(true)
            }
            BranchPurpose::Review => {
                let plain = b
                    .short
                    .strip_prefix("origin/")
                    .unwrap_or(&b.short)
                    .to_string();
                let pending = PendingCreate {
                    // No base ref: review adopts origin/<branch> as it stands.
                    base: None,
                    branch: plain.clone(),
                    dir: plain.clone(),
                    path: self.root_dir().join(&plain),
                };
                // `ops::add` already adopts an existing local branch, so only a
                // taken path is a genuine conflict here.
                if pending.path.exists() && self.check_conflicts(&pending) {
                    return Ok(true);
                }
                let job = if self.layout.is_some() {
                    CreateJob::Adopt {
                        branch: plain.clone(),
                        dir: plain.clone(),
                    }
                } else {
                    CreateJob::PlainRemote {
                        path: pending.path,
                        remote_ref: b.short.clone(),
                    }
                };
                self.start_create(format!("{} {plain}", t::creating()), job);
                Ok(true)
            }
        }
    }

    /// Keys the picker responds to, grouped for the `?` overlay.
    pub fn key_help() -> Vec<KeySection> {
        use gwt_core::t;
        vec![
            KeySection {
                title: t::help_sec_nav().into(),
                rows: vec![
                    KeyRow {
                        keys: "j / k   ↑ / ↓",
                        desc: t::k_updown().into(),
                    },
                    KeyRow {
                        keys: "^n / ^p  ^j / ^k",
                        desc: t::k_updown().into(),
                    },
                    KeyRow {
                        keys: "g / G",
                        desc: t::k_topbottom().into(),
                    },
                    KeyRow {
                        keys: "f  /",
                        desc: t::k_filter().into(),
                    },
                ],
            },
            KeySection {
                title: t::help_sec_act().into(),
                rows: vec![
                    KeyRow {
                        keys: "enter",
                        desc: t::k_enter_cd().into(),
                    },
                    KeyRow {
                        keys: "tab  space",
                        desc: t::k_select().into(),
                    },
                    KeyRow {
                        keys: "a",
                        desc: t::k_select_all().into(),
                    },
                    KeyRow {
                        keys: "e / n",
                        desc: t::k_new().into(),
                    },
                    KeyRow {
                        keys: "E / N",
                        desc: t::k_new_dir().into(),
                    },
                    KeyRow {
                        keys: "r",
                        desc: t::k_review().into(),
                    },
                ],
            },
            KeySection {
                title: t::help_sec_sync().into(),
                rows: vec![
                    KeyRow {
                        keys: "p",
                        desc: t::k_pull().into(),
                    },
                    KeyRow {
                        keys: "P",
                        desc: t::k_push().into(),
                    },
                ],
            },
            KeySection {
                title: t::help_sec_danger().into(),
                rows: vec![
                    KeyRow {
                        keys: "d",
                        desc: t::k_del().into(),
                    },
                    KeyRow {
                        keys: "D",
                        desc: t::k_force_del().into(),
                    },
                ],
            },
            KeySection {
                title: t::help_sec_other().into(),
                rows: vec![
                    KeyRow {
                        keys: "?",
                        desc: t::k_help().into(),
                    },
                    KeyRow {
                        keys: "q  esc",
                        desc: t::k_quit().into(),
                    },
                ],
            },
        ]
    }

    pub fn set_error(&mut self, text: String) {
        self.mode = Mode::Message { text, error: true };
    }

    pub fn set_info(&mut self, text: String) {
        self.mode = Mode::Message { text, error: false };
    }

    // ---- the base branch ---------------------------------------------------

    /// Ask origin whether the chosen base branch is still current.
    ///
    /// Branching from a `main` that is a week old is the mistake this catches,
    /// and it is only catchable *here*: once the worktree exists, rebasing it is
    /// a real job. Without the layout there is no bare dir to fetch through, so
    /// that case goes straight on to the name prompt.
    pub fn begin_base_check(&mut self, base: String, customize_dir: bool) {
        let Some(layout) = self.layout.clone() else {
            return self.enter_name_input(base, customize_dir, None);
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let probe = base.clone();
        std::thread::spawn(move || {
            let _ = tx.send(ops::base_status(&layout, &probe).map_err(|e| e.to_string()));
        });
        self.mode = Mode::CheckingBase {
            base,
            customize_dir,
            frame: 0,
            rx,
        };
    }

    /// Advance the spinner; move on once origin has answered. Returns `true`
    /// when the check is over, whichever way it went.
    pub fn tick_base_check(&mut self) -> bool {
        let (base, customize_dir, outcome) = match &mut self.mode {
            Mode::CheckingBase {
                base,
                customize_dir,
                frame,
                rx,
            } => {
                *frame = frame.wrapping_add(1);
                let outcome = match rx.try_recv() {
                    Ok(res) => Some(res),
                    Err(TryRecvError::Empty) => None,
                    // Nothing came back, so we know nothing about the base —
                    // which is the same position as "it looks up to date".
                    Err(TryRecvError::Disconnected) => Some(Ok(None)),
                };
                (base.clone(), *customize_dir, outcome)
            }
            _ => return true,
        };
        let Some(res) = outcome else {
            return false;
        };
        match res {
            Ok(Some(status)) => {
                self.mode = Mode::ConfirmBasePull {
                    base,
                    customize_dir,
                    status: Box::new(status),
                }
            }
            // Up to date, or nothing to compare against.
            Ok(None) => self.enter_name_input(base, customize_dir, None),
            // A base we could not measure is still a base you can branch from;
            // say what went wrong on the next screen rather than stopping here.
            Err(e) => self.enter_name_input(
                base,
                customize_dir,
                Some(BaseNote {
                    text: e,
                    error: true,
                }),
            ),
        }
        true
    }

    /// Fast-forward the base branch, then carry on to the name prompt.
    pub fn begin_base_update(&mut self, base: String, customize_dir: bool) {
        let Some(layout) = self.layout.clone() else {
            return self.enter_name_input(base, customize_dir, None);
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let branch = base.clone();
        std::thread::spawn(move || {
            let _ = tx.send(ops::update_base_branch(&layout, &branch).map_err(|e| e.to_string()));
        });
        self.mode = Mode::UpdatingBase {
            base,
            customize_dir,
            frame: 0,
            rx,
        };
    }

    pub fn tick_base_update(&mut self) -> bool {
        let (base, customize_dir, outcome) = match &mut self.mode {
            Mode::UpdatingBase {
                base,
                customize_dir,
                frame,
                rx,
            } => {
                *frame = frame.wrapping_add(1);
                let outcome = match rx.try_recv() {
                    Ok(res) => Some(res),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => {
                        Some(Err("the update task ended unexpectedly".to_string()))
                    }
                };
                (base.clone(), *customize_dir, outcome)
            }
            _ => return true,
        };
        let Some(res) = outcome else {
            return false;
        };
        // Either way the base is usable, so both answers are a note on the name
        // prompt rather than a screen of their own. A refused fast-forward is
        // the interesting one: the branch has diverged and wants a real shell.
        let note = match res {
            Ok(text) => BaseNote { text, error: false },
            Err(e) => BaseNote {
                text: e,
                error: true,
            },
        };
        let _ = self.refresh_worktrees();
        self.enter_name_input(base, customize_dir, Some(note));
        true
    }

    // ---- pull / push -------------------------------------------------------

    /// Start a sync on the worktree under the cursor. Pull is fast-forward-only
    /// so it runs straight away; push leaves the machine and gets a confirm.
    pub fn begin_sync(&mut self, op: SyncOp) {
        let Some(wt) = self.selected_worktree() else {
            return;
        };
        if wt.status == WorktreeStatus::Bare {
            self.set_error(t::no_branch_bare().into());
            return;
        }
        let path = wt.path.clone();
        let branch = wt.short_branch();
        if branch.starts_with('(') {
            self.set_error(format!(
                "{} is detached — nothing to sync",
                path_name(&path)
            ));
            return;
        }
        match op {
            SyncOp::Pull => self.start_sync(op, path, branch),
            SyncOp::Push => self.mode = Mode::ConfirmSync { op, path, branch },
        }
    }

    /// Kick off the git call on a worker thread and switch to the spinner.
    /// Build a worktree off the UI thread, streaming the recipe's output back.
    pub fn start_create(&mut self, label: String, job: CreateJob) {
        let (tx, rx) = std::sync::mpsc::channel();
        let layout = self.layout.clone();
        let repo = (*self.repo).clone();
        std::thread::spawn(move || {
            let out = tx.clone();
            let mut report = |ev: sync::Event| {
                let msg = match ev {
                    // Only commands are worth narrating; a symlink appearing is
                    // not news, and a failure is news whatever the kind.
                    sync::Event::StepStart(s @ sync::Step::Run(_)) => {
                        Some(CreateMsg::Line(s.subject_line()))
                    }
                    sync::Event::Output(l) => Some(CreateMsg::Line(l.to_string())),
                    sync::Event::StepDone(s, o) if o.is_failure() => {
                        Some(CreateMsg::Warn(format!("{} failed", s.subject_line())))
                    }
                    _ => None,
                };
                if let Some(m) = msg {
                    let _ = out.send(m);
                }
            };
            let res: std::result::Result<PathBuf, String> = match (layout, job) {
                (Some(l), CreateJob::New { base, branch, dir }) => {
                    ops::new(&l, &base, &branch, &dir, &mut report).map_err(|e| e.to_string())
                }
                (Some(l), CreateJob::Adopt { branch, dir }) => {
                    ops::add(&l, &branch, &dir, &mut report).map_err(|e| e.to_string())
                }
                (Some(l), CreateJob::AdoptExisting { branch, dir }) => {
                    ops::add_existing_branch(&l, &branch, &dir, &mut report)
                        .map_err(|e| e.to_string())
                }
                (Some(l), CreateJob::RecreateFromRemote { branch, dir }) => {
                    ops::recreate_branch_from_remote(&l, &branch, &dir, &mut report)
                        .map_err(|e| e.to_string())
                }
                (Some(l), CreateJob::Recreate { dir, branch, base }) => {
                    ops::recreate_worktree(&l, &dir, &branch, base.as_deref(), &mut report)
                        .map_err(|e| e.to_string())
                }
                (_, CreateJob::PlainNew { path, branch }) => repo
                    .add_worktree(&path, &branch, true)
                    .map(|_| path)
                    .map_err(|e| e.to_string()),
                (_, CreateJob::PlainRemote { path, remote_ref }) => repo
                    .add_worktree_from_remote(&path, &remote_ref)
                    .map(|_| path)
                    .map_err(|e| e.to_string()),
                (None, _) => Err("not a bare-style worktree root".to_string()),
            };
            let _ = tx.send(CreateMsg::Done(res));
        });
        self.mode = Mode::Creating {
            label,
            last: String::new(),
            warn: None,
            frame: 0,
            rx,
        };
    }

    /// Advance the spinner and pick up whatever the worker has said. Returns
    /// `true` when the worktree is built (or failed to be).
    pub fn tick_create(&mut self) -> bool {
        let (outcome, warn) = match &mut self.mode {
            Mode::Creating {
                frame,
                rx,
                last,
                warn,
                ..
            } => {
                *frame = frame.wrapping_add(1);
                let mut done = None;
                loop {
                    match rx.try_recv() {
                        Ok(CreateMsg::Line(l)) => *last = l,
                        Ok(CreateMsg::Warn(w)) => *warn = Some(w),
                        Ok(CreateMsg::Done(r)) => {
                            done = Some(r);
                            break;
                        }
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            done = Some(Err("the worktree task ended unexpectedly".to_string()));
                            break;
                        }
                    }
                }
                (done, warn.clone())
            }
            _ => return true,
        };
        let Some(res) = outcome else {
            return false;
        };
        match res {
            Ok(path) => {
                let _ = self.refresh_worktrees();
                match warn {
                    // The worktree is there either way; a failed step is a
                    // result to read, not a reason to pretend nothing happened.
                    Some(w) => self.set_error(format!("{} created, but {w}", path_name(&path))),
                    None => self.mode = Mode::List,
                }
            }
            Err(e) => self.set_error(e),
        }
        true
    }

    pub fn start_sync(&mut self, op: SyncOp, path: PathBuf, branch: String) {
        let (tx, rx) = std::sync::mpsc::channel();
        let work_path = path.clone();
        std::thread::spawn(move || {
            let res = match op {
                SyncOp::Pull => ops::pull(&work_path),
                SyncOp::Push => ops::push(&work_path),
            };
            // The error type isn't Send-friendly to keep around; a string is all
            // the UI needs. A closed channel means the picker moved on.
            let _ = tx.send(res.map_err(|e| e.to_string()));
        });
        self.mode = Mode::Syncing {
            op,
            path,
            branch,
            frame: 0,
            rx,
        };
    }

    /// Advance the spinner and pick up the worker's result if it has landed.
    /// Returns `true` when the operation is finished.
    pub fn tick_sync(&mut self) -> bool {
        let (op, path, branch, outcome) = match &mut self.mode {
            Mode::Syncing {
                op,
                path,
                branch,
                frame,
                rx,
            } => {
                *frame = frame.wrapping_add(1);
                let outcome = match rx.try_recv() {
                    Ok(res) => Some(res),
                    Err(TryRecvError::Empty) => None,
                    // The worker died without sending — don't spin forever.
                    Err(TryRecvError::Disconnected) => {
                        Some(Err("sync task ended unexpectedly".to_string()))
                    }
                };
                (*op, path.clone(), branch.clone(), outcome)
            }
            _ => return true,
        };
        let Some(res) = outcome else {
            return false;
        };
        let name = path_name(&path);
        match res {
            Ok(msg) => {
                let _ = self.refresh_worktrees();
                self.set_info(format!("{} {}: {msg}", op.verb(), name));
            }
            Err(e) => self.set_error(format!("{} {name} ({branch}) failed — {e}", op.verb())),
        }
        true
    }

    // ---- conflict resolution ----------------------------------------------

    /// Build the "the path is already taken" menu.
    fn path_conflict(&self, pending: PendingCreate) -> Mode {
        let known_worktree = self
            .worktrees
            .iter()
            .any(|w| same_path(&w.path, &pending.path));
        let mut choices = Vec::new();
        if known_worktree {
            choices.push(ConflictChoice {
                key: 'g',
                label: format!("go to '{}'", pending.dir),
                detail: "cd into the worktree that is already there".into(),
                action: ConflictAction::GoToWorktree,
            });
        }
        choices.push(ConflictChoice {
            key: 'R',
            label: format!("delete '{}' and re-create it", pending.dir),
            detail: match &pending.base {
                Some(base) => format!("discards it, then branches {} from {base}", pending.branch),
                None => format!("discards it, then re-pulls origin/{}", pending.branch),
            },
            action: ConflictAction::RecreateWorktree,
        });
        choices.push(ConflictChoice {
            key: 'c',
            label: t::cancel().into(),
            detail: t::cancel_detail().into(),
            action: ConflictAction::Cancel,
        });
        Mode::Conflict {
            title: t::title_worktree_exists().into(),
            reason: format!("{} already exists", pending.path.display()),
            pending,
            choices,
            cursor: 0,
        }
    }

    /// Build the "that branch is already checked out / already exists" menu.
    fn branch_conflict(&self, pending: PendingCreate, holder: Option<PathBuf>) -> Mode {
        let has_remote = self
            .layout
            .as_ref()
            .and_then(|l| ops::branch_exists_remote(l, &pending.branch).ok())
            .unwrap_or(false);
        let mut choices = Vec::new();

        // A branch checked out elsewhere can be neither adopted nor deleted, so
        // the only useful move is going to where it lives.
        if let Some(holder) = &holder {
            choices.push(ConflictChoice {
                key: 'g',
                label: format!("go to '{}'", path_name(holder)),
                detail: "the worktree that currently has this branch".into(),
                action: ConflictAction::GoToWorktree,
            });
        } else {
            choices.push(ConflictChoice {
                key: 'u',
                label: format!("use the existing '{}' branch", pending.branch),
                detail: format!("checks it out in the new worktree '{}'", pending.dir),
                action: ConflictAction::UseExistingBranch,
            });
            if has_remote {
                choices.push(ConflictChoice {
                    key: 'R',
                    label: format!("delete '{}' and re-pull from origin", pending.branch),
                    detail: "local-only commits on that branch are lost".into(),
                    action: ConflictAction::RecreateBranchFromRemote,
                });
            }
        }
        choices.push(ConflictChoice {
            key: 'c',
            label: t::cancel().into(),
            detail: t::cancel_detail().into(),
            action: ConflictAction::Cancel,
        });

        let reason = match (&holder, has_remote) {
            (Some(h), _) => format!(
                "branch '{}' is already checked out in {}",
                pending.branch,
                h.display()
            ),
            (None, true) => format!(
                "local branch '{}' already exists (origin/{} exists too)",
                pending.branch, pending.branch
            ),
            (None, false) => format!(
                "local branch '{}' already exists (no origin/{})",
                pending.branch, pending.branch
            ),
        };
        Mode::Conflict {
            title: t::title_branch_exists().into(),
            reason,
            pending,
            choices,
            cursor: 0,
        }
    }

    /// Pre-flight a create request. Returns `true` when a conflict was found and
    /// the app has switched into the resolution menu.
    fn check_conflicts(&mut self, pending: &PendingCreate) -> bool {
        // Only the bare-style layout has the machinery to resolve these; plain
        // repos keep the old straight-to-error behavior.
        let Some(layout) = self.layout.clone() else {
            return false;
        };
        // The path check comes first: nothing can be created there either way.
        if pending.path.exists() {
            self.mode = self.path_conflict(pending.clone());
            return true;
        }
        let exists_local = ops::branch_exists_local(&layout, &pending.branch).unwrap_or(false);
        if exists_local {
            let holder = ops::worktree_holding_branch(&layout, &pending.branch)
                .ok()
                .flatten();
            self.mode = self.branch_conflict(pending.clone(), holder);
            return true;
        }
        false
    }

    pub fn conflict_move(&mut self, delta: isize) {
        if let Mode::Conflict {
            choices, cursor, ..
        } = &mut self.mode
        {
            if choices.is_empty() {
                return;
            }
            let len = choices.len() as isize;
            *cursor = ((*cursor as isize) + delta).rem_euclid(len) as usize;
        }
    }

    /// Take the choice under the cursor (or the one bound to `key`).
    pub fn conflict_pick(&mut self, key: Option<char>) -> Result<Option<PathBuf>> {
        let Mode::Conflict {
            choices,
            cursor,
            pending,
            ..
        } = &self.mode
        else {
            return Ok(None);
        };
        let choice = match key {
            Some(k) => choices.iter().find(|c| c.key == k).cloned(),
            None => choices.get(*cursor).cloned(),
        };
        let Some(choice) = choice else {
            return Ok(None);
        };
        let pending = pending.clone();

        if choice.action.is_destructive() {
            self.mode = Mode::ConfirmAction {
                prompt: choice.label.clone(),
                pending,
                action: choice.action,
            };
            return Ok(None);
        }
        self.apply_conflict_action(&pending, choice.action)
    }

    /// Run a resolution. Returns `Some(path)` when the picker should cd there.
    pub fn apply_conflict_action(
        &mut self,
        pending: &PendingCreate,
        action: ConflictAction,
    ) -> Result<Option<PathBuf>> {
        let Some(layout) = self.layout.clone() else {
            self.mode = Mode::List;
            return Ok(None);
        };
        match action {
            ConflictAction::Cancel => {
                self.mode = Mode::List;
                Ok(None)
            }
            ConflictAction::GoToWorktree => {
                // For a branch conflict the target is wherever the branch lives,
                // which need not be the path the user asked for.
                let target = ops::worktree_holding_branch(&layout, &pending.branch)
                    .ok()
                    .flatten()
                    .filter(|_| !pending.path.exists())
                    .unwrap_or_else(|| pending.path.clone());
                Ok(Some(target))
            }
            ConflictAction::UseExistingBranch => {
                self.start_create(
                    format!("{} {}", t::creating(), pending.dir),
                    CreateJob::AdoptExisting {
                        branch: pending.branch.clone(),
                        dir: pending.dir.clone(),
                    },
                );
                Ok(None)
            }
            ConflictAction::RecreateBranchFromRemote => {
                self.start_create(
                    format!("{} {}", t::creating(), pending.dir),
                    CreateJob::RecreateFromRemote {
                        branch: pending.branch.clone(),
                        dir: pending.dir.clone(),
                    },
                );
                Ok(None)
            }
            ConflictAction::RecreateWorktree => {
                self.start_create(
                    format!("{} {}", t::creating(), pending.dir),
                    CreateJob::Recreate {
                        dir: pending.dir.clone(),
                        branch: pending.branch.clone(),
                        base: pending.base.clone(),
                    },
                );
                Ok(None)
            }
        }
    }
}

/// Score one branch against the filter, returning `(score, hit positions)`.
///
/// A remote branch is matched on its **branch name** first, and on the full ref
/// only if that fails. Scoring `origin/feature` whole would let the `/` earn a
/// word-boundary bonus that the local `feature` cannot — so typing `feature`
/// put `origin/feature` above the branch of the same name, which is both
/// surprising and the wrong one to pick. Falling back to the full ref keeps
/// `origin/fea` working for anyone who types the remote out.
fn score_branch(q: &str, b: &BranchRef) -> Option<(i32, Vec<usize>)> {
    if let BranchKind::Remote { remote } = &b.kind {
        let prefix = remote.chars().count() + 1; // the remote name and its `/`
        if let Some(name) = b.short.strip_prefix(&format!("{remote}/")) {
            if let Some(m) = fuzzy::score(q, name) {
                // The hits are positions in the name; the row renders the ref.
                return Some((m.score, m.indices.iter().map(|i| i + prefix).collect()));
            }
        }
    }
    fuzzy::score(q, &b.short).map(|m| (m.score, m.indices))
}

/// Sort bucket for the base-branch list: the default branch, then the rest of
/// the local branches, then the remote ones.
///
/// `origin/<default>` is deliberately *not* promoted with its local twin: they
/// would sit next to each other looking equivalent, and picking the wrong one
/// makes a worktree that tracks nothing.
pub fn branch_rank(b: &BranchRef, default: Option<&str>) -> u8 {
    match &b.kind {
        BranchKind::Local if Some(b.short.as_str()) == default => 0,
        BranchKind::Local => 1,
        BranchKind::Remote { .. } => 2,
    }
}

/// Ticks each worktree lingers on the spinner before it is actually removed.
/// Gives the delete a visible, animated "working…" beat even when git is fast.
pub const DELETE_STEPS: usize = 3;

/// Score a worktree row against the query.
///
/// The directory name and the branch are scored as **separate** targets. A
/// single concatenated haystack would let a query match across the boundary
/// (`aaaa-bbbb` + `fix/...` matching a query that exists in neither) and would
/// make the hit positions meaningless for per-column highlighting. Matching
/// either field is enough to keep the row; the better score decides the order.
///
/// Returns `(score, name_hits, branch_hits)`.
pub fn score_worktree(q: &str, name: &str, branch: &str) -> Option<(i32, Vec<usize>, Vec<usize>)> {
    let n = fuzzy::score(q, name);
    let b = fuzzy::score(q, branch);
    if n.is_none() && b.is_none() {
        return None;
    }
    let score = n
        .as_ref()
        .map(|m| m.score)
        .max(b.as_ref().map(|m| m.score))
        .unwrap_or(0);
    Some((
        score,
        n.map(|m| m.indices).unwrap_or_default(),
        b.map(|m| m.indices).unwrap_or_default(),
    ))
}

/// The worktrees worth showing: the bare dir and the root itself are neither
/// somewhere you can `cd` to work, nor something you can pull, push, or delete —
/// listing them only adds a row you must skip past.
fn visible_worktrees(repo: &Repo, layout: Option<&BareLayout>) -> Result<Vec<Worktree>> {
    Ok(repo
        .list_worktrees()?
        .into_iter()
        .filter(|w| w.status != WorktreeStatus::Bare)
        .filter(|w| layout.is_none_or(|l| !same_path(&w.path, &l.root)))
        .collect())
}

/// Compare two paths for "same place on disk".
///
/// Git reports fully-resolved worktree paths while we build ours by joining onto
/// the layout root, so a symlink anywhere above the repo (`/tmp` on macOS, a
/// symlinked home) makes two spellings of one directory. Canonicalize when we
/// can, fall back to a literal compare when the path doesn't exist.
pub fn same_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

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

#[cfg(test)]
mod tests {
    use super::{branch_rank, score_branch, score_worktree};
    use gwt_core::{BranchKind, BranchRef};

    fn branch(short: &str, kind: BranchKind) -> BranchRef {
        BranchRef {
            short: short.into(),
            full: format!("refs/{short}"),
            kind,
            checked_out_at: None,
        }
    }

    #[test]
    fn the_default_branch_leads_the_base_list() {
        let mut all = [
            branch(
                "origin/main",
                BranchKind::Remote {
                    remote: "origin".into(),
                },
            ),
            branch("zebra", BranchKind::Local),
            branch("main", BranchKind::Local),
            branch("alpha", BranchKind::Local),
        ];
        all.sort_by_key(|b| (branch_rank(b, Some("main")), b.short.clone()));
        let order: Vec<&str> = all.iter().map(|b| b.short.as_str()).collect();
        assert_eq!(order, vec!["main", "alpha", "zebra", "origin/main"]);
    }

    #[test]
    fn a_local_branch_is_not_outscored_by_its_own_remote_twin() {
        let local = branch("feature", BranchKind::Local);
        let remote = branch(
            "origin/feature",
            BranchKind::Remote {
                remote: "origin".into(),
            },
        );
        let (local_score, _) = score_branch("feature", &local).unwrap();
        let (remote_score, hits) = score_branch("feature", &remote).unwrap();
        assert!(
            local_score >= remote_score,
            "the `/` must not buy origin/feature a better score ({remote_score} > {local_score})"
        );
        // The row still renders the full ref, so the hits have to point into it.
        assert_eq!(hits, (7..14).collect::<Vec<_>>());

        // Typing the remote out still finds it.
        assert!(score_branch("origin/fea", &remote).is_some());
        assert!(score_branch("origin/fea", &local).is_none());
    }

    #[test]
    fn with_no_default_the_list_is_still_local_first_and_alphabetical() {
        let mut all = [
            branch(
                "origin/alpha",
                BranchKind::Remote {
                    remote: "origin".into(),
                },
            ),
            branch("zebra", BranchKind::Local),
            branch("alpha", BranchKind::Local),
        ];
        all.sort_by_key(|b| (branch_rank(b, None), b.short.clone()));
        let order: Vec<&str> = all.iter().map(|b| b.short.as_str()).collect();
        assert_eq!(order, vec!["alpha", "zebra", "origin/alpha"]);
    }

    #[test]
    fn matches_on_worktree_name_or_branch() {
        // The motivating case: worktree `aaaa-bbbb` holding branch `fix/aaaa-bbbb`.
        let (_, name_hits, branch_hits) = score_worktree("fix", "aaaa-bbbb", "fix/aaaa-bbbb")
            .expect("branch match keeps the row");
        assert!(name_hits.is_empty(), "the name has no `fix` to highlight");
        assert_eq!(branch_hits, vec![0, 1, 2], "highlight `fix` in the branch");

        let (_, name_hits, _) =
            score_worktree("aaaa", "aaaa-bbbb", "fix/aaaa-bbbb").expect("name match keeps the row");
        assert_eq!(name_hits, vec![0, 1, 2, 3]);
    }

    #[test]
    fn does_not_match_across_the_field_boundary() {
        // "ax" is a subsequence of "aaaa-bbbb" + "fix/..." concatenated, but of
        // neither field on its own, so the row must be filtered out.
        assert!(score_worktree("bx", "aaaa-bbbb", "fix/aaaa-bbbb").is_none());
    }

    #[test]
    fn unrelated_query_drops_the_row() {
        assert!(score_worktree("chore", "aaaa-bbbb", "fix/aaaa-bbbb").is_none());
    }

    #[test]
    fn empty_query_keeps_everything() {
        assert!(score_worktree("", "zzz", "feat/zzz").is_some());
    }
}
