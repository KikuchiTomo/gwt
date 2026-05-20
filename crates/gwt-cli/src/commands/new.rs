use anyhow::Result;
use gwt_core::layout::BareLayout;
use gwt_core::ops;

pub fn run(layout: &BareLayout, base: &str, branch: &str, name: &str) -> Result<()> {
    let path = ops::new(layout, base, branch, name)?;
    println!("{}", path.display());
    Ok(())
}
