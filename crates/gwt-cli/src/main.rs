use std::env;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use gwt_core::layout::BareLayout;
use gwt_core::Repo;
use gwt_tui::{run_display, run_picker, PickerOutcome};

mod commands;

#[derive(Parser, Debug)]
#[command(
    name = "git-wt",
    bin_name = "git wt",
    about = "Cross-platform git worktree helper (bare-style layout)",
    version
)]
struct Cli {
    /// Fullscreen live dashboard.
    #[arg(long, global = true)]
    display: bool,

    /// Inline picker height in lines (fzf-style); only used in picker mode.
    #[arg(long, default_value_t = 15, global = true)]
    height: u16,

    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Clone <url> into a bare-style worktree root, then add a `default` worktree.
    Clone { url: String, dir: Option<String> },
    /// Adopt an existing branch (local or origin) as a new worktree at <name>.
    Add { branch: String, name: String },
    /// Create a brand-new branch from <base> in worktree directory <name>.
    New {
        base: String,
        branch: String,
        name: String,
    },
    /// Fetch origin/<branch> and create a tracking worktree for review.
    Review { branch: String },
    /// Remove worktree directory <name> and delete the local branch.
    #[command(alias = "rm")]
    Remove { name: String },
    /// Rich list of worktrees with ahead/behind, dirty, and stash columns.
    #[command(alias = "ls")]
    List,
    /// Compare local <branch> against origin/<branch>.
    Check {
        branch: String,
        #[arg(long)]
        fetch: bool,
    },
    /// Manage secret files that are symlinked into every worktree.
    #[command(long_about = SECRET_ABOUT)]
    Secret {
        #[command(subcommand)]
        op: SecretOp,
    },
    /// Re-apply secret links to every existing worktree.
    Relink,
    /// Convert worktree gitdir pointers to relative paths.
    Relativize { name: Option<String> },
    /// Print the shell function that gives `git wt` real `cd` support.
    Shellinit {
        #[arg(value_parser = ["bash", "zsh", "fish"], default_value = "bash")]
        shell: String,
    },
}

/// The two columns use different bases, and that is the whole confusion — spell
/// it out wherever the user can see it.
const SECRET_ABOUT: &str = "\
Manage secret files that are symlinked into every worktree.

The real file lives once in the repo root; each worktree gets a symlink to it.
The two paths are relative to DIFFERENT places:

  SOURCE            relative to the REPO ROOT      (where .git / .bare / secrets/ live)
  DEST_IN_WORKTREE  relative to EACH WORKTREE ROOT (created in every worktree)

  <repo-root>/
  ├── secrets/.env                          <- SOURCE            = secrets/.env
  ├── default/.env    -> ../secrets/.env    <- DEST_IN_WORKTREE  = .env
  └── feature-a/.env  -> ../secrets/.env    <- DEST_IN_WORKTREE  = .env

Example:
  git wt secret add secrets/.env .env
  git wt secret add secrets/gcp.json config/gcp.json

`add` and `rm` take effect immediately in every existing worktree; `relink` is
only needed after creating the source file later, or to repair links by hand.";

const SECRET_ADD_ABOUT: &str = "\
Register a secret and link it into every existing worktree right away.

  SOURCE            path of the real file, relative to the REPO ROOT.
                    An absolute path inside the root works too.
  DEST_IN_WORKTREE  path the symlink takes inside EACH WORKTREE, relative to
                    that worktree's root. Must be relative.

Example (run from the repo root):
  git wt secret add secrets/.env .env
    -> <repo-root>/default/.env   -> <repo-root>/secrets/.env
    -> <repo-root>/feature-a/.env -> <repo-root>/secrets/.env";

