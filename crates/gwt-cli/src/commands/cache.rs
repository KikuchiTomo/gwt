use anyhow::Result;
use gwt_core::cache::{self, CacheStep};
use gwt_core::layout::BareLayout;
use gwt_core::ops;
use gwt_core::sync::{self, Step};

pub fn ls(layout: &BareLayout) -> Result<()> {
    let worktrees = ops::worktree_dirs(layout)?;
    let buckets = cache::buckets(layout, &worktrees);
    if buckets.is_empty() {
        eprintln!("no cache buckets yet");
        eprintln!("  detect what this project needs:  git wt cache init");
        eprintln!("  or add one by hand:              git wt sync cache target --key Cargo.lock");
        return Ok(());
    }
    eprintln!("cache root: {}", cache::cache_root(layout).display());
    eprintln!();

    let total: u64 = buckets.iter().map(|b| b.bytes).sum();
    let w = |f: fn(&cache::Bucket) -> String, head: &str| {
        buckets
            .iter()
            .map(|b| f(b).chars().count())
            .chain(std::iter::once(head.chars().count()))
            .max()
            .unwrap_or(8)
    };
    let sw = w(|b| b.slot.clone(), "CACHE");
    let bw = w(|b| b.id.clone(), "BUCKET");
    let zw = w(|b| cache::human_bytes(b.bytes), "SIZE");

    println!(
        "{:<sw$}  {:<bw$}  {:>zw$}  USED BY",
        "CACHE", "BUCKET", "SIZE"
    );
    println!(
        "{:<sw$}  {:<bw$}  {:>zw$}  -------",
        "-".repeat(sw),
        "-".repeat(bw),
        "-".repeat(zw)
    );
    for b in &buckets {
        let used = if b.used_by.is_empty() {
            // The interesting column: an unused bucket is what `gc` deletes,
            // and also what a branch switch will pick straight back up.
            "(unused)".to_string()
        } else {
            b.used_by.join(", ")
        };
        println!(
            "{:<sw$}  {:<bw$}  {:>zw$}  {used}",
            b.slot,
            b.id,
            cache::human_bytes(b.bytes)
        );
    }
    eprintln!();
    let unused: u64 = buckets
        .iter()
        .filter(|b| b.used_by.is_empty())
        .map(|b| b.bytes)
        .sum();
    eprintln!(
        "{} in {} bucket(s); {} not used by any worktree",
        cache::human_bytes(total),
        buckets.len(),
        cache::human_bytes(unused)
    );
    Ok(())
}

pub fn gc(layout: &BareLayout, older_than: Option<u64>, yes: bool) -> Result<()> {
    let worktrees = ops::worktree_dirs(layout)?;
    let doomed: Vec<_> = cache::buckets(layout, &worktrees)
        .into_iter()
        .filter(|b| b.used_by.is_empty())
        .filter(|b| {
            older_than.is_none_or(|d| {
                b.modified
                    .elapsed()
                    .map(|e| e.as_secs() >= d * 86_400)
                    .unwrap_or(false)
            })
        })
        .collect();
    if doomed.is_empty() {
        eprintln!("nothing to collect");
        return Ok(());
    }
    let total: u64 = doomed.iter().map(|b| b.bytes).sum();
    eprintln!("about to DELETE {} bucket(s):", doomed.len());
    for b in &doomed {
        eprintln!("  {}/{}  {}", b.slot, b.id, cache::human_bytes(b.bytes));
    }
    eprintln!("  {} in total", cache::human_bytes(total));
    // These are rebuildable by definition, but rebuilding is the cost the whole
    // feature exists to avoid — so it still asks.
    if !super::conflict::confirm("delete them?", yes)? {
        anyhow::bail!("cancelled");
    }
    let removed = cache::gc(layout, &worktrees, older_than)?;
    eprintln!(
        "freed {} from {} bucket(s)",
        cache::human_bytes(removed.iter().map(|b| b.bytes).sum()),
        removed.len()
    );
    Ok(())
}

