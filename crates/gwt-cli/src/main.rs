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
    /// Manage the secrets manifest (TSV of <src>\t<dst>).
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

#[derive(Subcommand, Debug)]
enum SecretOp {
    Add {
        src: String,
        dst: String,
    },
    #[command(alias = "rm")]
    Remove {
        src: String,
    },
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
            PickerOutcome::ChangeDir(p) => println!("{}", p.display()),
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
