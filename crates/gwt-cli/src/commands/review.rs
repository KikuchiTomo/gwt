use anyhow::Result;
use gwt_core::layout::BareLayout;
use gwt_core::ops;

pub fn run(layout: &BareLayout, branch: &str) -> Result<()> {
    let path = ops::review(layout, branch)?;
    println!("{}", path.display());
    Ok(())
}