#[derive(Subcommand, Debug)]
enum SecretOp {
    /// Register a secret and link it into every worktree now.
    #[command(long_about = SECRET_ADD_ABOUT)]
    Add {
        /// Real file, relative to the REPO ROOT (e.g. secrets/.env).
        #[arg(value_name = "SOURCE")]
        src: String,
        /// Where the link appears in EACH WORKTREE, relative to its root (e.g. .env).
        #[arg(value_name = "DEST_IN_WORKTREE")]
        dst: String,
    },
    /// Unregister a secret and remove its link from every worktree now.
    #[command(alias = "rm")]
    Remove {
        /// Source path as shown in the SOURCE column of `git wt secret ls`.
        #[arg(value_name = "SOURCE")]
        src: String,
    },
    /// Show every mapping with both bases spelled out, plus link health.
    #[command(alias = "list")]
    Ls,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match dispatch(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("git wt: {e:#}");
            ExitCode::from(1)
        }
    }
}

/// Hand the chosen worktree path back to the shell wrapper.
///
/// We prefer a file channel (`GWT_CD_FILE`) over stdout so that stdout stays
/// connected to the terminal. The inline picker's crossterm cursor-position
/// probe (`ESC[6n`) is written to stdout; if the wrapper captured stdout via
/// `$(...)`, that probe would go to the pipe instead of the terminal — failing
/// (so the inline viewport falls back to the alt-screen) and leaking its escape
/// bytes into the captured path (so the `cd` never happens). Writing the path to
/// a file keeps stdout clean for the probe. Falls back to stdout when the env var
/// is absent, so older shell wrappers keep working.
fn emit_cd_target(path: &std::path::Path) -> Result<()> {
    if let Some(file) = env::var_os("GWT_CD_FILE") {
        std::fs::write(&file, path.to_string_lossy().as_bytes())
            .with_context(|| format!("failed to write cd target to {}", file.to_string_lossy()))?;
    } else {
        println!("{}", path.display());
    }
    Ok(())
}

fn dispatch(cli: Cli) -> Result<()> {
    let cwd = env::current_dir().context("failed to read current dir")?;

    // shellinit needs no repo context.
    if let Some(Cmd::Shellinit { shell }) = cli.command.as_ref() {
        commands::shellinit::print(shell);
        return Ok(());
    }
    // clone runs before any bare-root exists.
    if let Some(Cmd::Clone { url, dir }) = cli.command.as_ref() {
        return commands::clone::run(&cwd, url, dir.as_deref());
    }

    // --display and the bare default picker accept either bare or normal repos.
    if cli.display {
        let repo = Repo::discover(&cwd)?;
        return run_display(&repo, Duration::from_millis(1500));
    }
    if cli.command.is_none() {
        let repo = Repo::discover(&cwd)?;
        match run_picker(&repo, cli.height)? {
            PickerOutcome::Cancelled => {}
            PickerOutcome::ChangeDir(p) => emit_cd_target(&p)?,
        }
        return Ok(());
    }

    // Everything else requires the bare-style layout.
    let layout = BareLayout::require(&cwd)?;
    match cli.command.unwrap() {
        Cmd::Add { branch, name } => commands::add::run(&layout, &branch, &name)?,
        Cmd::New { base, branch, name } => commands::new::run(&layout, &base, &branch, &name)?,
        Cmd::Review { branch } => commands::review::run(&layout, &branch)?,
        Cmd::Remove { name } => commands::remove::run(&layout, &name)?,
        Cmd::List => commands::list::run(&layout)?,
        Cmd::Check { branch, fetch } => commands::check::run(&layout, &branch, fetch)?,
        Cmd::Secret { op } => match op {
            SecretOp::Add { src, dst } => commands::secret::add(&layout, &src, &dst)?,
            SecretOp::Remove { src } => commands::secret::remove(&layout, &src)?,
            SecretOp::Ls => commands::secret::ls(&layout)?,
        },
        Cmd::Relink => commands::relink::run(&layout)?,
        Cmd::Relativize { name } => commands::relativize::run(&layout, name.as_deref())?,
        Cmd::Clone { .. } | Cmd::Shellinit { .. } => unreachable!("handled above"),
    }
    Ok(())
}
