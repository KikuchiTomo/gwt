//! Non-interactive counterpart to the picker's conflict menu.
//!
//! The picker asks "what do you want to do?"; on the CLI the answer has to
//! arrive up front as `--reuse` / `--recreate`. What does NOT change is that
//! anything destructive is confirmed before it runs — `--recreate` prompts on
//! the terminal unless `--yes` was passed too.

use std::io::{IsTerminal, Write};

use anyhow::{bail, Result};
use gwt_core::layout::BareLayout;
use gwt_core::{ops, Error};

#[derive(Debug, Clone, Copy, Default)]
pub struct Resolve {
    pub reuse: bool,
    pub recreate: bool,
    pub yes: bool,
}

/// Ask the terminal directly.
///
/// stdin is frequently a pipe here (`curl … | sh`, scripts, `git wt` inside a
/// shell function), so a prompt read from stdin would either block forever or
/// silently read the wrong thing. /dev/tty is the honest channel; if there is no
/// terminal at all we refuse rather than guess.
pub fn confirm(prompt: &str, assume_yes: bool) -> Result<bool> {
    if assume_yes {
        return Ok(true);
    }
    let mut tty = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
    {
        Ok(f) => f,
        Err(_) if !std::io::stdin().is_terminal() => {
            bail!("refusing to run a destructive step without a terminal — pass --yes to confirm");
        }
        Err(e) => bail!("cannot open /dev/tty to confirm: {e}"),
    };
    write!(tty, "{prompt} [y/N] ")?;
    tty.flush()?;
    let mut line = String::new();
    {
        use std::io::BufRead;
        let mut reader = std::io::BufReader::new(tty.try_clone()?);
        reader.read_line(&mut line)?;
    }
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "YES"))
}

/// Describe exactly what is about to be destroyed, then ask.
fn confirm_destroy(layout: &BareLayout, name: &str, branch: &str, yes: bool) -> Result<bool> {
    let dest = layout.root.join(name);
    eprintln!("about to DELETE and re-create:");
    if dest.exists() {
        eprintln!("  worktree  {}", dest.display());
    }
    if ops::branch_exists_local(layout, branch).unwrap_or(false) {
        eprintln!("  branch    {branch} (local commits not on origin are lost)");
    }
    confirm("proceed?", yes)
}

/// Shared fallback for `add` / `new` / `review` once the plain attempt failed.
///
/// `base` is the ref a fresh branch is cut from; `None` means "adopt
/// origin/<branch>", which is what review-style commands want.
pub fn resolve(
    layout: &BareLayout,
    err: Error,
    branch: &str,
    name: &str,
    base: Option<&str>,
    r: Resolve,
) -> Result<std::path::PathBuf> {
    match &err {
        Error::BranchExists(_) if r.reuse => {
            let p = ops::add_existing_branch(layout, branch, name)?;
            eprintln!("reused existing branch '{branch}'");
            Ok(p)
        }
        Error::BranchExists(_) | Error::PathExists(_) if r.recreate => {
            if !confirm_destroy(layout, name, branch, r.yes)? {
                bail!("cancelled");
            }
            let p = ops::recreate_worktree(layout, name, branch, base)?;
            eprintln!("re-created '{name}'");
            Ok(p)
        }
        // No flag covers this one — explain the ways out rather than just fail.
        Error::BranchExists(_) => bail!(
            "{err}\n  · --reuse      check out the existing branch in {name}\n  \
             · --recreate   delete it and re-create from origin (asks first)\n  \
             · or run `git wt` and choose interactively"
        ),
        Error::PathExists(_) => bail!(
            "{err}\n  · cd {name}    go to what is already there\n  \
             · --recreate   delete it and re-create (asks first)\n  \
             · or run `git wt` and choose interactively"
        ),
        _ => Err(err.into()),
    }
}
