use anyhow::Result;
use gwt_core::layout::BareLayout;
use gwt_core::ops;

use super::conflict::{self, Resolve};

pub fn run(layout: &BareLayout, branch: &str, r: Resolve) -> Result<()> {
    let name = branch.strip_prefix("origin/").unwrap_or(branch);
    let path = match ops::review(layout, branch) {
        Ok(p) => p,
        // Review always wants origin's version, so a rebuild tracks the remote.
        Err(e) => conflict::resolve(layout, e, name, name, None, r)?,
    };
    println!("{}", path.display());
    Ok(())
}
