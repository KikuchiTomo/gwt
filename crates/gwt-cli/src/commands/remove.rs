use anyhow::Result;
use gwt_core::layout::BareLayout;
use gwt_core::ops;

pub fn run(layout: &BareLayout, name: &str) -> Result<()> {
    let branch = ops::remove(layout, name)?;
    if let Some(b) = branch {
        eprintln!("deleted branch: {b}");
    }
    Ok(())
}
