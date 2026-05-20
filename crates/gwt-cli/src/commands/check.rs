use anyhow::Result;
use gwt_core::layout::BareLayout;
use gwt_core::ops;

const C_RESET: &str = "\x1b[0m";
const C_GREEN: &str = "\x1b[32m";
const C_YELLOW: &str = "\x1b[33m";
const C_CYAN: &str = "\x1b[36m";

pub fn run(layout: &BareLayout, branch: &str, do_fetch: bool) -> Result<()> {
    let r = ops::check(layout, branch, do_fetch)?;
    if !r.has_remote {
        eprintln!(
            "{C_YELLOW}origin/{br} not found{C_RESET} (hint: --fetch)",
            br = r.branch
        );
        return Ok(());
    }
    let remote = r.remote_short.as_deref().unwrap_or("?");
    if let Some(local) = r.local_short.as_deref() {
        println!("  local  {} ({})", r.branch, local);
        println!("  remote origin/{} ({})", r.branch, remote);
        match (r.ahead, r.behind) {
            (0, 0) => println!("{C_GREEN}up to date with origin/{}{C_RESET}", r.branch),
            (a, 0) => println!("{C_CYAN}local is {a} commit(s) ahead of origin{C_RESET}"),
            (0, b) => {
                println!("{C_YELLOW}local is {b} commit(s) BEHIND origin — pull before branching{C_RESET}")
            }
            (a, b) => {
                println!("{C_YELLOW}DIVERGED: {a} ahead, {b} behind — rebase/merge before branching{C_RESET}")
            }
        }
    } else {
        println!("  local branch '{}' does not exist", r.branch);
        println!("  remote origin/{} ({})", r.branch, remote);
        println!(
            "branching from origin/{} starts at the remote tip",
            r.branch
        );
    }
    Ok(())
}
