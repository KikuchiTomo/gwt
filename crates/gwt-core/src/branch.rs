use std::path::PathBuf;

use crate::error::Result;
use crate::git;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchKind {
    Local,
    Remote { remote: String },
}

#[derive(Debug, Clone)]
pub struct BranchRef {
    pub short: String,
    pub full: String,
    pub kind: BranchKind,
    pub checked_out_at: Option<PathBuf>,
}

impl BranchRef {
    pub fn is_checked_out(&self) -> bool {
        self.checked_out_at.is_some()
    }
}

pub fn list(repo_dir: &std::path::Path) -> Result<Vec<BranchRef>> {
    // Worktree column tells us which branches already have a worktree, so the
    // UI can dim them and the new-worktree flow can skip the `--force` ask.
    let raw = git::run(
        repo_dir,
        [
            "for-each-ref",
            "--format=%(refname)\t%(refname:short)\t%(worktreepath)",
            "refs/heads",
            "refs/remotes",
        ],
    )?;

    let mut out = Vec::with_capacity(64);
    for line in raw.lines() {
        let mut parts = line.splitn(3, '\t');
        let full = parts.next().unwrap_or("").to_string();
        let short = parts.next().unwrap_or("").to_string();
        let wt = parts.next().unwrap_or("");
        if full.is_empty() || short.ends_with("/HEAD") {
            continue;
        }
        let kind = if let Some(rest) = full.strip_prefix("refs/remotes/") {
            let remote = rest.split('/').next().unwrap_or("origin").to_string();
            BranchKind::Remote { remote }
        } else {
            BranchKind::Local
        };
        let checked_out_at = if wt.is_empty() {
            None
        } else {
            Some(PathBuf::from(wt))
        };
        out.push(BranchRef {
            short,
            full,
            kind,
            checked_out_at,
        });
    }
    Ok(out)
}
