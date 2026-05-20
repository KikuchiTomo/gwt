use anyhow::Result;
use gwt_core::Repo;

pub fn run(repo: &Repo) -> Result<()> {
    for w in repo.list_worktrees()? {
        println!("{}\t{}\t{}", w.name(), w.short_branch(), w.path.display());
    }
    Ok(())
}
