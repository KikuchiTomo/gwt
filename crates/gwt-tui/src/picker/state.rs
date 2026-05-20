use std::path::PathBuf;

use anyhow::Result;
use gwt_core::{BranchKind, BranchRef, Repo, Worktree};

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
    pub mode: Mode,

    pub worktrees: Vec<Worktree>,
    pub filter: String,
    pub filtered_wt: Vec<Scored>,
    pub wt_cursor: usize,

    pub branch_filter: String,
    pub filtered_branches: Vec<Scored>,
    pub branch_cursor: usize,
}

impl<'a> App<'a> {
    pub fn new(repo: &'a Repo) -> Result<Self> {
        let worktrees = repo.list_worktrees()?;
        let mut s = Self {
            repo,
            mode: Mode::List,
            worktrees,
            filter: String::new(),
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

    pub fn branch_cursor(&mut self, delta: isize) {
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
            let path = self.repo.worktree_root().join(&q);
            self.repo.add_worktree(&path, &q, true)?;
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

        match (&b.kind, purpose) {
            (BranchKind::Local, _) => {
                let path = self.repo.worktree_root().join(&b.short);
                self.repo.add_worktree(&path, &b.short, false)?;
            }
            (BranchKind::Remote { .. }, _) => {
                let local_short = b.short.split_once('/').map(|(_, s)| s).unwrap_or(&b.short);
                let path = self.repo.worktree_root().join(local_short);
                self.repo.add_worktree_from_remote(&path, &b.short)?;
            }
        }
        self.refresh_worktrees()?;
        self.mode = Mode::List;
        Ok(true)
    }

    pub fn set_error(&mut self, text: String) {
        self.mode = Mode::Message { text, error: true };
    }
}
