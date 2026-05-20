use std::path::Path;

use anyhow::Result;
use gwt_core::layout::BareLayout;
use gwt_core::relativize;

pub fn run(layout: &BareLayout, name: Option<&str>) -> Result<()> {
    match name {
        Some(n) => relativize::relativize_one(layout, Path::new(n))?,
        None => {
            let count = relativize::relativize_all(layout)?;
            eprintln!("relativized {count} worktree(s)");
        }
    }
    Ok(())
}
