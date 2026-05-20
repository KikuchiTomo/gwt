use std::path::{Path, PathBuf};

use anyhow::Result;
use gwt_core::Repo;

pub fn run(repo: &Repo, branch: &str, path: Option<&Path>, create_branch: bool) -> Result<()> {
    let path: PathBuf = path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| repo.worktree_root().join(branch));
    repo.add_worktree(&path, branch, create_branch)?;
    println!("{}", path.display());
    Ok(())
}
