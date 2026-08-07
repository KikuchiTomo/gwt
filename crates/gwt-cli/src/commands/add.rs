use anyhow::Result;
use gwt_core::layout::BareLayout;
use gwt_core::ops;

use super::conflict::{self, Resolve};

pub fn run(layout: &BareLayout, branch: &str, name: &str, r: Resolve) -> Result<()> {
    // `add` already adopts an existing local branch, so only a taken path can
    // fail here — but route everything through the same resolver for one story.
    let path = match ops::add(layout, branch, name) {
        Ok(p) => p,
        Err(e) => conflict::resolve(layout, e, branch, name, None, r)?,
    };
    println!("{}", path.display());
    Ok(())
}
