use std::path::{Component, Path, PathBuf};

use crate::error::{Error, Result};
use crate::git;
use crate::worktree::{self, Worktree};

#[derive(Debug, Clone)]
pub struct Repo {
    pub cwd: PathBuf,
    pub common_dir: PathBuf,
    pub current_worktree: Option<PathBuf>,
}

impl Repo {
    pub fn discover(cwd: &Path) -> Result<Self> {
        // `rev-parse` answers as many questions as it is asked, one line each,
        // and the picker cannot draw anything until both are answered — so ask
        // once. Inside a bare checkout `--show-toplevel` fails, and it fails
        // for the whole command, which is what the second attempt is for.
        let both = git::run(cwd, ["rev-parse", "--git-common-dir", "--show-toplevel"]);
        let (common, top) = match &both {
            Ok(raw) => {
                let mut lines = raw.lines();
                (lines.next(), lines.next())
            }
            Err(_) => (None, None),
        };
        let common_dir = match common {
            Some(c) => c.trim().to_string(),
            None => git::run(cwd, ["rev-parse", "--git-common-dir"])
                .map_err(|_| Error::NotARepo(cwd.to_path_buf()))?
                .trim()
                .to_string(),
        };
        let common_dir = absolutize(cwd, Path::new(&common_dir));

        // Inside a bare checkout there is no worktree to be standing in.
        let current_worktree = match top {
            Some(t) => Some(PathBuf::from(t.trim())),
            None if both.is_ok() => None,
            None => git::run(cwd, ["rev-parse", "--show-toplevel"])
                .ok()
                .map(|s| PathBuf::from(s.trim())),
        };

        Ok(Self {
            cwd: cwd.to_path_buf(),
            common_dir,
            current_worktree,
        })
    }

    pub fn list_worktrees(&self) -> Result<Vec<Worktree>> {
        let raw = git::run(&self.cwd, ["worktree", "list", "--porcelain"])?;
        let mut wts = worktree::parse_porcelain(&raw)?;
        // A relative path here means this git could not read the pointer that
        // put it there (see `repair_relative_gitdirs`). Repair, then ask again;
        // if the repair changed nothing, absolutize what we got so callers —
        // the picker hands its answer to a shell `cd` — never see a path that
        // is relative to a directory they know nothing about.
        if wts.iter().any(|w| w.path.is_relative()) {
            let repaired = self.repair_relative_gitdirs().unwrap_or(0);
            if repaired > 0 {
                let raw = git::run(&self.cwd, ["worktree", "list", "--porcelain"])?;
                wts = worktree::parse_porcelain(&raw)?;
            }
            for w in &mut wts {
                if w.path.is_relative() {
                    w.path = self.resolve_worktree_path(&w.path);
                }
            }
        }
        Ok(wts)
    }

    /// git reads `<common>/worktrees/<id>/gitdir` relative to the directory
    /// holding it, so a relative entry resolves the same way for every `<id>`
    /// (they are all one component deep).
    fn resolve_worktree_path(&self, rel: &Path) -> PathBuf {
        let base = self.common_dir.join("worktrees").join("_id");
        normalize(&base.join(rel))
    }