/// Propose cache steps for whatever this project turns out to be built with.
pub fn init(layout: &BareLayout, yes: bool) -> Result<()> {
    let worktrees = ops::worktree_dirs(layout)?;
    let probe = worktrees
        .iter()
        .find(|w| w.file_name().is_some_and(|n| n == "default"))
        .or_else(|| worktrees.first());
    let Some(probe) = probe else {
        anyhow::bail!("no worktree to look at yet — create one first");
    };

    let existing: Vec<String> = sync::load(layout)?
        .steps
        .iter()
        .filter_map(|s| s.dst().map(str::to_string))
        .collect();
    let found: Vec<CacheStep> = cache::presets(probe)
        .into_iter()
        .filter(|p| !existing.contains(&p.path))
        .collect();
    if found.is_empty() {
        eprintln!("nothing new to suggest for {}", probe.display());
        eprintln!("  add one by hand: git wt sync cache <DIR> --key <FILE>");
        return Ok(());
    }

    eprintln!("detected in {}:", probe.display());
    for p in &found {
        let key = if p.key.is_empty() {
            String::new()
        } else {
            format!("  key: {}", p.key.join(" "))
        };
        eprintln!("  {:<16} {:<8}{key}", p.path, p.mode.as_str());
    }
    if !super::conflict::confirm("add these to the recipe?", yes)? {
        anyhow::bail!("cancelled");
    }
    for p in found {
        super::sync::add(layout, Step::Cache(p))?;
    }
    Ok(())
}

/// Print the environment a shell would need to point tools at their buckets.
///
/// Mounting by symlink already works without this. It exists for the tools that
/// bake an absolute path into their own cache keys: given the variable, cargo
/// writes to the bucket directly and the worktree's path never enters into it.
pub fn env(layout: &BareLayout, worktree: Option<&str>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let wt = match worktree {
        Some(name) => layout.root.join(name),
        None => cwd,
    };
    let steps = sync::load(layout)?.steps;
    let mut printed = 0;
    for s in &steps {
        let Step::Cache(c) = s else { continue };
        let Some(var) = &c.env else { continue };
        println!(
            "export {var}={}",
            cache::bucket_dir(layout, c, &wt).display()
        );
        printed += 1;
    }
    if printed == 0 {
        eprintln!("no cache step declares an `env` variable");
        eprintln!("  add one in .gwt/sync.toml:  env = \"CARGO_TARGET_DIR\"");
    }
    Ok(())
}

const HOOK_MARK: &str = "# >>> git-wt cache <<<";

/// Install git hooks that re-check the cache keys after the working tree moves.
///
/// A keyed bucket is chosen from the contents of a lockfile, so switching
/// branches or merging can invalidate the choice. The hooks live in the bare
/// repo's `hooks/`, which every worktree shares, so one install covers them all.
pub fn hooks(layout: &BareLayout, remove: bool) -> Result<()> {
    let dir = layout.bare_dir.join("hooks");
    std::fs::create_dir_all(&dir)?;
    for name in ["post-checkout", "post-merge"] {
        let path = dir.join(name);
        if remove {
            if std::fs::read_to_string(&path).is_ok_and(|s| s.contains(HOOK_MARK)) {
                std::fs::remove_file(&path)?;
                eprintln!("removed {}", path.display());
            }
            continue;
        }
        if let Ok(existing) = std::fs::read_to_string(&path) {
            if !existing.contains(HOOK_MARK) {
                eprintln!(
                    "note: {} already exists and is not ours — left alone",
                    path.display()
                );
                continue;
            }
        }
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\n{HOOK_MARK}\n\
                 # Re-point cache mounts after the working tree changed.\n\
                 command -v git-wt >/dev/null 2>&1 || exit 0\n\
                 cd \"$(git rev-parse --show-toplevel)/..\" || exit 0\n\
                 git-wt sync apply >/dev/null 2>&1 || true\n"
            ),
        )?;
        make_executable(&path)?;
        eprintln!("installed {}", path.display());
    }
    Ok(())
}

#[cfg(unix)]
fn make_executable(p: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(p)?.permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(p, perm)
}

#[cfg(windows)]
fn make_executable(_p: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}
