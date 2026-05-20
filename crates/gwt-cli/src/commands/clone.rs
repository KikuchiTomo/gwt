use std::path::Path;

use anyhow::Result;
use gwt_core::ops;

pub fn run(cwd: &Path, url: &str, dir: Option<&str>) -> Result<()> {
    let root = ops::clone(url, dir, cwd)?;
    println!("{}", root.display());
    Ok(())
}
