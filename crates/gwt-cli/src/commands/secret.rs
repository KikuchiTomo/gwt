use anyhow::Result;
use gwt_core::layout::BareLayout;
use gwt_core::ops;
use gwt_core::secrets::{self, LinkOutcome, UnlinkOutcome};

/// Short label for a worktree in status lines — the directory name is what the
/// user recognizes, the absolute path is noise here.
fn wt_name(p: &std::path::Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

fn join_names(paths: &[String]) -> String {
    paths.join(", ")
}

pub fn add(layout: &BareLayout, src: &str, dst: &str) -> Result<()> {
    let res = ops::secret_add(layout, src, dst)?;
    let e = &res.entry;

    let verb = match &res.previous {
        None => "registered",
        Some(prev) if prev.dst == e.dst => "updated",
        Some(_) => "re-pointed",
    };
    eprintln!("{verb}: {} → (worktree)/{}", e.src, e.dst);
    eprintln!("  source  {}", res.src_abs.display());
    eprintln!("  link    <each worktree>/{}", e.dst);
    if let Some(prev) = res.previous.as_ref().filter(|p| p.dst != e.dst) {
        eprintln!("  (old destination (worktree)/{} was unlinked)", prev.dst);
    }

    let linked: Vec<String> = res
        .linked
        .iter()
        .filter(|(_, o)| *o == LinkOutcome::Linked)
        .map(|(p, _)| wt_name(p))
        .collect();
    let skipped: Vec<String> = res
        .linked
        .iter()
        .filter(|(_, o)| *o != LinkOutcome::Linked)
        .map(|(p, _)| wt_name(p))
        .collect();

    if !linked.is_empty() {
        eprintln!(
            "  linked into {} worktree(s): {}",
            linked.len(),
            join_names(&linked)
        );
    }
    if !res.src_exists {
        eprintln!(
            "  warning: source does not exist yet — nothing was linked. \
             Create it, then run `git wt relink`."
        );
    } else if !skipped.is_empty() {
        eprintln!("  skipped: {}", join_names(&skipped));
    }
    if res.linked.is_empty() {
        eprintln!("  (no worktrees yet — the link is applied when one is created)");
    }
    Ok(())
}

pub fn remove(layout: &BareLayout, src: &str) -> Result<()> {
    let Some(res) = ops::secret_remove(layout, src)? else {
        anyhow::bail!("no entry for: {src} (see `git wt secret ls`)");
    };
    eprintln!("removed: {} → (worktree)/{}", res.entry.src, res.entry.dst);

    let removed: Vec<String> = res
        .unlinked
        .iter()
        .filter(|(_, o)| *o == UnlinkOutcome::Removed)
        .map(|(p, _)| wt_name(p))
        .collect();
    if !removed.is_empty() {
        eprintln!(
            "  unlinked from {} worktree(s): {}",
            removed.len(),
            join_names(&removed)
        );
    }
    // Anything we refused to delete is worth calling out by name — it means a
    // real file is sitting where the link used to be.
    for (p, outcome) in &res.unlinked {
        if let UnlinkOutcome::Kept { reason } = outcome {
            eprintln!(
                "  kept {}/{}: {reason} — remove it by hand if you meant to",
                wt_name(p),
                res.entry.dst
            );
        }
    }
    eprintln!("  (the source file itself was not touched)");
    Ok(())
}

pub fn ls(layout: &BareLayout) -> Result<()> {
    let entries = secrets::read_manifest(layout)?;
    if entries.is_empty() {
        eprintln!("no secret mappings registered");
        eprintln!("  add one with: git wt secret add <SOURCE> <DEST_IN_WORKTREE>");
        eprintln!("  e.g.          git wt secret add secrets/.env .env");
        return Ok(());
    }

    let worktrees = ops::worktree_dirs(layout)?;
    let total = worktrees.len();

    // Spell out both bases in the header — this is the thing people get wrong.
    eprintln!("repo root: {}", layout.root.display());
    eprintln!("manifest:  {}", layout.manifest.display());
    eprintln!();

    let src_w = entries
        .iter()
        .map(|e| e.src.len())
        .chain(std::iter::once("SOURCE (<repo-root>/…)".len()))
        .max()
        .unwrap_or(24);
    let dst_w = entries
        .iter()
        .map(|e| e.dst.len())
        .chain(std::iter::once("DEST (<worktree>/…)".len()))
        .max()
        .unwrap_or(20);

    println!(
        "{:<src_w$}  {:<dst_w$}  {:<8}  LINKED",
        "SOURCE (<repo-root>/…)", "DEST (<worktree>/…)", "SOURCE"
    );
    println!(
        "{:<src_w$}  {:<dst_w$}  {:<8}  ------",
        "-".repeat(src_w),
        "-".repeat(dst_w),
        "--------"
    );
    for e in &entries {
        let state = if e.src_abs(layout).exists() {
            "ok"
        } else {
            "MISSING"
        };
        let n = ops::secret_link_count(layout, e, &worktrees);
        println!(
            "{:<src_w$}  {:<dst_w$}  {:<8}  {n}/{total}",
            e.src, e.dst, state
        );
    }
    Ok(())
}
