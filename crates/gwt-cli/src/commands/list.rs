use anyhow::Result;
use gwt_core::layout::BareLayout;
use gwt_core::status::{ahead_behind, dirty_count, stash_map, AheadBehind};
use gwt_core::worktree::WorktreeStatus;
use gwt_core::Repo;

const C_RESET: &str = "\x1b[0m";
const C_BOLD: &str = "\x1b[1m";
const C_RED: &str = "\x1b[31m";
const C_GREEN: &str = "\x1b[32m";
const C_YELLOW: &str = "\x1b[33m";
const C_CYAN: &str = "\x1b[36m";
const C_MAGENTA: &str = "\x1b[35m";
const C_DIM: &str = "\x1b[90m";

struct Row {
    name: String,
    branch: String,
    remote: String,
    remote_plain: String,
    dirty: String,
    dirty_plain: String,
    stash: String,
}

pub fn run(layout: &BareLayout) -> Result<()> {
    let repo = Repo::discover(&layout.root)?;
    let worktrees = repo.list_worktrees()?;
    let stashes = stash_map(layout).unwrap_or_default();

    let mut rows = Vec::new();
    for w in &worktrees {
        if w.status == WorktreeStatus::Bare || w.path == layout.root {
            continue;
        }
        let branch = w.short_branch();
        let branch_for_remote = w.branch.as_ref().and_then(|_| {
            // ahead/behind only makes sense for an actual branch ref, not detached HEAD.
            if branch.starts_with('(') {
                None
            } else {
                Some(branch.as_str())
            }
        });
        let ab = branch_for_remote.and_then(|b| ahead_behind(layout, b).ok().flatten());
        let (remote_plain, remote) = format_remote(ab);
        let dirty_n = dirty_count(&w.path).ok();
        let (dirty_plain, dirty) = format_dirty(dirty_n);
        let stash = stashes
            .get(branch.as_str())
            .copied()
            .unwrap_or(0)
            .to_string();
        rows.push(Row {
            name: w.name(),
            branch,
            remote,
            remote_plain,
            dirty,
            dirty_plain,
            stash,
        });
    }

    if rows.is_empty() {
        eprintln!("{C_YELLOW}No worktrees yet.{C_RESET}");
        eprintln!("create one with:");
        eprintln!("  git wt add <branch> <name>            # existing branch");
        eprintln!("  git wt new <base> <branch> <name>     # new branch");
        return Ok(());
    }

    render(rows);
    Ok(())
}

fn format_remote(ab: Option<AheadBehind>) -> (String, String) {
    match ab {
        None => ("—".into(), format!("{C_DIM}—{C_RESET}")),
        Some(AheadBehind {
            ahead: 0,
            behind: 0,
        }) => ("=".into(), format!("{C_GREEN}={C_RESET}")),
        Some(AheadBehind { ahead, behind }) => {
            let plain = format!("↑{ahead} ↓{behind}");
            let color = if behind == 0 {
                C_CYAN
            } else if ahead == 0 {
                C_YELLOW
            } else {
                C_MAGENTA
            };
            (plain.clone(), format!("{color}{plain}{C_RESET}"))
        }
    }
}

fn format_dirty(n: Option<u32>) -> (String, String) {
    match n {
        None => ("?".into(), format!("{C_DIM}?{C_RESET}")),
        Some(0) => ("0".into(), "0".into()),
        Some(n) => (n.to_string(), format!("{C_RED}{n}{C_RESET}")),
    }
}

fn render(rows: Vec<Row>) {
    let mut w_name = "WORKTREE".len();
    let mut w_branch = "BRANCH".len();
    let mut w_remote = "REMOTE".len();
    let mut w_dirty = "DIRTY".len();
    let mut w_stash = "STASH".len();
    for r in &rows {
        w_name = w_name.max(visible_width(&r.name));
        w_branch = w_branch.max(visible_width(&r.branch));
        w_remote = w_remote.max(visible_width(&r.remote_plain));
        w_dirty = w_dirty.max(visible_width(&r.dirty_plain));
        w_stash = w_stash.max(visible_width(&r.stash));
    }
    let gap = "  ";

    print!("{C_BOLD}");
    print_padded("WORKTREE", w_name, gap, false);
    print_padded("BRANCH", w_branch, gap, false);
    print_padded("REMOTE", w_remote, gap, false);
    print_padded("DIRTY", w_dirty, gap, false);
    print_padded("STASH", w_stash, "", true);
    println!("{C_RESET}");

    println!(
        "{n}{g}{b}{g}{r}{g}{d}{g}{s}",
        n = "-".repeat(w_name),
        b = "-".repeat(w_branch),
        r = "-".repeat(w_remote),
        d = "-".repeat(w_dirty),
        s = "-".repeat(w_stash),
        g = gap,
    );

    for r in rows {
        print_padded(&r.name, w_name, gap, false);
        print_padded(&r.branch, w_branch, gap, false);
        print_colored(&r.remote, &r.remote_plain, w_remote, gap);
        print_colored(&r.dirty, &r.dirty_plain, w_dirty, gap);
        print!("{}", r.stash);
        println!();
    }
}

fn print_padded(text: &str, width: usize, gap: &str, last: bool) {
    let w = visible_width(text);
    let pad = width.saturating_sub(w);
    print!("{text}{:pad$}", "", pad = pad);
    if !last {
        print!("{gap}");
    }
}

fn print_colored(colored: &str, plain: &str, width: usize, gap: &str) {
    let w = visible_width(plain);
    let pad = width.saturating_sub(w);
    print!("{colored}{:pad$}{gap}", "", pad = pad);
}

fn visible_width(s: &str) -> usize {
    // Count chars (not bytes); ANSI escapes don't appear in plain strings we measure.
    s.chars().count()
}
