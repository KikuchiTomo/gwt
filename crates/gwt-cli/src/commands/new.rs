use anyhow::Result;
use gwt_core::layout::BareLayout;
use gwt_core::{ops, Error};

pub fn run(layout: &BareLayout, base: &str, branch: &str, name: &str) -> Result<()> {
    match ops::new(layout, base, branch, name) {
        Ok(path) => {
            println!("{}", path.display());
            Ok(())
        }
        // The picker resolves these interactively; the CLI can at least name the
        // ways out instead of stopping at "already exists".
        Err(e @ Error::BranchExists(_)) => Err(anyhow::anyhow!(
            "{e}\n  · reuse it:    git wt add {branch} {name}\n  \
             · start over:  git wt remove <its worktree> && git wt new {base} {branch} {name}\n  \
             · or run `git wt` and pick an option interactively"
        )),
        Err(e @ Error::PathExists(_)) => Err(anyhow::anyhow!(
            "{e}\n  · go there:    cd {name}\n  \
             · replace it:  git wt remove {name} && git wt new {base} {branch} {name}\n  \
             · or run `git wt` and pick an option interactively"
        )),
        Err(e) => Err(e.into()),
    }
}
