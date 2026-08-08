use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::git;
use crate::layout::{BareLayout, BARE_DIR};

/// Rewrites the gitdir pointers of `worktree_rel` (path relative to the bare
/// root) so they use relative paths. Needed when the same checkout is mounted
/// at different absolute paths (Mac host vs Parallels VM share, etc.).
pub fn relativize_one(layout: &BareLayout, worktree_rel: &Path) -> Result<()> {
    let wt_dir = layout.root.join(worktree_rel);
    if !wt_dir.is_dir() {
        return Err(Error::NotARepo(wt_dir));
    }
    let dot_git = wt_dir.join(".git");
    if !dot_git.is_file() {
        return Err(Error::NotARepo(dot_git));
    }

    // Trust git for which metadata dir under .bare/worktrees/* this worktree owns
    // — basenames can collide and get a numeric suffix.
    let bare_wt_dir = git::run(&wt_dir, ["rev-parse", "--absolute-git-dir"])?
        .trim()
        .to_string();
    let bare_wt_dir = PathBuf::from(bare_wt_dir);
    if !bare_wt_dir.is_dir() {
        return Err(Error::NotARepo(bare_wt_dir));
    }
    let meta_name = bare_wt_dir
        .file_name()
        .ok_or_else(|| Error::Parse("bare metadata dir has no basename".into()))?
        .to_string_lossy()
        .into_owned();

    // Build "../" prefix matching the depth of `worktree_rel` (foo/bar → "../../").
    let depth = worktree_rel.components().count();
    let up = "../".repeat(depth);

    fs::write(
        &dot_git,
        format!("gitdir: {up}{BARE_DIR}/worktrees/{meta_name}\n"),
    )?;
    // The reverse pointer is the half git reads back, and only git 2.48+ (which
    // added worktree.useRelativePaths) understands a relative one. Older git
    // takes the string as the worktree's location: `git worktree list` then
    // reports `../../../default`, so nothing downstream can cd into it, and it
    // flags the worktree prunable — meaning a routine `git gc` is entitled to
    // delete its metadata. Only write relative where it is actually read back.
    // Metadata dir always sits 3 levels under the root: .bare / worktrees / <meta>.
    let reverse = if git::understands_relative_gitdirs(&wt_dir) {
        format!("../../../{}/.git\n", worktree_rel.display())
    } else {
        format!("{}\n", dot_git.display())
    };
    fs::write(bare_wt_dir.join("gitdir"), reverse)?;
    Ok(())
}

pub fn relativize_all(layout: &BareLayout) -> Result<usize> {
    let raw = git::run(
        &layout.root,
        ["--git-dir", BARE_DIR, "worktree", "list", "--porcelain"],
    )?;
    let mut count = 0usize;
    for line in raw.lines() {
        let Some(path_str) = line.strip_prefix("worktree ") else {
            continue;
        };
        let abs = PathBuf::from(path_str);
        if abs == layout.root || abs == layout.bare_dir {
            continue;
        }
        let Ok(rel) = abs.strip_prefix(&layout.root) else {
            continue;
        };
        if rel.as_os_str().is_empty() || !layout.root.join(rel).is_dir() {
            continue;
        }
        relativize_one(layout, rel)?;
        count += 1;
    }
    Ok(count)
}
