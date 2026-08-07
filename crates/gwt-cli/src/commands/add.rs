use anyhow::Result;
use gwt_core::layout::BareLayout;
use gwt_core::{ops, Error};

pub fn run(layout: &BareLayout, branch: &str, name: &str) -> Result<()> {
    match ops::add(layout, branch, name) {
        Ok(path) => {
            println!("{}", path.display());
            Ok(())
        }
        Err(e @ Error::PathExists(_)) => Err(anyhow::anyhow!(
            "{e}\n  · go there:    cd {name}\n  \
             · replace it:  git wt remove {name} && git wt add {branch} {name}\n  \
             · or run `git wt` and pick an option interactively"
        )),
        Err(e) => Err(e.into()),
    }
}
