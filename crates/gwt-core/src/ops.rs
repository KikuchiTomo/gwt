// High-level operations matching the original bash `git wt` subcommands.
// Each one is a thin orchestration over git + secrets + relativize so the CLI
// and the TUI share identical behavior.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::git;
use crate::layout::{BareLayout, BARE_DIR, DEFAULT_WT_NAME, SECRETS_DIR};
use crate::relativize::relativize_one;
use crate::sync;
use crate::sync::{Outcome, Phase, Reporter, Step, UnlinkOutcome};

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

/// What a clone produced. `empty_origin` is true when the remote has no commits
/// yet, which changes what the caller should tell the user to do next.
#[derive(Debug, Clone)]
pub struct Cloned {
    pub root: PathBuf,
    pub branch: String,
    pub empty_origin: bool,
}

pub fn clone(
    url: &str,
    dir_name: Option<&str>,
    cwd: &Path,
    report: &mut dyn FnMut(&git::Progress),
) -> Result<Cloned> {
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

    // `--progress` because git only volunteers it to a terminal, and our stderr
    // is a pipe.
    let mut on_line = |line: &str| {
        if let Some(p) = git::parse_progress(line) {
            report(&p);
        }
    };
    git::stream(
        &root,
        ["clone", "--bare", "--progress", url, BARE_DIR],
        &mut on_line,
    )?;
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
    git::stream(
        &root,
        ["--git-dir", BARE_DIR, "fetch", "--progress", "origin"],
        &mut on_line,
    )?;

    // The conventional home for the real files a recipe points at. Creating it
    // empty is the cheapest way to answer "where do I put my .env?".
    std::fs::create_dir_all(root.join(SECRETS_DIR))?;

    let layout = BareLayout::require(&root)?;
    sync::write_starter(&layout)?;
    let branch = layout.default_branch()?;
    // A repo created on the host five minutes ago has a HEAD but no commit, so
    // the branch HEAD names does not exist as a ref. `worktree add <branch>`
    // then fails with "invalid reference", which said nothing about the actual
    // situation — and left a root with no worktree in it.
    let empty_origin = !branch_exists_local(&layout, &branch)?;
    if empty_origin {
        add_unborn_worktree(&layout, &branch, DEFAULT_WT_NAME)?;
    } else {
        git::run(&root, ["worktree", "add", DEFAULT_WT_NAME, &branch])?;
    }
    relativize_one(&layout, Path::new(DEFAULT_WT_NAME))?;
    sync::apply_quiet(&layout, &root.join(DEFAULT_WT_NAME), Phase::Create)?;
    Ok(Cloned {
        root,
        branch,
        empty_origin,
    })
}

/// Check out a branch that has no commits yet, the way `git clone` leaves you
/// after cloning an empty repository.
///
/// git 2.42 added `worktree add --orphan` for exactly this. Older git — Ubuntu
/// 22.04 ships 2.34 — needs the same state built from plumbing: branch the
/// worktree off a throwaway commit, then delete the ref, which leaves HEAD
/// pointing at an unborn branch. The temporary commit is unreachable from that
/// moment and the next `git gc` collects it.
fn add_unborn_worktree(layout: &BareLayout, branch: &str, name: &str) -> Result<()> {
    let root = &layout.root;
    if matches!(git::version(root), Some(v) if v >= (2, 42)) {
        git::run(root, ["worktree", "add", "--orphan", "-b", branch, name])?;
        return Ok(());
    }
    let empty_tree = git::run(
        root,
        [
            "--git-dir",
            BARE_DIR,
            "hash-object",
            "-w",
            "-t",
            "tree",
            devnull(),
        ],
    )?
    .trim()
    .to_string();
    let seed = git::run(
        root,
        [
            "--git-dir",
            BARE_DIR,
            "commit-tree",
            &empty_tree,
            "-m",
            "gwt: temporary root for an empty repository",
        ],
    )?
    .trim()
    .to_string();
    git::run(root, ["worktree", "add", "-b", branch, name, &seed])?;
    git::run(
        root,
        [
            "--git-dir",
            BARE_DIR,
            "update-ref",
            "-d",
            &format!("refs/heads/{branch}"),
        ],
    )?;
    Ok(())
}

#[cfg(unix)]
fn devnull() -> &'static str {
    "/dev/null"
}

#[cfg(windows)]
fn devnull() -> &'static str {
    "NUL"
}

/// Adopt an existing branch (local or origin) into a fresh worktree at
/// `<root>/<name>`. Runs the sync recipe and relativizes.
pub fn add(layout: &BareLayout, branch: &str, name: &str, report: Reporter) -> Result<PathBuf> {
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
    sync::apply(layout, &dest, Phase::Create, report)?;
    Ok(dest)
}

