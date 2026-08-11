use std::env;
use std::io::IsTerminal;
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

    /// Interface language: en or ja. Overrides $GWT_LANG and the stored config.
    #[arg(long, global = true, value_name = "CODE")]
    lang: Option<String>,

    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Clone <url> into a bare-style worktree root, then add a `default` worktree.
    Clone { url: String, dir: Option<String> },
    /// Adopt an existing branch (local or origin) as a new worktree at <name>.
    Add {
        branch: String,
        name: String,
        #[command(flatten)]
        conflict: ConflictFlags,
    },
    /// Create a brand-new branch from <base> in worktree directory <name>.
    New {
        base: String,
        branch: String,
        name: String,
        #[command(flatten)]
        conflict: ConflictFlags,
    },
    /// Fetch origin/<branch> and create a tracking worktree for review.
    Review {
        branch: String,
        #[command(flatten)]
        conflict: ConflictFlags,
    },
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
    /// Manage what every worktree gets that git does not carry: linked files,
    /// copied files, and commands. With no subcommand, opens the manager.
    #[command(long_about = SYNC_ABOUT)]
    Sync {
        #[command(subcommand)]
        op: Option<SyncOp>,
    },
    /// Inspect and maintain the build caches mounted into worktrees.
    #[command(long_about = CACHE_ABOUT)]
    Cache {
        #[command(subcommand)]
        op: CacheOp,
    },
    /// Renamed to `sync`.
    #[command(hide = true)]
    Secret {
        #[command(subcommand)]
        op: Option<SyncOp>,
    },
    /// Renamed to `sync apply`.
    #[command(hide = true)]
    Relink,
    /// Convert worktree gitdir pointers to relative paths.
    Relativize { name: Option<String> },
    /// Show or change stored settings (currently: the interface language).
    Config {
        #[command(subcommand)]
        op: Option<ConfigOp>,
    },
    /// Print the shell function that gives `git wt` real `cd` support.
    Shellinit {
        #[arg(value_parser = ["bash", "zsh", "fish"], default_value = "bash")]
        shell: String,
    },
}

/// The two columns use different bases, and that is the whole confusion — spell
/// it out wherever the user can see it.
const SYNC_ABOUT: &str = "\
Manage what every worktree needs that git does not carry.

The recipe lives at <repo-root>/.gwt/sync.toml, outside every worktree, and
holds an ordered list of steps. Order matters: put `.env` in place before the
command that reads it.

  link  symlink one real file into every worktree (the old `secret`)
  copy  copy it instead, for files a tool rewrites in place
  run   run a command in a worktree, by default only when it is created

The two path columns are relative to DIFFERENT places:

  SOURCE            relative to the REPO ROOT      (where .git / .bare / secrets/ live)
  DEST_IN_WORKTREE  relative to EACH WORKTREE ROOT (created in every worktree)

  <repo-root>/
  ├── .gwt/sync.toml                        <- the recipe
  ├── secrets/.env                          <- SOURCE            = secrets/.env
  ├── default/.env    -> ../secrets/.env    <- DEST_IN_WORKTREE  = .env
  └── feature-a/.env  -> ../secrets/.env    <- DEST_IN_WORKTREE  = .env

Example:
  git wt sync add secrets/.env .env
  git wt sync copy secrets/env.sample .env --render
  git wt sync run 'npm ci' --only-if package.json

link/copy/rm take effect immediately in every existing worktree. `apply` is for
repairing them later, or after creating a source file that did not exist yet.

A `run` step is only ever read from .gwt/sync.toml, which is not inside any
worktree and therefore not tracked by git: `git pull` cannot add one.";

const SYNC_LINK_ABOUT: &str = "\
Register a symlink and create it in every existing worktree right away.

  SOURCE            path of the real file, relative to the REPO ROOT.
                    An absolute path inside the root works too.
  DEST_IN_WORKTREE  path the symlink takes inside EACH WORKTREE, relative to
                    that worktree's root. Must be relative.

Example (run from the repo root):
  git wt sync add secrets/.env .env
    -> <repo-root>/default/.env   -> <repo-root>/secrets/.env
    -> <repo-root>/feature-a/.env -> <repo-root>/secrets/.env";

const SYNC_COPY_ABOUT: &str = "\
Copy a file into every worktree instead of linking it.

Use this when the file must be a real file: something a tool rewrites in place,
or a per-worktree config that starts from a shared template.

An existing file at the destination is left alone unless --overwrite, so a copy
you edited inside a worktree survives `git wt sync apply`.

