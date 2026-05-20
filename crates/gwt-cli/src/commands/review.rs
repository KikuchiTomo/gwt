use anyhow::Result;
use gwt_core::Repo;

pub fn run(repo: &Repo, remote_branch: &str) -> Result<()> {
    let local = remote_branch
        .split_once('/')
        .map(|(_, b)| b)
        .unwrap_or(remote_branch);
    let path = repo.worktree_root().join(local);
    repo.add_worktree_from_remote(&path, remote_branch)?;
    println!("{}", path.display());
    Ok(())
}
