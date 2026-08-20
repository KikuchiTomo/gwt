use std::path::Path;

use anyhow::Result;
use gwt_core::layout::BareLayout;
use gwt_core::ops;
use gwt_core::sync::{self, Event, Origin, Outcome, Phase, Step, UnlinkOutcome};

/// Short label for a worktree in status lines — the directory name is what the
/// user recognizes, the absolute path is noise here.
fn wt_name(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// Progress for the command line.
///
/// Links and copies stay silent unless they fail: nobody asked to watch six
/// symlinks appear. A `run` step is the opposite — it can take minutes, so its
/// output is echoed as it arrives rather than after the fact.
pub fn report(ev: Event) {
    match ev {
        Event::StepStart(Step::Run(r)) => eprintln!("· {}", sync::one_line(&r.cmd)),
        Event::StepStart(_) => {}
        Event::Output(line) => eprintln!("  {line}"),
        Event::StepDone(step, outcome) => match outcome {
            Outcome::Ran { code: 0, secs } => eprintln!("  ✓ {} ({secs}s)", step.subject_line()),
            Outcome::Ran { code, secs } => {
                eprintln!("  ✗ {} exited {code} after {secs}s", step.subject_line())
            }
            Outcome::Failed { detail } => eprintln!("  ✗ {}: {detail}", step.subject_line()),
            Outcome::Blocked { reason } => eprintln!(
                "  ✗ {}: {reason} — move one aside, then run `git wt sync apply`",
                step.dst().unwrap_or("")
            ),
            // A cache that was adopted or seeded moved real data around, which
            // is worth a line; a cache that was simply re-attached is not.
            Outcome::Mounted {
                bucket,
                adopted,
                seeded,
            } if *adopted || *seeded => eprintln!(
                "  · {} → bucket {bucket} ({})",
                step.dst().unwrap_or(""),
                if *adopted {
                    "adopted what was already there"
                } else {
                    "seeded from the most recent bucket"
                }
            ),
            _ => {}
        },
    }
}

fn describe(step: &Step) -> String {
    match step {
        // A cache has no source to point at a destination from; what it has is
        // a policy, and reading it the other way round is confusing.
        Step::Cache(c) => format!(
            "cache (worktree)/{} · {} {}",
            c.path,
            c.mode.as_str(),
            c.summary()
        ),
        _ => match step.dst() {
            Some(dst) => format!("{} {} → (worktree)/{dst}", step.kind(), step.subject_line()),
            None => format!("{} {}", step.kind(), step.subject_line()),
        },
    }
}

pub fn add(layout: &BareLayout, step: Step) -> Result<()> {
    let is_run = matches!(step, Step::Run(_));
    let res = ops::sync_add(layout, step)?;

    let verb = if res.replaced.is_empty() {
        "registered"
    } else {
        "replaced"
    };
    eprintln!("{verb}: {}", describe(&res.step));
    for old in &res.replaced {
        eprintln!("  (was: {})", describe(old));
    }
    if let Some(src) = &res.src_abs {
        eprintln!("  source  {}", src.display());
    }
    if let Some(dst) = res.step.dst() {
        eprintln!("  lands   <each worktree>/{dst}");
    }

    if is_run {
        eprintln!(
            "  it will run when a worktree is created — or now, with `git wt sync apply --run`"
        );
        return Ok(());
    }

    let done: Vec<String> = res
        .applied
        .iter()
        .filter(|(_, o)| {
            matches!(
                o,
                Outcome::Linked | Outcome::Copied | Outcome::Mounted { .. }
            )
        })
        .map(|(p, _)| wt_name(p))
        .collect();
    for (p, o) in &res.applied {
        if let Outcome::Blocked { reason } = o {
            eprintln!("  ✗ {}: {reason}", wt_name(p));
        }
    }
    let skipped: Vec<String> = res
        .applied
        .iter()
        .filter(|(_, o)| matches!(o, Outcome::Skipped { .. }))
        .map(|(p, _)| wt_name(p))
        .collect();

    if !done.is_empty() {
        eprintln!(
            "  applied to {} worktree(s): {}",
            done.len(),
            done.join(", ")
        );
    }
    if res.step.src().is_some() && !res.src_exists {
        eprintln!(
            "  warning: the source does not exist yet — nothing was applied. \
             Create it, then run `git wt sync apply`."
        );
    } else if !skipped.is_empty() {
        eprintln!("  skipped: {}", skipped.join(", "));
    }
    if res.applied.is_empty() {
        eprintln!("  (no worktrees yet — it is applied when one is created)");
    }
    Ok(())
}

pub fn remove(layout: &BareLayout, key: &str) -> Result<()> {
    let removed = ops::sync_remove(layout, key)?;
    if removed.is_empty() {
        anyhow::bail!("nothing registered as: {key} (see `git wt sync ls`)");
    }
    for res in &removed {
        eprintln!("removed: {}", describe(&res.step));

        let gone: Vec<String> = res
            .unlinked
            .iter()
            .filter(|(_, o)| *o == UnlinkOutcome::Removed)
            .map(|(p, _)| wt_name(p))
            .collect();
        if !gone.is_empty() {
            eprintln!(
                "  cleared from {} worktree(s): {}",
                gone.len(),
                gone.join(", ")
            );
        }
        // Anything we refused to delete is worth calling out by name — it means
        // a real file the user may have edited is sitting at that path.
        for (p, outcome) in &res.unlinked {
            if let UnlinkOutcome::Kept { reason } = outcome {
                eprintln!(
                    "  kept {}/{}: {reason} — remove it by hand if you meant to",
                    wt_name(p),
                    res.step.dst().unwrap_or("")
                );
            }
        }
    }
    eprintln!("  (source files themselves are never touched)");
    Ok(())
}

pub fn apply(layout: &BareLayout, run_commands: bool) -> Result<()> {
    let phase = if run_commands {
        Phase::Create
    } else {
        Phase::Apply
    };
    let visited = ops::sync_apply(layout, phase, &mut report)?;
    let failures: usize = visited
        .iter()
        .flat_map(|(_, outcomes)| outcomes.iter())
        .filter(|(_, o)| o.is_failure())
        .count();
    eprintln!("applied to {} worktree(s)", visited.len());
    if failures > 0 {
        anyhow::bail!("{failures} step(s) failed");
    }
    Ok(())
}

pub fn ls(layout: &BareLayout) -> Result<()> {
    let cfg = sync::load(layout)?;
    if cfg.steps.is_empty() {
        eprintln!("no sync steps registered");
        eprintln!("  link a file:  git wt sync add secrets/.env .env");
        eprintln!("  copy a file:  git wt sync copy secrets/env.sample .env --render");
        eprintln!("  run a command: git wt sync run 'npm ci' --only-if package.json");
        return Ok(());
    }

    // Spell out both bases in the header — this is the thing people get wrong.
    eprintln!("repo root: {}", layout.root.display());
    match cfg.origin {
        Origin::Toml => eprintln!("recipe:    {}", layout.sync_config.display()),
        Origin::Legacy => {
            eprintln!(
                "recipe:    {} (pre-0.7 format)",
                layout.legacy_manifest.display()
            );
            eprintln!(
                "           the next change writes {}",
                layout.sync_config.display()
            );
        }
        Origin::Missing => {}
    }
    eprintln!();

    let worktrees = ops::worktree_dirs(layout)?;
    let total = worktrees.len();

    let rows: Vec<(String, String, String, String, String)> = cfg
        .steps
        .iter()
        .map(|s| {
            let state = match s {
                Step::Run(r) => r
                    .when
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
                Step::Cache(c) => c.mode.as_str().to_string(),
                _ => {
                    if s.src_abs(layout).is_some_and(|p| p.exists()) {
                        "ok".into()
                    } else {
                        "MISSING".into()
                    }
                }
            };
            let applied = match s {
                // A command leaves no mark, so counting worktrees would be a lie.
                Step::Run(_) => "-".to_string(),
                _ => format!("{}/{total}", ops::sync_applied_count(layout, s, &worktrees)),
            };
            (
                s.kind().to_string(),
                s.subject_line(),
                s.dst().unwrap_or("-").to_string(),
                state,
                applied,
            )
        })
        .collect();

    const H: (&str, &str, &str, &str, &str) = (
        "KIND",
        "SOURCE (<repo-root>/…) or COMMAND",
        "DEST (<worktree>/…)",
        "STATE",
        "APPLIED",
    );
    let w = |pick: fn(&(String, String, String, String, String)) -> &String, head: &str| {
        rows.iter()
            .map(|r| pick(r).chars().count())
            .chain(std::iter::once(head.chars().count()))
            .max()
            .unwrap_or(8)
    };
    let (kw, sw, dw, tw) = (
        w(|r| &r.0, H.0),
        w(|r| &r.1, H.1),
        w(|r| &r.2, H.2),
        w(|r| &r.3, H.3),
    );

    println!(
        "{:<kw$}  {:<sw$}  {:<dw$}  {:<tw$}  {}",
        H.0, H.1, H.2, H.3, H.4
    );
    println!(
        "{:<kw$}  {:<sw$}  {:<dw$}  {:<tw$}  {}",
        "-".repeat(kw),
        "-".repeat(sw),
        "-".repeat(dw),
        "-".repeat(tw),
        "-".repeat(H.4.len())
    );
    for r in &rows {
        println!(
            "{:<kw$}  {:<sw$}  {:<dw$}  {:<tw$}  {}",
            r.0, r.1, r.2, r.3, r.4
        );
    }
    Ok(())
}

/// Open the recipe in `$VISUAL` / `$EDITOR`, then refuse to leave a file that
/// does not parse — a broken recipe would otherwise only surface at the next
/// `worktree add`, far from the edit that caused it.
pub fn edit(layout: &BareLayout) -> Result<()> {
    sync::write_starter(layout)?;
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());
    let status = std::process::Command::new(&editor)
        .arg(&layout.sync_config)
        .status()?;
    if !status.success() {
        anyhow::bail!("{editor} exited with {status}");
    }
    match sync::load(layout) {
        Ok(cfg) => {
            eprintln!(
                "{} step(s) in {}",
                cfg.steps.len(),
                layout.sync_config.display()
            );
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}
