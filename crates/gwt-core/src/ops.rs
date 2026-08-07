// High-level operations matching the original bash `git wt` subcommands.
// Each one is a thin orchestration over git + secrets + relativize so the CLI
// and the TUI share identical behavior.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::git;
use crate::layout::{BareLayout, BARE_DIR, DEFAULT_WT_NAME, MANIFEST_FILE, SECRETS_DIR};
use crate::relativize::relativize_one;
use crate::secrets;
use crate::secrets::{LinkOutcome, SecretEntry, UnlinkOutcome};

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

/// Every real worktree directory, excluding the bare dir and the root itself.
pub fn worktree_dirs(layout: &BareLayout) -> Result<Vec<PathBuf>> {
    let raw = git::run(
        &layout.root,
        ["--git-dir", BARE_DIR, "worktree", "list", "--porcelain"],
    )?;
    Ok(raw
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(PathBuf::from)
        .filter(|abs| *abs != layout.root && *abs != layout.bare_dir)
        .collect())
}

/// Re-apply secrets to every existing worktree (idempotent, never touches the
/// bare or root itself).
pub fn relink(layout: &BareLayout) -> Result<Vec<PathBuf>> {
    let visited = worktree_dirs(layout)?;
    for abs in &visited {
        secrets::apply_links(layout, abs)?;
    }
    Ok(visited)
}

/// What `secret add` did, per worktree — so the CLI can report the links it
/// actually created rather than telling the user to run `relink` themselves.
#[derive(Debug, Clone)]
pub struct SecretAdd {
    pub entry: SecretEntry,
    /// The mapping this replaced, if `src` was already registered.
    pub previous: Option<SecretEntry>,
    pub src_abs: PathBuf,
    pub src_exists: bool,
    pub linked: Vec<(PathBuf, LinkOutcome)>,
}

#[derive(Debug, Clone)]
pub struct SecretRemove {
    pub entry: SecretEntry,
    pub unlinked: Vec<(PathBuf, UnlinkOutcome)>,
}

/// Register a mapping and immediately link it into every existing worktree.
pub fn secret_add(layout: &BareLayout, src: &str, dst: &str) -> Result<SecretAdd> {
    let (entry, previous) = secrets::add_entry(layout, src, dst)?;
    let src_abs = entry.src_abs(layout);
    let src_exists = src_abs.exists();

    let mut linked = Vec::new();
    for wt in worktree_dirs(layout)? {
        // A changed destination leaves a stale link behind; clear it first so the
        // worktree only ever carries the mapping the manifest describes.
        if let Some(prev) = previous.as_ref().filter(|p| p.dst != entry.dst) {
            secrets::unlink_entry(layout, &wt, prev)?;
        }
        let outcome = secrets::apply_entry(layout, &wt, &entry)?;
        linked.push((wt, outcome));
    }
    Ok(SecretAdd {
        entry,
        previous,
        src_abs,
        src_exists,
        linked,
    })
}

/// Drop a mapping and immediately remove its links from every worktree.
/// Returns `None` when `src` was not registered.
pub fn secret_remove(layout: &BareLayout, src: &str) -> Result<Option<SecretRemove>> {
    let Some(entry) = secrets::remove_entry(layout, src)? else {
        return Ok(None);
    };
    let mut unlinked = Vec::new();
    for wt in worktree_dirs(layout)? {
        let outcome = secrets::unlink_entry(layout, &wt, &entry)?;
        unlinked.push((wt, outcome));
    }
    Ok(Some(SecretRemove { entry, unlinked }))
}

/// How many worktrees currently carry the link for `entry` (for `secret ls`).
pub fn secret_link_count(layout: &BareLayout, entry: &SecretEntry, worktrees: &[PathBuf]) -> usize {
    let src_abs = entry.src_abs(layout);
    worktrees
        .iter()
        .filter(|wt| {
            let dst = entry.dst_abs(wt);
            dst.symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink())
                && std::fs::read_link(&dst).is_ok_and(|t| t == src_abs)
        })
        .count()
}

/// Fast-forward-only pull for one worktree.
///
/// `--ff-only` is deliberate: a merge or rebase here could leave the worktree
/// mid-conflict with no TUI to resolve it, so refuse instead and let the user
/// deal with divergence in a real shell.
pub fn pull(worktree_dir: &Path) -> Result<String> {
    let branch = current_branch(worktree_dir)?;
    // A bare-style clone copies every origin head into refs/heads, so worktrees
    // are normally created from a plain local branch with no upstream set. Wire
    // it up on first pull rather than making the user decode git's hint.
    let mut adopted = false;
    if !has_upstream(worktree_dir) {
        if !remote_branch_exists(worktree_dir, &branch) {
            return Err(Error::NoUpstream(branch));
        }
        git::run(
            worktree_dir,
            [
                "branch",
                &format!("--set-upstream-to=origin/{branch}"),
                &branch,
            ],
        )?;
        adopted = true;
    }
    // Describe the result from the refs themselves. git's stdout here is a
    // diffstat whose last line ("create mode 100644 …") says nothing useful.
    let before = head_sha(worktree_dir)?;
    git::run(worktree_dir, ["pull", "--ff-only"])?;
    let after = head_sha(worktree_dir)?;

    let msg = if before == after {
        "already up to date".to_string()
    } else {
        let n = count_commits(worktree_dir, &before, &after).unwrap_or(0);
        format!("fast-forwarded {n} commit(s) → {}", short(&after))
    };
    Ok(if adopted {
        format!("{msg} (now tracking origin/{branch})")
    } else {
        msg
    })
}

