use std::path::{Path, PathBuf};

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
        let common_dir = git::run(cwd, ["rev-parse", "--git-common-dir"])
            .map_err(|_| Error::NotARepo(cwd.to_path_buf()))?
            .trim()
            .to_string();
        let common_dir = absolutize(cwd, Path::new(&common_dir));

        // Inside a bare checkout `--show-toplevel` fails; treat as no worktree.
        let current_worktree = git::run(cwd, ["rev-parse", "--show-toplevel"])
            .ok()
            .map(|s| PathBuf::from(s.trim()));

        Ok(Self {
            cwd: cwd.to_path_buf(),
            common_dir,
            current_worktree,
        })
    }

    pub fn list_worktrees(&self) -> Result<Vec<Worktree>> {
        let raw = git::run(&self.cwd, ["worktree", "list", "--porcelain"])?;
        worktree::parse_porcelain(&raw)
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