/// Create a brand-new branch from `base` and add a worktree for it at
/// `<root>/<name>`. `base` may be a branch, tag, or commit.
pub fn new(
    layout: &BareLayout,
    base: &str,
    branch: &str,
    name: &str,
    report: Reporter,
) -> Result<PathBuf> {
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
    sync::apply(layout, &dest, Phase::Create, report)?;
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

pub fn review(layout: &BareLayout, branch: &str, report: Reporter) -> Result<PathBuf> {
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
    new(layout, &format!("origin/{branch}"), branch, branch, report)
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
///
/// The comparison goes through `canonicalize`, because git answers with
/// resolved paths: on macOS a repo under `/var/…` comes back as `/private/var/…`
/// and a plain `!=` then fails to recognise `.bare` — which is how the recipe
/// used to get applied *inside the bare directory*.
pub fn worktree_dirs(layout: &BareLayout) -> Result<Vec<PathBuf>> {
    let raw = git::run(
        &layout.root,
        ["--git-dir", BARE_DIR, "worktree", "list", "--porcelain"],
    )?;
    let real = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let root = real(&layout.root);
    let bare = real(&layout.bare_dir);
    Ok(raw
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(PathBuf::from)
        .filter(|abs| {
            let c = real(abs);
            c != root && c != bare
        })
        .collect())
}

/// What one worktree's pass through the recipe produced.
pub type WorktreeReport = (PathBuf, Vec<(Step, Outcome)>);

/// Re-apply the recipe to every existing worktree (never touches the bare dir
/// or the root itself). `phase` decides whether `run` steps fire.
pub fn sync_apply(
    layout: &BareLayout,
    phase: Phase,
    report: Reporter,
) -> Result<Vec<WorktreeReport>> {
    let mut out = Vec::new();
    for abs in worktree_dirs(layout)? {
        let outcomes = sync::apply(layout, &abs, phase, report)?;
        out.push((abs, outcomes));
    }
    Ok(out)
}

/// What `sync add` did, per worktree — so the CLI can report what it actually
/// created rather than telling the user to run `apply` themselves.
#[derive(Debug, Clone)]
pub struct StepAdded {
    pub step: Step,
    /// Steps this replaced, because they targeted the same destination.
    pub replaced: Vec<Step>,
    pub src_abs: Option<PathBuf>,
    pub src_exists: bool,
    pub applied: Vec<(PathBuf, Outcome)>,
}

#[derive(Debug, Clone)]
pub struct StepRemoved {
    pub step: Step,
    pub unlinked: Vec<(PathBuf, UnlinkOutcome)>,
}

/// Two steps writing to the same path inside a worktree is the one genuine
/// conflict, so that is what "already registered" means. A `run` step collides
/// only with the identical command line.
fn collides(a: &Step, b: &Step) -> bool {
    match (a.dst(), b.dst()) {
        (Some(x), Some(y)) => x == y,
        (None, None) => a.subject() == b.subject(),
        _ => false,
    }
}

/// Register a step and apply it to every existing worktree right away.
///
/// A `run` step is registered but not executed: a command is not idempotent the
/// way a link is, and firing someone's `npm ci` in six worktrees because they
/// typed `sync add --run` would be its own kind of surprise.
pub fn sync_add(layout: &BareLayout, step: Step) -> Result<StepAdded> {
    let mut steps = sync::load(layout)?.steps;
    let mut replaced = Vec::new();
    match steps.iter().position(|s| collides(s, &step)) {
        Some(pos) => {
            replaced.push(std::mem::replace(&mut steps[pos], step.clone()));
            // Anything else aiming at the same place was already dead weight.
            let mut i = pos + 1;
            while i < steps.len() {
                if collides(&steps[i], &step) {
                    replaced.push(steps.remove(i));
                } else {
                    i += 1;
                }
            }
        }
        None => steps.push(step.clone()),
    }
    sync::save(layout, &steps)?;

    let src_abs = step.src_abs(layout);
    let src_exists = src_abs.as_ref().is_some_and(|p| p.exists());
    let mut applied = Vec::new();
    if !matches!(step, Step::Run(_)) {
        for wt in worktree_dirs(layout)? {
            let outcome = sync::apply_step(layout, &wt, &step, &mut sync::noop)?;
            applied.push((wt, outcome));
        }
    }
    Ok(StepAdded {
        step,
        replaced,
        src_abs,
        src_exists,
        applied,
    })
}

/// Replace the step at `idx`, undoing the old one where it landed somewhere
/// else. Returns `None` when the recipe is shorter than `idx`.
pub fn sync_replace_at(layout: &BareLayout, idx: usize, step: Step) -> Result<Option<StepAdded>> {
    let mut steps = sync::load(layout)?.steps;
    if idx >= steps.len() {
        return Ok(None);
    }
    let previous = std::mem::replace(&mut steps[idx], step.clone());
    sync::save(layout, &steps)?;

    let src_abs = step.src_abs(layout);
    let src_exists = src_abs.as_ref().is_some_and(|p| p.exists());
    let mut applied = Vec::new();
    if !matches!(step, Step::Run(_)) {
        for wt in worktree_dirs(layout)? {
            // A moved destination leaves the old file behind; clear it first so
            // a worktree only ever carries what the recipe describes.
            if previous.dst() != step.dst() {
                sync::unlink_step(layout, &wt, &previous)?;
            }
            let outcome = sync::apply_step(layout, &wt, &step, &mut sync::noop)?;
            applied.push((wt, outcome));
        }
    }
    Ok(Some(StepAdded {
        step,
        replaced: vec![previous],
        src_abs,
        src_exists,
        applied,
    }))
}

pub fn sync_remove_at(layout: &BareLayout, idx: usize) -> Result<Option<StepRemoved>> {
    let mut steps = sync::load(layout)?.steps;
    if idx >= steps.len() {
        return Ok(None);
    }
    let step = steps.remove(idx);
    sync::save(layout, &steps)?;
    Ok(Some(unlink_everywhere(layout, step)?))
}

/// Drop every step matching `key` and undo it in each worktree.
///
/// `key` is matched against the destination first, then the source, then a
/// command line — so both halves of what `sync ls` prints identify a step.
pub fn sync_remove(layout: &BareLayout, key: &str) -> Result<Vec<StepRemoved>> {
    let steps = sync::load(layout)?.steps;
    let matches = |s: &Step| {
        s.dst() == Some(key) || s.src() == Some(key) || (s.dst().is_none() && s.subject() == key)
    };
    let hit_dst = steps.iter().any(|s| s.dst() == Some(key));
    let doomed: Vec<usize> = steps
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            // An exact destination match wins outright: one source linked to two
            // destinations should not lose both because one was named.
            if hit_dst {
                s.dst() == Some(key)
            } else {
                matches(s)
            }
        })
        .map(|(i, _)| i)
        .collect();
    if doomed.is_empty() {
        return Ok(Vec::new());
    }
    let kept: Vec<Step> = steps
        .iter()
        .enumerate()
        .filter(|(i, _)| !doomed.contains(i))
        .map(|(_, s)| s.clone())
        .collect();
    sync::save(layout, &kept)?;

    let mut out = Vec::new();
    for i in doomed {
        out.push(unlink_everywhere(layout, steps[i].clone())?);
    }
    Ok(out)
}

