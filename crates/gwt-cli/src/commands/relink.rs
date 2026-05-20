use anyhow::Result;
use gwt_core::layout::BareLayout;
use gwt_core::ops;

pub fn run(layout: &BareLayout) -> Result<()> {
    let visited = ops::relink(layout)?;
    eprintln!("relinked {} worktree(s)", visited.len());
    Ok(())
}