    /// gwt ≤ 0.6.3 wrote relative gitdir pointers unconditionally, but only
    /// git 2.48+ can read them back. On older git those worktrees are reported
    /// with a relative path and flagged prunable — so `git gc` may delete their
    /// metadata. Rewrite the pointers absolutely, which is what
    /// `git worktree repair` does, and return how many were fixed.
    pub fn repair_relative_gitdirs(&self) -> Result<usize> {
        let dir = self.common_dir.join("worktrees");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Ok(0);
        };
        let mut fixed = 0usize;
        for entry in entries.flatten() {
            let pointer = entry.path().join("gitdir");
            let Ok(content) = std::fs::read_to_string(&pointer) else {
                continue;
            };
            let target = Path::new(content.trim());
            if target.as_os_str().is_empty() || target.is_absolute() {
                continue;
            }
            let abs = normalize(&entry.path().join(target));
            // Only rewrite a pointer we can still make sense of: if the worktree
            // is genuinely gone, leave it for `git worktree prune` to report.
            if !abs.exists() {
                continue;
            }
            std::fs::write(&pointer, format!("{}\n", abs.display()))?;
            fixed += 1;
        }
        Ok(fixed)
    }

    pub fn add_worktree(&self, path: &Path, branch: &str, create_branch: bool) -> Result<()> {
        let mut args: Vec<std::ffi::OsString> = vec!["worktree".into(), "add".into()];
        if create_branch {
            args.push("-b".into());
            args.push(branch.into());
            args.push(path.into());
        } else {
            args.push(path.into());
            args.push(branch.into());
        }
        git::run(&self.cwd, args)?;
        Ok(())
    }

    pub fn add_worktree_from_remote(&self, path: &Path, remote_ref: &str) -> Result<()> {
        // Strip the remote prefix so the local branch name is `feature/x`, not `origin/feature/x`.
        let local = remote_ref
            .split_once('/')
            .map(|(_, b)| b)
            .unwrap_or(remote_ref);
        git::run(
            &self.cwd,
            [
                "worktree".as_ref(),
                "add".as_ref(),
                "--track".as_ref(),
                "-b".as_ref(),
                local.as_ref(),
                path.as_os_str(),
                remote_ref.as_ref(),
            ],
        )?;
        Ok(())
    }

    pub fn remove_worktree(&self, path: &Path, force: bool) -> Result<()> {
        let mut args: Vec<std::ffi::OsString> =
            vec!["worktree".into(), "remove".into(), path.into()];
        if force {
            args.push("--force".into());
        }
        git::run(&self.cwd, args)?;
        Ok(())
    }

    pub fn branches(&self) -> Result<Vec<crate::branch::BranchRef>> {
        crate::branch::list(&self.cwd)
    }

    /// The branch this repository treats as its trunk.
    ///
    /// `origin/HEAD` is the authoritative answer when it exists, but a bare-style
    /// clone builds its remote refs by hand and never gets one — there the bare
    /// repo's own HEAD is what `git clone` copied from the remote. Falling back
    /// to the conventional names is a guess, but a better one than "no default",
    /// which would leave the branch list ordered by nothing in particular.
    pub fn default_branch(&self) -> Option<String> {
        if let Ok(raw) = git::run(
            &self.cwd,
            ["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
        ) {
            if let Some(b) = raw.trim().strip_prefix("origin/") {
                if !b.is_empty() {
                    return Some(b.to_string());
                }
            }
        }
        for candidate in ["main", "master"] {
            let found = git::run(
                &self.cwd,
                [
                    "show-ref",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{candidate}"),
                ],
            )
            .is_ok();
            if found {
                return Some(candidate.to_string());
            }
        }
        None
    }

    pub fn remote_branches(&self) -> Result<Vec<String>> {
        let raw = git::run(
            &self.cwd,
            ["for-each-ref", "--format=%(refname:short)", "refs/remotes/"],
        )?;
        Ok(raw
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.ends_with("/HEAD"))
            .map(String::from)
            .collect())
    }

    pub fn worktree_root(&self) -> PathBuf {
        self.common_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.cwd.clone())
    }
}

fn absolutize(base: &Path, p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}

/// Resolve `..` lexically. The path may not exist yet (a pruned worktree), so
/// `canonicalize` is not an option, and leaving `..` in would produce a target
/// that only works from the directory it was written in.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_a_relative_pointer_to_the_worktree_beside_the_bare_dir() {
        let repo = Repo {
            cwd: PathBuf::from("/r/proj"),
            common_dir: PathBuf::from("/r/proj/.bare"),
            current_worktree: None,
        };
        // What git 2.43 prints for a gwt ≤ 0.6.3 clone.
        assert_eq!(
            repo.resolve_worktree_path(Path::new("../../../default")),
            PathBuf::from("/r/proj/default")
        );
        assert_eq!(
            repo.resolve_worktree_path(Path::new("../../../team/api")),
            PathBuf::from("/r/proj/team/api")
        );
    }
}
