// Per-worktree status snapshot for the rich `list` / TUI columns.

use std::collections::HashMap;
use std::path::Path;

use crate::error::Result;
use crate::git;
use crate::layout::BareLayout;

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
    // No remote ref → there's nothing to compare against (e.g. local-only branch).
    let probe = git::run(
        &layout.root,
        [
            "--git-dir",
            crate::layout::BARE_DIR,
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/remotes/origin/{branch}"),
        ],
    );
    if probe.is_err() {
        return Ok(None);
    }
    let raw = git::run(
        &layout.root,
        [
            "--git-dir",
            crate::layout::BARE_DIR,
            "rev-list",
            "--left-right",
            "--count",
            &format!("{branch}...origin/{branch}"),
        ],
    )?;
    let mut it = raw.split_whitespace();
    let ahead: u32 = it.next().unwrap_or("0").parse().unwrap_or(0);
    let behind: u32 = it.next().unwrap_or("0").parse().unwrap_or(0);
    Ok(Some(AheadBehind { ahead, behind }))
}

pub fn dirty_count(wt_path: &Path) -> Result<u32> {
    let raw = git::run(wt_path, ["status", "--porcelain"])?;
    Ok(raw.lines().filter(|l| !l.is_empty()).count() as u32)
}

/// Build a `branch → stash count` map by parsing `git stash list`. Stash entries
/// look like `stash@{0}: WIP on <branch>: …` or `stash@{0}: On <branch>: …`.
pub fn stash_map(layout: &BareLayout) -> Result<HashMap<String, u32>> {
    let raw = git::run(
        &layout.root,
        ["--git-dir", crate::layout::BARE_DIR, "stash", "list"],
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
