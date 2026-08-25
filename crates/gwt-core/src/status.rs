// Per-worktree status snapshot for the rich `list` / TUI columns.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};

use crate::error::Result;
use crate::git;
use crate::layout::BareLayout;

/// How many worktrees are measured at once.
///
/// These are git processes waiting on the filesystem, not on us, so running
/// them one at a time means the wait is the sum of every worktree's `git
/// status` — which on a big repo is most of a second each. The cap is there
/// because the disk is the bottleneck the moment there are more than a handful
/// in flight.
const AT_ONCE: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AheadBehind {
    pub ahead: u32,
    pub behind: u32,
}

#[derive(Debug, Clone, Default)]
pub struct WorktreeMetrics {
    pub ahead_behind: Option<AheadBehind>,
    pub dirty: Option<u32>,
    pub stash: u32,
}

pub fn collect(
    layout: &BareLayout,
    wt_path: &Path,
    branch: Option<&str>,
    stash_map: &HashMap<String, u32>,
) -> WorktreeMetrics {
    let ahead_behind = branch.and_then(|b| ahead_behind(layout, b).ok().flatten());
    let dirty = dirty_count(wt_path).ok();
    let stash = branch.and_then(|b| stash_map.get(b)).copied().unwrap_or(0);
    WorktreeMetrics {
        ahead_behind,
        dirty,
        stash,
    }
}

pub fn ahead_behind(layout: &BareLayout, branch: &str) -> Result<Option<AheadBehind>> {
    // One git call, not two: `rev-list` fails on its own when there is no
    // `origin/<branch>` to compare against, which is the only thing the
    // `show-ref` probe in front of it ever established. The picker makes this
    // call once per worktree before it can draw, so the round trip it saves is
    // one the user is sitting through.
    let raw = git::run(
        &layout.root,
        [
            "--no-optional-locks",
            "--git-dir",
            crate::layout::BARE_DIR,
            "rev-list",
            "--left-right",
            "--count",
            &format!("{branch}...origin/{branch}"),
        ],
    );
    let Ok(raw) = raw else {
        // No remote ref → nothing to compare against (e.g. a local-only branch).
        return Ok(None);
    };
    let mut it = raw.split_whitespace();
    let ahead: u32 = it.next().unwrap_or("0").parse().unwrap_or(0);
    let behind: u32 = it.next().unwrap_or("0").parse().unwrap_or(0);
    Ok(Some(AheadBehind { ahead, behind }))
}

pub fn dirty_count(wt_path: &Path) -> Result<u32> {
    // `--no-optional-locks` keeps a status from refreshing (and rewriting) the
    // index. These run several at a time, in the background, while the user may
    // well be running git themselves in one of these very worktrees.
    let raw = git::run(wt_path, ["--no-optional-locks", "status", "--porcelain"])?;
    Ok(raw.lines().filter(|l| !l.is_empty()).count() as u32)
}

/// Build a `branch → stash count` map by parsing `git stash list`. Stash entries
/// look like `stash@{0}: WIP on <branch>: …` or `stash@{0}: On <branch>: …`.
pub fn stash_map(layout: &BareLayout) -> Result<HashMap<String, u32>> {
    let raw = git::run(
        &layout.root,
        [
            "--no-optional-locks",
            "--git-dir",
            crate::layout::BARE_DIR,
            "stash",
            "list",
        ],
    )
    .unwrap_or_default();

    let mut map: HashMap<String, u32> = HashMap::new();
    for line in raw.lines() {
        let Some(rest) = line.split(": ").nth(1) else {
            continue;
        };
        let trimmed = rest.trim_start_matches("WIP on ").trim_start_matches("On ");
        let Some(branch) = trimmed.split(':').next() else {
            continue;
        };
        *map.entry(branch.trim().to_string()).or_default() += 1;
    }
    Ok(map)
}

/// One worktree to measure: where it is, and the branch it holds.
pub type Target = (PathBuf, Option<String>);

/// Measure every worktree off the calling thread, reporting each as it lands.
///
/// The picker used to compute all of this before its first frame, so opening it
/// in a repo with a dozen worktrees meant a dozen `git status` runs — several
/// seconds of a terminal that looks hung — before a single line appeared. The
/// list itself needs none of it: names, branches and paths come out of one
/// `git worktree list`. So the columns fill themselves in afterwards, and the
/// index in each message says which row just learned something.
///
/// The channel closing is the end of the job.
pub fn spawn(layout: &BareLayout, targets: Vec<Target>) -> Receiver<(usize, WorktreeMetrics)> {
    let (tx, rx) = mpsc::channel();
    let layout = layout.clone();
    std::thread::spawn(move || {
        // One `git stash list` answers for every worktree, so it is worth doing
        // once up front rather than per worker.
        let stashes = stash_map(&layout).unwrap_or_default();
        let next = AtomicUsize::new(0);
        let workers = targets.len().clamp(1, AT_ONCE);
        std::thread::scope(|scope| {
            for _ in 0..workers {
                let (tx, targets, stashes, next) = (tx.clone(), &targets, &stashes, &next);
                let layout = &layout;
                scope.spawn(move || loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some((path, branch)) = targets.get(i) else {
                        return;
                    };
                    let m = collect(layout, path, branch.as_deref(), stashes);
                    // A receiver that has hung up means the picker moved on.
                    if tx.send((i, m)).is_err() {
                        return;
                    }
                });
            }
        });
    });
    rx
}
