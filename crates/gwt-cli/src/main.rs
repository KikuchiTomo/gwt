use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use gwt_core::Repo;
use gwt_tui::{run_display, run_picker, PickerOutcome};

mod commands;

#[derive(Parser, Debug)]
#[command(
    name = "git-wt",
    bin_name = "git wt",
    about = "Cross-platform git worktree helper",
    version
)]
struct Cli {
    /// Fullscreen live dashboard of the worktrees in this repo.
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
    /// List worktrees (plain, no TUI). Good for scripting.
    List,
    /// Create a worktree for `<branch>` (created from HEAD by default).
    Add {
        branch: String,
        /// Override the path (defaults to `<repo-root>/<branch>`).
        #[arg(long)]
        path: Option<PathBuf>,
        /// Use an existing branch instead of creating one.
        #[arg(long)]
        existing: bool,
    },
    /// Remove the worktree at `<path>`.
    Remove {
        path: PathBuf,
        #[arg(long)]
        force: bool,
    },
    /// Create a worktree tracking `origin/<branch>` for a review.
    Review { remote_branch: String },
    /// Print the shell function that gives `git wt` real `cd` support.
    Shellinit {
        #[arg(value_parser = ["bash", "zsh", "fish"], default_value = "bash")]
        shell: String,
    },
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

fn dispatch(cli: Cli) -> Result<()> {
    let cwd = env::current_dir().context("failed to read current dir")?;

    if let Some(Cmd::Shellinit { shell }) = cli.command.as_ref() {
        commands::shellinit::print(shell);
        return Ok(());
    }

    let repo = Repo::discover(&cwd)?;

    if cli.display {
        return run_display(&repo, Duration::from_millis(1500));
    }

    match cli.command {
        None => match run_picker(&repo, cli.height)? {
            PickerOutcome::Cancelled => {}
            PickerOutcome::ChangeDir(p) => {
                // Stdout is the channel the shell wrapper reads to perform `cd`.
                println!("{}", p.display());
            }
        },
        Some(Cmd::List) => commands::list::run(&repo)?,
        Some(Cmd::Add {
            branch,
            path,
            existing,
        }) => commands::add::run(&repo, &branch, path.as_deref(), !existing)?,
        Some(Cmd::Remove { path, force }) => repo.remove_worktree(&path, force)?,
        Some(Cmd::Review { remote_branch }) => commands::review::run(&repo, &remote_branch)?,
        Some(Cmd::Shellinit { .. }) => unreachable!("handled above"),
    }
    Ok(())
}
