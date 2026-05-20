use std::path::PathBuf;

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub status: WorktreeStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeStatus {
    Normal,
    Bare,
    Detached,
    Locked,
    Prunable,
}

impl Worktree {
    /// Short, user-facing label for the worktree (directory name).
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.to_string_lossy().into_owned())
    }

    /// `feature/x` from `refs/heads/feature/x`, falling back to detached HEAD.
    pub fn short_branch(&self) -> String {
        match &self.branch {
            Some(b) => b.strip_prefix("refs/heads/").unwrap_or(b).to_string(),
            None => match &self.head {
                Some(h) if h.len() >= 7 => format!("({})", &h[..7]),
                _ => "(unknown)".into(),
            },
        }
    }
}

pub fn parse_porcelain(input: &str) -> Result<Vec<Worktree>> {
    let mut out = Vec::new();
    let mut cur: Option<Builder> = None;

    for line in input.lines() {
        if line.is_empty() {
            if let Some(b) = cur.take() {
                out.push(b.build()?);
            }
            continue;
        }
        let (key, val) = match line.split_once(' ') {
            Some((k, v)) => (k, Some(v)),
            None => (line, None),
        };
        let b = cur.get_or_insert_with(Builder::default);
        match key {
            "worktree" => b.path = val.map(PathBuf::from),
            "HEAD" => b.head = val.map(String::from),
            "branch" => b.branch = val.map(String::from),
            "bare" => b.bare = true,
            "detached" => b.detached = true,
            "locked" => b.locked = true,
            "prunable" => b.prunable = true,
            _ => {} // unknown keys are forward-compat noise
        }
    }
    if let Some(b) = cur.take() {
        out.push(b.build()?);
    }
    Ok(out)
}

#[derive(Default)]
struct Builder {
    path: Option<PathBuf>,
    head: Option<String>,
    branch: Option<String>,
    bare: bool,
    detached: bool,
    locked: bool,
    prunable: bool,
}

impl Builder {
    fn build(self) -> Result<Worktree> {
        let path = self
            .path
            .ok_or_else(|| Error::Parse("worktree record missing path".into()))?;
        let status = if self.bare {
            WorktreeStatus::Bare
        } else if self.prunable {
            WorktreeStatus::Prunable
        } else if self.locked {
            WorktreeStatus::Locked
        } else if self.detached {
            WorktreeStatus::Detached
        } else {
            WorktreeStatus::Normal
        };
        Ok(Worktree {
            path,
            head: self.head,
            branch: self.branch,
            status,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_output() {
        let sample = "\
worktree /repo/.bare
bare

worktree /repo/default
HEAD abcdef1234567890abcdef1234567890abcdef12
branch refs/heads/main

worktree /repo/feature-x
HEAD 1111111111111111111111111111111111111111
detached
";
        let wts = parse_porcelain(sample).unwrap();
        assert_eq!(wts.len(), 3);
        assert_eq!(wts[0].status, WorktreeStatus::Bare);
        assert_eq!(wts[1].short_branch(), "main");
        assert_eq!(wts[2].status, WorktreeStatus::Detached);
    }
}
