use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use gwt_core::layout::BareLayout;
use gwt_core::status::{self, WorktreeMetrics};
use gwt_core::{ops, BranchKind, BranchRef, Repo, Worktree};

use crate::fuzzy;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchPurpose {
    New,
    Review,
}

pub enum Mode {
    List,
    ConfirmDelete(PathBuf),
    Branch {
        purpose: BranchPurpose,
        all: Vec<BranchRef>,
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

pub struct App<'a> {
    pub repo: &'a Repo,
    pub layout: Option<BareLayout>,
    pub mode: Mode,

    pub worktrees: Vec<Worktree>,
    pub metrics: Vec<Option<WorktreeMetrics>>,
    pub filter: String,
    pub filter_active: bool,
    pub filtered_wt: Vec<Scored>,
    pub wt_cursor: usize,

    pub branch_filter: String,
    pub filtered_branches: Vec<Scored>,
    pub branch_cursor: usize,
}

impl<'a> App<'a> {
    pub fn new(repo: &'a Repo) -> Result<Self> {
        let worktrees = repo.list_worktrees()?;
        let layout = BareLayout::require(&repo.cwd).ok();
        let metrics = compute_metrics(layout.as_ref(), &worktrees);
        let mut s = Self {
            repo,
            layout,
            mode: Mode::List,
            worktrees,
            metrics,
            filter: String::new(),
            filter_active: false,
            filtered_wt: Vec::new(),
            wt_cursor: 0,
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

    pub fn enter_branch_mode(&mut self, purpose: BranchPurpose) -> Result<()> {
        let mut all = self.repo.branches()?;
        if purpose == BranchPurpose::Review {
            all.retain(|b| matches!(b.kind, BranchKind::Remote { .. }));
        }
        // Hide branches already checked out somewhere else — git would refuse anyway.
        all.retain(|b| !b.is_checked_out());

        // Local first, then alphabetical.
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
        let Mode::Branch { purpose, all } = &self.mode else {
            return false;
        };
        if *purpose != BranchPurpose::New {
            return false;
        }
        let q = self.branch_filter.trim();
        if q.is_empty() {
            return false;
        }
        // Don't offer to "create" a name that already exists as a local branch.
        !all.iter()
            .any(|b| b.short == q && matches!(b.kind, BranchKind::Local))
    }

    pub fn commit_branch_selection(&mut self) -> Result<bool> {
        let purpose = match &self.mode {
            Mode::Branch { purpose, .. } => *purpose,
            _ => return Ok(false),
        };
        let n = self.filtered_branches.len();
        let pick_create = self.show_create_entry() && self.branch_cursor == n;

        if pick_create {
            let q = self.branch_filter.trim().to_string();
            self.create_new_branch(&q)?;
            self.refresh_worktrees()?;
            self.mode = Mode::List;
            return Ok(true);
        }

        let s = match self.filtered_branches.get(self.branch_cursor) {
            Some(s) => s.clone(),
            None => return Ok(false),
        };
        let Mode::Branch { all, .. } = &self.mode else {
            return Ok(false);
        };
        let b = all[s.idx].clone();

        match &b.kind {
            BranchKind::Local => self.adopt_branch(&b.short, &b.short)?,
            BranchKind::Remote { .. } => {
                let local_short = b
                    .short
                    .split_once('/')
                    .map(|(_, s)| s.to_string())
                    .unwrap_or_else(|| b.short.clone());
                self.adopt_branch(&b.short, &local_short)?
            }
        }
        let _ = purpose;
        self.refresh_worktrees()?;
        self.mode = Mode::List;
        Ok(true)
    }

    fn create_new_branch(&self, branch: &str) -> Result<()> {
        if let Some(layout) = &self.layout {
            // Use ops::new so secrets + relativize run, matching the CLI's `git wt new` behavior.
            let base = layout
                .default_branch()
                .unwrap_or_else(|_| "HEAD".to_string());
            ops::new(layout, &base, branch, branch)?;
        } else {
            let path = self.repo.worktree_root().join(branch);
            self.repo.add_worktree(&path, branch, true)?;
        }
        Ok(())
    }

    fn adopt_branch(&self, branch_ref: &str, name: &str) -> Result<()> {
        if let Some(layout) = &self.layout {
            // `ops::add` handles both local and remote-tracking adoption + secrets.
            let plain = branch_ref.strip_prefix("origin/").unwrap_or(branch_ref);
            ops::add(layout, plain, name)?;
        } else if branch_ref.contains('/') {
            let path = self.repo.worktree_root().join(name);
            self.repo.add_worktree_from_remote(&path, branch_ref)?;
        } else {
            let path = self.repo.worktree_root().join(name);
            self.repo.add_worktree(&path, branch_ref, false)?;
        }
        Ok(())
    }

    pub fn set_error(&mut self, text: String) {
        self.mode = Mode::Message { text, error: true };
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
