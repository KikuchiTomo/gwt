use anyhow::Result;
use gwt_core::layout::BareLayout;
use gwt_core::ops;

use super::conflict::{self, Resolve};

pub fn run(layout: &BareLayout, base: &str, branch: &str, name: &str, r: Resolve) -> Result<()> {
    let path = match ops::new(layout, base, branch, name) {
        Ok(p) => p,
        Err(e) => conflict::resolve(layout, e, branch, name, Some(base), r)?,
    };
    println!("{}", path.display());
    Ok(())
}