fn head_sha(worktree_dir: &Path) -> Result<String> {
    Ok(git::run(worktree_dir, ["rev-parse", "HEAD"])?
        .trim()
        .to_string())
}

fn count_commits(worktree_dir: &Path, from: &str, to: &str) -> Option<u32> {
    git::run(
        worktree_dir,
        ["rev-list", "--count", &format!("{from}..{to}")],
    )
    .ok()?
    .trim()
    .parse()
    .ok()
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

fn has_upstream(worktree_dir: &Path) -> bool {
    git::run(
        worktree_dir,
        ["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .is_ok()
}

fn remote_branch_exists(worktree_dir: &Path, branch: &str) -> bool {
    git::run(
        worktree_dir,
        [
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/remotes/origin/{branch}"),
        ],
    )
    .is_ok()
}

/// Push the worktree's branch, setting upstream on first push.
pub fn push(worktree_dir: &Path) -> Result<String> {
    let branch = current_branch(worktree_dir)?;
    // `git push` with no upstream fails with a hint rather than guessing; do the
    // -u dance ourselves so the first push of a new branch just works.
    let first_push = !has_upstream(worktree_dir);
    // How far ahead we were is the interesting number, and it's only knowable
    // before the push lands.
    let ahead = if first_push {
        None
    } else {
        count_commits(worktree_dir, "@{u}", "HEAD")
    };
    if first_push {
        git::run(worktree_dir, ["push", "-u", "origin", &branch])?;
    } else {
        git::run(worktree_dir, ["push"])?;
    }
    Ok(match (first_push, ahead) {
        (true, _) => format!("pushed {branch} → origin/{branch} (upstream set)"),
        (_, Some(0)) => format!("{branch} was already up to date on origin"),
        (_, Some(n)) => format!("pushed {n} commit(s) → origin/{branch}"),
        (_, None) => format!("pushed {branch} → origin/{branch}"),
    })
}

pub fn current_branch(worktree_dir: &Path) -> Result<String> {
    Ok(
        git::run(worktree_dir, ["rev-parse", "--abbrev-ref", "HEAD"])?
            .trim()
            .to_string(),
    )
}

/// The worktree that currently has `branch` checked out, if any. Git refuses to
/// delete or re-check-out such a branch, so callers need to say where it lives.
pub fn worktree_holding_branch(layout: &BareLayout, branch: &str) -> Result<Option<PathBuf>> {
    let raw = git::run(
        &layout.root,
        ["--git-dir", BARE_DIR, "worktree", "list", "--porcelain"],
    )?;
    let mut current: Option<PathBuf> = None;
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("worktree ") {
            current = Some(PathBuf::from(rest));
        } else if let Some(rest) = line.strip_prefix("branch ") {
            let short = rest.strip_prefix("refs/heads/").unwrap_or(rest);
            if short == branch {
                return Ok(current);
            }
        }
    }
    Ok(None)
}

/// Force-delete a local branch via the bare dir.
pub fn delete_local_branch(layout: &BareLayout, branch: &str) -> Result<()> {
    git::run(
        &layout.root,
        ["--git-dir", BARE_DIR, "branch", "-D", branch],
    )?;
    Ok(())
}

/// Adopt an existing local branch into a new worktree at `<root>/<name>`,
/// without creating or moving the branch.
pub fn add_existing_branch(layout: &BareLayout, branch: &str, name: &str) -> Result<PathBuf> {
    let dest = layout.root.join(name);
    if dest.exists() {
        return Err(Error::PathExists(dest));
    }
    git::run(&layout.root, ["worktree", "add", name, branch])?;
    relativize_one(layout, Path::new(name))?;
    secrets::apply_links(layout, &dest)?;
    Ok(dest)
}

/// Throw away the local branch and re-create it tracking `origin/<branch>` in a
/// fresh worktree. Destructive: the caller must have confirmed first.
pub fn recreate_branch_from_remote(
    layout: &BareLayout,
    branch: &str,
    name: &str,
) -> Result<PathBuf> {
    let _ = git::run(
        &layout.root,
        ["--git-dir", BARE_DIR, "fetch", "origin", branch],
    );
    if !branch_exists_remote(layout, branch)? {
        return Err(Error::RemoteBranchMissing(branch.into()));
    }
    if branch_exists_local(layout, branch)? {
        delete_local_branch(layout, branch)?;
    }
    add(layout, branch, name)
}

/// Remove an existing worktree directory (and its branch), then build it again.
/// Destructive: the caller must have confirmed first.
pub fn recreate_worktree(
    layout: &BareLayout,
    name: &str,
    branch: &str,
    base: Option<&str>,
) -> Result<PathBuf> {
    let dest = layout.root.join(name);
    if dest.exists() {
        // --force: the whole point of this path is discarding what's there.
        // Not fatal on failure — the path may be a stray directory that git
        // never registered as a worktree, which the rmdir below handles.
        let _ = git::run(&layout.root, ["worktree", "remove", "--force", name]);
    }
    if dest.exists() {
        std::fs::remove_dir_all(&dest)?;
        // Deleting the directory behind git's back leaves a stale admin entry
        // that would make `worktree add` refuse the same name.
        let _ = git::run(&layout.root, ["--git-dir", BARE_DIR, "worktree", "prune"]);
    }
    if branch_exists_local(layout, branch)? {
        let _ = delete_local_branch(layout, branch);
    }
    match base {
        Some(base) => new(layout, base, branch, name),
        None => recreate_branch_from_remote(layout, branch, name),
    }
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
