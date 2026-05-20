use anyhow::Result;
use gwt_core::layout::BareLayout;
use gwt_core::secrets;

pub fn add(layout: &BareLayout, src: &str, dst: &str) -> Result<()> {
    let inserted = secrets::add_entry(layout, src, dst)?;
    eprintln!(
        "{}: {src} → (worktree)/{dst}",
        if inserted { "registered" } else { "updated" }
    );
    Ok(())
}

pub fn remove(layout: &BareLayout, src: &str) -> Result<()> {
    let removed = secrets::remove_entry(layout, src)?;
    if !removed {
        anyhow::bail!("no entry for: {src}");
    }
    eprintln!("removed from manifest: {src}");
    Ok(())
}

pub fn ls(layout: &BareLayout) -> Result<()> {
    let entries = secrets::read_manifest(layout)?;
    if entries.is_empty() {
        eprintln!("no secret mappings registered");
        return Ok(());
    }
    println!("{:<40}  DESTINATION (in worktree)", "SOURCE");
    println!("{:<40}  -------------------------", "------");
    for e in entries {
        println!("{:<40}  {}", e.src, e.dst);
    }
    Ok(())
}
