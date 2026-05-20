// High-level operations matching the original bash `git wt` subcommands.
// Each one is a thin orchestration over git + secrets + relativize so the CLI
// and the TUI share identical behavior.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::git;
use crate::layout::{BareLayout, BARE_DIR, DEFAULT_WT_NAME, MANIFEST_FILE, SECRETS_DIR};
use crate::relativize::relativize_one;
use crate::secrets;

#[derive(Debug, Clone)]
pub struct CheckReport {
    pub branch: String,
    pub remote_short: Option<String>,
    pub local_short: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub has_remote: bool,
    pub has_local: bool,
}

pub fn clone(url: &str, dir_name: Option<&str>, cwd: &Path) -> Result<PathBuf> {
    let inferred = dir_name.map(str::to_string).unwrap_or_else(|| {
        let trimmed = url.trim_end_matches('/');
        let base = trimmed.rsplit('/').next().unwrap_or(trimmed);
        base.strip_suffix(".git").unwrap_or(base).to_string()
    });
    let root = cwd.join(&inferred);
    if root.exists() {
        return Err(Error::PathExists(root));
    }
    std::fs::create_dir(&root)?;

    git::run(&root, ["clone", "--bare", url, BARE_DIR])?;
    std::fs::write(root.join(".git"), format!("gitdir: ./{BARE_DIR}\n"))?;
    // `--bare` doesn't set the canonical fetch refspec — fix that so subsequent
    // `git fetch` brings down remote branches as `refs/remotes/origin/*`.
    git::run(
        &root,
        [
            "--git-dir",
            BARE_DIR,
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    )?;
    git::run(&root, ["--git-dir", BARE_DIR, "fetch", "origin"])?;

    let secrets_dir = root.join(SECRETS_DIR);
    std::fs::create_dir_all(&secrets_dir)?;
    std::fs::File::create(secrets_dir.join(MANIFEST_FILE))?;

    let layout = BareLayout::require(&root)?;
    let default = layout.default_branch()?;
    git::run(&root, ["worktree", "add", DEFAULT_WT_NAME, &default])?;
    relativize_one(&layout, Path::new(DEFAULT_WT_NAME))?;
    Ok(root)
}

/// Adopt an existing branch (local or origin) into a fresh worktree at
/// `<root>/<name>`. Applies secrets and relativizes.
pub fn add(layout: &BareLayout, branch: &str, name: &str) -> Result<PathBuf> {
    let dest = layout.root.join(name);
    if dest.exists() {
        return Err(Error::PathExists(dest));
    }
    if branch_exists_local(layout, branch)? {
        git::run(&layout.root, ["worktree", "add", name, branch])?;
    } else if branch_exists_remote(layout, branch)? {
        git::run(
            &layout.root,
            [
                "worktree",
                "add",
                "--track",
                "-b",
                branch,
                name,
                &format!("origin/{branch}"),
            ],
        )?;
    } else {
        return Err(Error::RemoteBranchMissing(branch.into()));
    }
    relativize_one(layout, Path::new(name))?;
    secrets::apply_links(layout, &dest)?;
    Ok(dest)
}

/// Create a brand-new branch from `base` and add a worktree for it at
/// `<root>/<name>`. `base` may be a branch, tag, or commit.
pub fn new(layout: &BareLayout, base: &str, branch: &str, name: &str) -> Result<PathBuf> {
    let dest = layout.root.join(name);
    if dest.exists() {
        return Err(Error::PathExists(dest));
    }
    if branch_exists_local(layout, branch)? {
        return Err(Error::BranchExists(branch.into()));
    }
    if !rev_parse_verify(layout, base)? {
        return Err(Error::InvalidBase(base.into()));
    }
    git::run(&layout.root, ["worktree", "add", "-b", branch, name, base])?;
    relativize_one(layout, Path::new(name))?;
    secrets::apply_links(layout, &dest)?;
    Ok(dest)
}

pub fn remove(layout: &BareLayout, name: &str) -> Result<Option<String>> {
    let dest = layout.root.join(name);
    if !dest.is_dir() {
        return Err(Error::NotARepo(dest));
    }
    let branch = git::run(&dest, ["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|b| !b.is_empty() && b != "HEAD");
    git::run(&layout.root, ["worktree", "remove", name])?;
    if let Some(b) = &branch {
        // The branch may have been the only ref to recent commits — that's fine,
        // mirror the bash version's `branch -D` and ignore "already gone" errors.
        let _ = git::run(&layout.root, ["--git-dir", BARE_DIR, "branch", "-D", b]);
    }
    Ok(branch)
}

pub fn review(layout: &BareLayout, branch: &str) -> Result<PathBuf> {
    let branch = branch.strip_prefix("origin/").unwrap_or(branch);
    let dest = layout.root.join(branch);
    if dest.exists() {
        return Err(Error::PathExists(dest));
    }
    if branch_exists_local(layout, branch)? {
        return Err(Error::BranchExists(branch.into()));
    }
    git::run(
        &layout.root,
        ["--git-dir", BARE_DIR, "fetch", "origin", branch],
    )?;
    if !branch_exists_remote(layout, branch)? {
        return Err(Error::RemoteBranchMissing(branch.into()));
    }
    new(layout, &format!("origin/{branch}"), branch, branch)
}

pub fn check(layout: &BareLayout, branch: &str, do_fetch: bool) -> Result<CheckReport> {
    let branch: String = branch.strip_prefix("origin/").unwrap_or(branch).to_string();
    if do_fetch {
        git::run(
            &layout.root,
            ["--git-dir", BARE_DIR, "fetch", "origin", &branch],
        )?;
    }
    let has_remote = branch_exists_remote(layout, &branch)?;
    let has_local_no_remote = !has_remote && branch_exists_local(layout, &branch)?;
    if !has_remote {
        return Ok(CheckReport {
            branch,
            remote_short: None,
            local_short: None,
            ahead: 0,
            behind: 0,
            has_remote: false,
            has_local: has_local_no_remote,
        });
    }
    let remote_short = Some(
        git::run(
            &layout.root,
            [
                "--git-dir",
                BARE_DIR,
                "rev-parse",
                "--short",
                &format!("origin/{branch}"),
            ],
        )?
        .trim()
        .to_string(),
    );
    let has_local = branch_exists_local(layout, &branch)?;
    let (local_short, ahead, behind) = if has_local {
        let local_short = git::run(
            &layout.root,
            ["--git-dir", BARE_DIR, "rev-parse", "--short", &branch],
        )?
        .trim()
        .to_string();
        let counts = git::run(
            &layout.root,
            [
                "--git-dir",
                BARE_DIR,
                "rev-list",
                "--left-right",
                "--count",
                &format!("{branch}...origin/{branch}"),
            ],
        )?;
        let mut it = counts.split_whitespace();
        let a: u32 = it.next().unwrap_or("0").parse().unwrap_or(0);
        let b: u32 = it.next().unwrap_or("0").parse().unwrap_or(0);
        (Some(local_short), a, b)
    } else {
        (None, 0, 0)
    };
    Ok(CheckReport {
        branch,
        remote_short,
        local_short,
        ahead,
        behind,
        has_remote,
        has_local,
    })
}

/// Re-apply secrets to every existing worktree (idempotent, never touches the
/// bare or root itself).
pub fn relink(layout: &BareLayout) -> Result<Vec<PathBuf>> {
    let raw = git::run(
        &layout.root,
        ["--git-dir", BARE_DIR, "worktree", "list", "--porcelain"],
    )?;
    let mut visited = Vec::new();
    for line in raw.lines() {
        let Some(rest) = line.strip_prefix("worktree ") else {
            continue;
        };
        let abs = PathBuf::from(rest);
        if abs == layout.root || abs == layout.bare_dir {
            continue;
        }
        secrets::apply_links(layout, &abs)?;
        visited.push(abs);
    }
    Ok(visited)
}

pub fn branch_exists_local(layout: &BareLayout, branch: &str) -> Result<bool> {
    Ok(git::run(
        &layout.root,
        [
            "--git-dir",
            BARE_DIR,
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok())
}

pub fn branch_exists_remote(layout: &BareLayout, branch: &str) -> Result<bool> {
    Ok(git::run(
        &layout.root,
        [
            "--git-dir",
            BARE_DIR,
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/remotes/origin/{branch}"),
        ],
    )
    .is_ok())
}

pub fn rev_parse_verify(layout: &BareLayout, spec: &str) -> Result<bool> {
    Ok(git::run(
        &layout.root,
        [
            "--git-dir",
            BARE_DIR,
            "rev-parse",
            "--verify",
            "--quiet",
            spec,
        ],
    )
    .is_ok())
}