With --render, these are substituted while copying:
  {{branch}}  {{worktree}}  {{worktree_name}}  {{root}}";

const CACHE_ABOUT: &str = "\
Build caches that survive the worktree they were built in.

A worktree starts empty, so every build system starts cold: six worktrees means
six target/ directories and six full builds. A cache step moves that directory
out of the worktree and symlinks it back in, so the data outlives the worktree
and can be shared with the worktrees it is safe to share with.

Which worktrees those are is decided by file contents, not by a promise:

  keyed    share only with worktrees whose key files are byte-identical
           (the default — change Cargo.lock and this worktree gets its own
           bucket automatically, change it back and it returns to the shared
           one, still warm)
  shared   one bucket for the repo, for caches that cannot be poisoned
  private  one bucket per worktree, which still outlives it

Add one with `git wt sync cache`, or let `git wt cache init` detect what this
project is built with. `git wt cache ls` shows the buckets and their sizes,
`git wt cache gc` deletes the ones no worktree points at.

The real data lives in <repo-root>/.gwt/cache/, outside every worktree, and the
mount point is added to the clone-local `info/exclude` so git stays quiet
without touching a tracked .gitignore.";

const SYNC_CACHE_ABOUT: &str = "\
Mount a build cache from outside the worktree.

  DIR   the directory to cache, relative to EACH WORKTREE ROOT (e.g. target)

An existing directory at that path is adopted — moved into its bucket, not
deleted — so bringing a warm 4 GB target/ under management costs nothing.

  --mode keyed    share with worktrees whose --key files match (default)
  --mode shared   one bucket for the whole repo
  --mode private  one bucket per worktree, outliving the worktree

Example:
  git wt sync cache target --key Cargo.lock --env CARGO_TARGET_DIR
  git wt sync cache node_modules --key package-lock.json";

const SYNC_RUN_ABOUT: &str = "\
Register a command to run inside a worktree.

By default it runs when a worktree is created, and not on a plain
`git wt sync apply` — re-running someone's `npm ci` because they repaired a
symlink would be its own surprise. Pass --when to change that.

The command runs through the shell, from the worktree root, with:
  GWT_ROOT  GWT_WORKTREE  GWT_WORKTREE_NAME  GWT_BRANCH

Example:
  git wt sync run 'npm ci' --only-if package.json --timeout 10m";

