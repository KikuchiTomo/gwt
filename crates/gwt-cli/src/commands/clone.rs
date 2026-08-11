use std::io::{IsTerminal, Write};
use std::path::Path;

use anyhow::Result;
use gwt_core::git::Progress;
use gwt_core::ops;

/// A one-line progress bar for git's `\r`-separated updates.
///
/// On a terminal the line is rewritten in place. Anywhere else — a CI log, a
/// `$(...)` capture — rewriting produces thousands of useless lines, so only
/// the phase changes are announced.
pub struct Bar {
    tty: bool,
    phase: String,
    open: bool,
}

const BAR_CELLS: usize = 24;
const PHASE_W: usize = 20;

impl Bar {
    pub fn new() -> Self {
        Self {
            tty: std::io::stderr().is_terminal(),
            phase: String::new(),
            open: false,
        }
    }

    pub fn update(&mut self, p: &Progress) {
        let new_phase = p.phase != self.phase;
        if new_phase {
            // Leave the finished phase on screen rather than overwriting it.
            self.end_line();
            self.phase = p.phase.clone();
        }
        if !self.tty {
            if new_phase {
                eprintln!("  {}…", p.phase);
            }
            return;
        }
        let filled = (BAR_CELLS * p.percent as usize) / 100;
        let mut err = std::io::stderr();
        let _ = write!(
            err,
            "\r\x1b[2K  {phase:<PHASE_W$} {bar}{empty} {pct:>3}%  {detail}",
            phase = truncate(&p.phase, PHASE_W),
            bar = "█".repeat(filled),
            empty = "░".repeat(BAR_CELLS - filled),
            pct = p.percent,
            detail = p.detail,
        );
        let _ = err.flush();
        self.open = true;
    }

    /// Close the current line, keeping whatever is on it.
    fn end_line(&mut self) {
        if self.open {
            eprintln!();
            self.open = false;
        }
    }

    /// Stop drawing. The last phase stays on screen — a clone that scrolled
    /// past leaves a record of what it did, and every earlier phase kept its
    /// line anyway.
    pub fn finish(&mut self) {
        self.end_line();
    }
}

impl Default for Bar {
    fn default() -> Self {
        Self::new()
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    s.chars().take(n.saturating_sub(1)).collect::<String>() + "…"
}

pub fn run(cwd: &Path, url: &str, dir: Option<&str>) -> Result<()> {
    let mut bar = Bar::new();
    let done = ops::clone(url, dir, cwd, &mut |p| bar.update(p));
    bar.finish();
    let done = done?;

    if done.empty_origin {
        // Nothing was checked out because there is nothing to check out. Say so
        // here rather than letting the empty directory raise the question.
        eprintln!(
            "note: {url} has no commits yet — '{}' is on the unborn branch '{}'.\n      \
             commit and `git push -u origin {}` to start it.",
            gwt_core::layout::DEFAULT_WT_NAME,
            done.branch,
            done.branch
        );
    }
    println!("{}", done.root.display());
    Ok(())
}
