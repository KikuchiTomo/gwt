use anyhow::Result;
use gwt_core::layout::BareLayout;
use gwt_core::ops;

pub fn run(layout: &BareLayout, branch: &str, name: &str) -> Result<()> {
    let path = ops::add(layout, branch, name)?;
    println!("{}", path.display());
    Ok(())
}