#[derive(Subcommand, Debug)]
enum CacheOp {
    /// Show every bucket: size, and which worktrees use it.
    #[command(alias = "list")]
    Ls,
    /// Delete buckets no worktree points at. Asks first.
    Gc {
        /// Only those untouched for this many days.
        #[arg(long, value_name = "DAYS")]
        older_than: Option<u64>,
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Detect what this project is built with and propose cache steps.
    Init {
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Print `export VAR=<bucket>` lines for the caches that declare one.
    Env {
        /// Worktree to compute for; defaults to the current directory.
        #[arg(value_name = "WORKTREE")]
        worktree: Option<String>,
    },
    /// Install git hooks that re-check the cache keys after a checkout or merge.
    Hooks {
        /// Remove them instead.
        #[arg(long)]
        remove: bool,
    },
}

#[derive(Subcommand, Debug)]
enum ConfigOp {
    /// Set the interface language (writes ~/.config/gwt/config).
    Lang {
        #[arg(value_parser = ["en", "ja"])]
        code: String,
    },
}

/// Non-interactive answers to the conflicts the picker resolves with a menu.
#[derive(clap::Args, Debug, Clone, Copy)]
struct ConflictFlags {
    /// If the local branch already exists, check it out in the new worktree
    /// instead of failing.
    #[arg(long)]
    reuse: bool,

    /// If the worktree or branch already exists, DELETE it and build it again
    /// from the remote (or <base>). Asks for confirmation unless --yes.
    #[arg(long)]
    recreate: bool,

    /// Skip the confirmation prompt for --recreate. Destructive.
    #[arg(long, short = 'y')]
    yes: bool,
}

impl From<ConflictFlags> for commands::conflict::Resolve {
    fn from(f: ConflictFlags) -> Self {
        Self {
            reuse: f.reuse,
            recreate: f.recreate,
            yes: f.yes,
        }
    }
}

#[derive(Subcommand, Debug)]
enum SyncOp {
    /// Symlink a file into every worktree.
    #[command(alias = "add", long_about = SYNC_LINK_ABOUT)]
    Link {
        /// Real file, relative to the REPO ROOT (e.g. secrets/.env).
        #[arg(value_name = "SOURCE")]
        src: String,
        /// Where it appears in EACH WORKTREE, relative to its root (e.g. .env).
        #[arg(value_name = "DEST_IN_WORKTREE")]
        dst: String,
    },
    /// Copy a file into every worktree, rather than linking it.
    #[command(long_about = SYNC_COPY_ABOUT)]
    Copy {
        #[arg(value_name = "SOURCE")]
        src: String,
        #[arg(value_name = "DEST_IN_WORKTREE")]
        dst: String,
        /// Replace a file already sitting at the destination.
        #[arg(long)]
        overwrite: bool,
        /// Substitute {{branch}}, {{worktree}}, {{worktree_name}} and {{root}}.
        #[arg(long)]
        render: bool,
    },
    /// Run a command inside a worktree.
    #[command(long_about = SYNC_RUN_ABOUT)]
    Run {
        /// The command line, run through the shell.
        #[arg(value_name = "COMMAND")]
        cmd: String,
        /// Only run where this path exists inside the worktree.
        #[arg(long, value_name = "PATH")]
        only_if: Option<String>,
        /// Give up after this long (30s, 10m, 1h).
        #[arg(long, default_value = "10m")]
        timeout: String,
        /// Working directory, relative to the worktree root.
        #[arg(long, value_name = "SUBDIR")]
        dir: Option<String>,
        /// When it fires: create, apply, manual (repeatable, comma-separated).
        #[arg(long, value_delimiter = ',', default_value = "create")]
        when: Vec<String>,
    },
    /// Mount a build cache from outside the worktree.
    #[command(long_about = SYNC_CACHE_ABOUT)]
    Cache {
        /// Directory to cache, relative to each worktree root (e.g. target).
        #[arg(value_name = "DIR")]
        path: String,
        /// keyed (default), shared, or private.
        #[arg(long, default_value = "keyed", value_parser = ["keyed", "shared", "private"])]
        mode: String,
        /// Files whose contents decide who shares the cache. `keyed` only.
        #[arg(long, value_name = "FILE", num_args = 1..)]
        key: Vec<String>,
        /// Do not fill a new bucket from the most recent one.
        #[arg(long)]
        no_seed: bool,
        /// Environment variable that points a tool at the bucket.
        #[arg(long, value_name = "VAR")]
        env: Option<String>,
    },
    /// Unregister a step and undo it in every worktree now.
    #[command(alias = "remove")]
    Rm {
        /// A destination, a source, or a command line, as shown by `sync ls`.
        #[arg(value_name = "STEP")]
        key: String,
    },
    /// Show every step, with both bases spelled out, plus its health.
    #[command(alias = "list")]
    Ls,
    /// Re-apply the recipe to every existing worktree.
    #[command(alias = "relink")]
    Apply {
        /// Also fire `run` steps, whatever their `when` says.
        #[arg(long)]
        run: bool,
    },
    /// Open .gwt/sync.toml in $VISUAL / $EDITOR and check it parses.
    Edit,
}

/// Turn one parsed subcommand into the step it describes.
fn step_from(op: &SyncOp, layout: &BareLayout) -> Result<gwt_core::sync::Step> {
    use gwt_core::cache::{CacheMode, CacheStep};
    use gwt_core::sync::{normalize_dst, normalize_src, CopyStep, LinkStep, Phase, RunStep, Step};
    Ok(match op {
        SyncOp::Cache {
            path,
            mode,
            key,
            no_seed,
            env,
        } => {
            let mode = CacheMode::parse(mode)
                .ok_or_else(|| anyhow::anyhow!("--mode takes keyed, shared or private"))?;
            if mode == CacheMode::Keyed && key.is_empty() {
                anyhow::bail!(
                    "a keyed cache needs --key: the files whose contents decide which \
                     worktrees may share it (e.g. --key Cargo.lock)"
                );
            }
            Step::Cache(CacheStep {
                path: normalize_dst(path)?,
                mode,
                key: key.clone(),
                seed: !no_seed,
                env: env.clone(),
            })
        }
        SyncOp::Link { src, dst } => Step::Link(LinkStep {
            src: normalize_src(layout, src)?,
            dst: normalize_dst(dst)?,
        }),
        SyncOp::Copy {
            src,
            dst,
            overwrite,
            render,
        } => Step::Copy(CopyStep {
            src: normalize_src(layout, src)?,
            dst: normalize_dst(dst)?,
            overwrite: *overwrite,
            render: *render,
        }),
        SyncOp::Run {
            cmd,
            only_if,
            timeout,
            dir,
            when,
        } => {
            let mut phases = Vec::new();
            for w in when {
                phases.push(match w.trim() {
                    "create" => Phase::Create,
                    "apply" => Phase::Apply,
                    "manual" => Phase::Manual,
                    other => anyhow::bail!("--when takes create, apply or manual, not '{other}'"),
                });
            }
            Step::Run(RunStep {
                cmd: cmd.clone(),
                when: phases,
                only_if: only_if.clone(),
                timeout: gwt_core::sync::parse_timeout(timeout)
                    .ok_or_else(|| anyhow::anyhow!("--timeout looks like 30s, 10m or 1h"))?,
                dir: dir.clone(),
            })
        }
        _ => unreachable!("not a step-producing subcommand"),
    })
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    // Fix the locale before anything can render a string.
    gwt_core::i18n::set(gwt_core::i18n::detect(cli.lang.as_deref()));
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
        return Ok(());
    }
    // No wrapper. A terminal on stdout means nobody is capturing the path, so
    // printing it accomplishes nothing and Enter just looked broken — explain
    // it instead. Otherwise something *is* reading stdout (`cd "$(git wt)"`),
    // so keep emitting the bare path.
    if std::io::stdout().is_terminal() {
        eprintln!(
            "{}",
            gwt_core::t::cd_integration_missing(&path.display().to_string(), &login_shell())
        );
    } else {
        println!("{}", path.display());
    }
    Ok(())
}

/// Which `shellinit` line to suggest. `$SHELL` is the user's login shell, which
/// is the rc they would have to edit; anything unrecognised gets the bash form,
/// which zsh also accepts.
fn login_shell() -> String {
    let raw = env::var("SHELL").unwrap_or_default();
    let name = std::path::Path::new(&raw)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    match name.as_str() {
        "zsh" | "fish" | "bash" => name,
        _ => "bash".into(),
    }
}

fn dispatch(cli: Cli) -> Result<()> {
    let cwd = env::current_dir().context("failed to read current dir")?;

    // config and shellinit need no repo context.
    if let Some(Cmd::Config { op }) = cli.command.as_ref() {
        return commands::config::run(op.as_ref().map(|ConfigOp::Lang { code }| code.as_str()));
    }
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
        Cmd::Add {
            branch,
            name,
            conflict,
        } => commands::add::run(&layout, &branch, &name, conflict.into())?,
        Cmd::New {
            base,
            branch,
            name,
            conflict,
        } => commands::new::run(&layout, &base, &branch, &name, conflict.into())?,
        Cmd::Review { branch, conflict } => {
            commands::review::run(&layout, &branch, conflict.into())?
        }
        Cmd::Remove { name } => commands::remove::run(&layout, &name)?,
        Cmd::List => commands::list::run(&layout)?,
        Cmd::Check { branch, fetch } => commands::check::run(&layout, &branch, fetch)?,
        Cmd::Sync { op } => run_sync(&layout, op)?,
        Cmd::Cache { op } => match op {
            CacheOp::Ls => commands::cache::ls(&layout)?,
            CacheOp::Gc { older_than, yes } => commands::cache::gc(&layout, older_than, yes)?,
            CacheOp::Init { yes } => commands::cache::init(&layout, yes)?,
            CacheOp::Env { worktree } => commands::cache::env(&layout, worktree.as_deref())?,
            CacheOp::Hooks { remove } => commands::cache::hooks(&layout, remove)?,
        },
        Cmd::Secret { op } => {
            eprintln!("git wt: `secret` is now `sync` — same thing, plus copy and run steps.");
            run_sync(&layout, op)?
        }
        Cmd::Relink => {
            eprintln!("git wt: `relink` is now `sync apply`.");
            commands::sync::apply(&layout, false)?
        }
        Cmd::Relativize { name } => commands::relativize::run(&layout, name.as_deref())?,
        Cmd::Clone { .. } | Cmd::Shellinit { .. } | Cmd::Config { .. } => {
            unreachable!("handled above")
        }
    }
    Ok(())
}

fn run_sync(layout: &BareLayout, op: Option<SyncOp>) -> Result<()> {
    match op {
        None => gwt_tui::run_sync_manager(layout)?,
        Some(SyncOp::Ls) => commands::sync::ls(layout)?,
        Some(SyncOp::Rm { key }) => commands::sync::remove(layout, &key)?,
        Some(SyncOp::Apply { run }) => commands::sync::apply(layout, run)?,
        Some(SyncOp::Edit) => commands::sync::edit(layout)?,
        Some(op) => commands::sync::add(layout, step_from(&op, layout)?)?,
    }
    Ok(())
}