fn unlink_everywhere(layout: &BareLayout, step: Step) -> Result<StepRemoved> {
    let mut unlinked = Vec::new();
    for wt in worktree_dirs(layout)? {
        unlinked.push((wt.clone(), sync::unlink_step(layout, &wt, &step)?));
    }
    Ok(StepRemoved { step, unlinked })
}

/// How many worktrees currently carry `step` (for `sync ls`).
pub fn sync_applied_count(layout: &BareLayout, step: &Step, worktrees: &[PathBuf]) -> usize {
    worktrees
        .iter()
        .filter(|wt| sync::is_applied(layout, wt, step))
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
    // On a branch with no commits yet, `rev-parse HEAD` has nothing to resolve
    // and fails. HEAD is still a symref to the branch, which is the answer.
    match git::run(worktree_dir, ["rev-parse", "--abbrev-ref", "HEAD"]) {
        Ok(out) => Ok(out.trim().to_string()),
        Err(e) => match git::run(worktree_dir, ["symbolic-ref", "--short", "HEAD"]) {
            Ok(out) => Ok(out.trim().to_string()),
            Err(_) => Err(e),
        },
    }
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
pub fn add_existing_branch(
    layout: &BareLayout,
    branch: &str,
    name: &str,
    report: Reporter,
) -> Result<PathBuf> {
    let dest = layout.root.join(name);
    if dest.exists() {
        return Err(Error::PathExists(dest));
    }
    git::run(&layout.root, ["worktree", "add", name, branch])?;
    relativize_one(layout, Path::new(name))?;
    sync::apply(layout, &dest, Phase::Create, report)?;
    Ok(dest)
}

/// Throw away the local branch and re-create it tracking `origin/<branch>` in a
/// fresh worktree. Destructive: the caller must have confirmed first.
pub fn recreate_branch_from_remote(
    layout: &BareLayout,
    branch: &str,
    name: &str,
    report: Reporter,
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
    add(layout, branch, name, report)
}

/// Remove an existing worktree directory (and its branch), then build it again.
/// Destructive: the caller must have confirmed first.
pub fn recreate_worktree(
    layout: &BareLayout,
    name: &str,
    branch: &str,
    base: Option<&str>,
    report: Reporter,
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
        Some(base) => new(layout, base, branch, name, report),
        None => recreate_branch_from_remote(layout, branch, name, report),
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
