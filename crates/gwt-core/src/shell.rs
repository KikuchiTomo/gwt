//! How a `run` step's command is handed to a shell.
//!
//! The old answer was `sh -c "<cmd>"`, and it was wrong in the way that is
//! hardest to debug: it works everywhere except the repositories that pin their
//! toolchain per directory. A recipe that says `bundle install` fails with
//! "command not found: bundle", or quietly installs against the wrong Ruby,
//! while the very same line typed into the very same directory works fine.
//!
//! Three separate things cause that, and all three are fixed here:
//!
//! 1. **`sh` is not your shell.** rbenv, nodenv, pyenv, nvm, asdf and mise are
//!    almost always wired up in `~/.zshrc` / `~/.bashrc` — an *interactive* rc
//!    file. `sh -c` reads no rc at all, so the shims never join `PATH` and
//!    `nvm`, which is a shell function and nothing else, does not exist. So we
//!    start the user's own `$SHELL` with the flags their terminal starts it
//!    with: interactive, and login too where that is what terminals do.
//!
//!    With no terminal to be interactive on — a script, a git hook, CI, output
//!    on its way into a pipe — a *login* shell is what is left, and a login
//!    shell is not a quieter interactive one: zsh reads `~/.zshrc` only when it
//!    is interactive, and bash skips `~/.bashrc` outright. On macOS that is
//!    worse than doing nothing at all, because `/etc/zprofile` runs
//!    `path_helper`, which rebuilds `PATH` with the system directories first —
//!    so the rbenv shims we inherited move *behind* `/usr/bin`, and with no
//!    `~/.zshrc` to put them back, `bundle` becomes `/usr/bin/bundle` running
//!    the system Ruby 2.6, which then cannot find the bundler its
//!    `Gemfile.lock` asks for. So a login shell standing in for an interactive
//!    one sources the interactive rc itself — see [`Plan::source_rc`].
//!
//! 2. **The environment was pinned to another directory.** If gwt was launched
//!    from a shell sitting in another project — with a virtualenv activated, or
//!    from inside `bundle exec` — then `BUNDLE_GEMFILE`, `GEM_HOME`,
//!    `RUBYOPT`, `VIRTUAL_ENV` and friends are inherited, and they *override*
//!    whatever the new worktree's `.ruby-version` asks for. A human opening a
//!    new terminal never sees those. We drop them, and let the rc we are about
//!    to run put back whatever the user actually sets for themselves.
//!
//! 3. **Nothing ever changed directory.** Starting the shell with its cwd
//!    already set is not the same as `cd`-ing into the worktree: zsh and fish
//!    fire `chpwd` hooks on a real `cd`, and that is how direnv, mise and the
//!    `.nvmrc` auto-switchers notice where they are. So the shell starts at the
//!    repo root and the script `cd`s from there.
//!
//! `shell = "posix"` on a step brings back the old `sh -c` behaviour verbatim,
//! including the inherited environment, for a recipe that wants a plain POSIX
//! shell and nothing else.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Which shell a `run` step's command is handed to.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Shell {
    /// The user's `$SHELL`, started the way their terminal starts it, so every
    /// rc file that sets up a version manager runs. With no terminal to be
    /// interactive on — a script, a hook, CI — it falls back to a login shell.
    #[default]
    Auto,
    /// The user's `$SHELL` as a login shell only. For a setup that lives in
    /// `.zprofile` / `.bash_profile`, or an interactive rc too slow or too
    /// chatty to run in front of every command.
    Login,
    /// `sh -c`, with the environment exactly as gwt received it. What every
    /// `run` step did before 0.9.
    Posix,
    /// A shell named by the recipe: `"bash"`, `"bash -euo pipefail"`,
    /// `"/opt/homebrew/bin/fish -l"`. `-c` is appended unless it is already
    /// there.
    Named(String),
}

impl Shell {
    pub fn parse(s: &str) -> Option<Shell> {
        match s.trim() {
            "" => None,
            "auto" => Some(Shell::Auto),
            "login" => Some(Shell::Login),
            "posix" | "sh" => Some(Shell::Posix),
            other => Some(Shell::Named(other.to_string())),
        }
    }

    /// What to write back into `.gwt/sync.toml`.
    pub fn as_str(&self) -> &str {
        match self {
            Shell::Auto => "auto",
            Shell::Login => "login",
            Shell::Posix => "posix",
            Shell::Named(s) => s,
        }
    }

    pub fn is_default(&self) -> bool {
        *self == Shell::Auto
    }
}

/// Which syntax the prologue has to be written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Posix,
    Fish,
    /// `cmd.exe`, which takes one line and no prologue.
    Cmd,
}

/// A resolved shell invocation: the program, its flags, and what we may assume
/// about the shell they start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub program: OsString,
    /// Everything before the command itself, `-c` included.
    pub args: Vec<OsString>,
    pub dialect: Dialect,
    /// The shell will read the user's rc files, so the environment it starts
    /// with is a starting point rather than the final word — see [`unpin`].
    pub reads_rc: bool,
    /// Job control is on, so a command would land in its own process group and
    /// outlive a killed shell. The prologue turns it back off.
    pub interactive: bool,
    /// An interactive rc this shell will not read on its own, for the prologue
    /// to source — written as a shell word (`${ZDOTDIR:-$HOME}/.zshrc`) rather
    /// than a resolved path, because `~/.zshenv` is allowed to move it.
    ///
    /// Only ever set for a login shell that is standing in for the interactive
    /// one we could not start. `shell = "login"`, asked for by name, means the
    /// login rc and nothing else, and gets none of this.
    pub source_rc: Option<String>,
}

impl Plan {
    /// Whether the prologue should walk into the worktree itself.
    ///
    /// Only for a shell that reads the user's rc: that rc is what installs the
    /// `chpwd` hooks worth firing, and it is also what might `cd` somewhere
    /// else on the way in. A plain `sh -c` has neither problem, and starting it
    /// anywhere but the working directory would be a change for its own sake.
    pub fn cds(&self) -> bool {
        self.reads_rc && self.dialect != Dialect::Cmd
    }

    /// How this reads in a log line: `zsh -l -i -c`.
    pub fn describe(&self) -> String {
        let mut out = Path::new(&self.program)
            .file_name()
            .unwrap_or(self.program.as_os_str())
            .to_string_lossy()
            .into_owned();
        for a in &self.args {
            out.push(' ');
            out.push_str(&a.to_string_lossy());
        }
        out
    }
}

/// The environment variable a `cd`-ing prologue reads the destination from.
///
/// Passing the path through the environment rather than the script text is what
/// keeps a directory containing a quote, a space or a `$` from turning into
/// shell syntax — in three dialects at once.
pub const STEP_DIR_VAR: &str = "GWT_STEP_DIR";

/// Resolve which shell to start.
///
/// `interactive_ok` says whether the command is getting the terminal. An
/// interactive shell without one is not just pointless — bash opens it by
/// printing "cannot set terminal process group" and "no job control in this
/// shell", straight into the middle of the output the step is there to produce.
/// So in a script, a hook or CI the login shell is what runs.
///
/// `$GWT_SYNC_SHELL` overrides [`Shell::Auto`] only: a step that names a shell
/// has a reason to, and a machine-wide preference must not silently hand a
/// POSIX script to fish.
pub fn plan(shell: &Shell, interactive_ok: bool) -> Plan {
    match shell {
        Shell::Auto | Shell::Login => match env_override() {
            Some(s) if *shell == Shell::Auto => plan(&s, interactive_ok),
            _ => user_shell(*shell == Shell::Auto, interactive_ok),
        },
        Shell::Posix => posix_plan(),
        // Spelled out by hand, so it is taken at its word — `-i` included.
        Shell::Named(spec) => named(spec),
    }
}

fn env_override() -> Option<Shell> {
    Shell::parse(&std::env::var("GWT_SYNC_SHELL").ok()?)
}

#[cfg(windows)]
fn posix_plan() -> Plan {
    Plan {
        program: std::env::var_os("ComSpec").unwrap_or_else(|| "cmd".into()),
        args: vec!["/C".into()],
        dialect: Dialect::Cmd,
        reads_rc: false,
        interactive: false,
        source_rc: None,
    }
}

#[cfg(not(windows))]
fn posix_plan() -> Plan {
    Plan {
        program: "sh".into(),
        args: vec!["-c".into()],
        dialect: Dialect::Posix,
        reads_rc: false,
        interactive: false,
        source_rc: None,
    }
}

/// `$SHELL`, started the way a terminal starts it.
#[cfg(windows)]
fn user_shell(_wants_interactive_rc: bool, _terminal: bool) -> Plan {
    // Windows has no login/interactive rc for the shell a recipe would use, so
    // there is nothing to reproduce: `cmd /C` *is* the faithful answer.
    posix_plan()
}

#[cfg(not(windows))]
fn user_shell(wants_interactive_rc: bool, terminal: bool) -> Plan {
    flags_for(
        &std::env::var_os("SHELL").unwrap_or_default(),
        wants_interactive_rc,
        terminal,
    )
}

/// The flags that start `raw` the way a terminal would.
///
/// `wants_interactive_rc` is the difference between [`Shell::Auto`], which is
/// after everything a terminal would set up, and [`Shell::Login`], which asked
/// for the login rc on purpose. `terminal` is whether there is one to be
/// interactive on.
#[cfg(not(windows))]
fn flags_for(raw: &OsStr, wants_interactive_rc: bool, terminal: bool) -> Plan {
    let name = Path::new(raw)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Only the shells whose rc files are worth reading, and that we know how to
    // ask for one. Anything else — /bin/sh, dash, a login shell set to
    // /usr/sbin/nologin — has no interactive setup to reproduce, so the plain
    // POSIX invocation is both faithful and safer.
    let (dialect, rc) = match name.as_str() {
        "bash" | "zsh" => (Dialect::Posix, true),
        "fish" => (Dialect::Fish, true),
        _ => return posix_plan(),
    };
    // Which rc files run is decided by exactly these two flags, and `-l` is not
    // simply "more": bash *skips* ~/.bashrc for a login shell, which is where
    // Linux puts every version manager's setup. So match what the platform's
    // terminals actually start — a login shell on macOS, a plain interactive
    // one elsewhere. With no terminal to be interactive on, a login shell is
    // what we fall back to — and `source_rc` below is what keeps that fallback
    // from losing the interactive rc a terminal would have read.
    let interactive = wants_interactive_rc && terminal;
    let mut args: Vec<OsString> = Vec::with_capacity(3);
    if !interactive || cfg!(target_os = "macos") {
        args.push("-l".into());
    }
    if interactive {
        args.push("-i".into());
    }
    args.push("-c".into());
    Plan {
        program: raw.to_os_string(),
        args,
        dialect,
        reads_rc: rc,
        interactive,
        // The login shell is standing in for an interactive one, so it has to
        // read what one would have read.
        source_rc: (wants_interactive_rc && !interactive)
            .then(|| interactive_rc(&name))
            .flatten(),
    }
}

/// Where the shell keeps the rc it reads only when interactive.
///
/// fish is absent on purpose: it reads `config.fish` whether or not it is
/// interactive, so there is nothing a login shell of its own would miss.
#[cfg(not(windows))]
fn interactive_rc(name: &str) -> Option<String> {
    match name {
        "zsh" => Some("${ZDOTDIR:-$HOME}/.zshrc".into()),
        "bash" => Some("$HOME/.bashrc".into()),
        _ => None,
    }
}

/// A shell spelled out by the recipe, split on whitespace: `"bash -euo
/// pipefail"`. Quoting is deliberately not supported — a shell lives at a path
/// without spaces, and a step that needs more than flags has a `cmd` to put it
/// in.
fn named(spec: &str) -> Plan {
    let mut words = spec.split_whitespace().map(OsString::from);
    let Some(program) = words.next() else {
        return posix_plan();
    };
    let mut args: Vec<OsString> = words.collect();
    let name = Path::new(&program)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dialect = if name.contains("fish") {
        Dialect::Fish
    } else if name.contains("cmd") || name.contains("powershell") || name.contains("pwsh") {
        Dialect::Cmd
    } else {
        Dialect::Posix
    };
    let has_c = args.iter().any(|a| {
        has_short_flag(a, 'c')
            || matches!(
                a.to_string_lossy().as_ref(),
                "/c" | "/C" | "-Command" | "-command"
            )
    });
    if !has_c {
        args.push(match dialect {
            Dialect::Cmd => "/C".into(),
            _ => "-c".into(),
        });
    }
    let interactive = args.iter().any(|a| has_short_flag(a, 'i'));
    let login = args.iter().any(|a| {
        has_short_flag(a, 'l')
            || a.to_string_lossy() == "--login"
            || a.to_string_lossy() == "-Login"
    });
    Plan {
        reads_rc: interactive || login,
        interactive,
        program,
        args,
        dialect,
        // Spelled out by hand, so it gets the rc its own flags ask for and not
        // one more.
        source_rc: None,
    }
}

/// Whether `arg` is a short flag carrying `want` — `-i`, and `-lic` too, since
/// nobody spells those out one at a time.
fn has_short_flag(arg: &OsStr, want: char) -> bool {
    let s = arg.to_string_lossy();
    let Some(rest) = s.strip_prefix('-') else {
        return false;
    };
    !rest.starts_with('-') && rest.contains(want)
}

/// The command, with whatever has to happen before it.
///
/// The prologue does two things a human does not have to think about: it `cd`s
/// into the worktree the way a person would (so `chpwd` hooks fire and direnv,
/// mise and the `.nvmrc` switchers get their chance), and it turns job control
/// back off, so the command shares our process group and a timeout can still
/// kill it.
pub fn script(plan: &Plan, cmd: &str) -> String {
    if !plan.cds() {
        // Nothing to fire and nowhere to walk from: the command is the script.
        return cmd.to_string();
    }
    match plan.dialect {
        // cmd.exe takes a single line and has no hooks to fire; the working
        // directory it is spawned with is the whole story.
        Dialect::Cmd => cmd.to_string(),
        Dialect::Posix => {
            let mut s = String::new();
            if plan.interactive {
                s.push_str("set +m 2>/dev/null\n");
            }
            // Before the `cd`, not after: the rc is what installs the `chpwd`
            // hooks the `cd` is there to fire. Its own output is startup noise
            // rather than anything the step was asked to produce, so it goes
            // nowhere, and a missing or unreadable rc is simply not our
            // business — `[ -r ]` keeps either from failing the step.
            if let Some(rc) = &plan.source_rc {
                s.push_str(&format!("[ -r \"{rc}\" ] && . \"{rc}\" >/dev/null 2>&1\n"));
            }
            // `\cd` skips an alias (`alias cd=z`) while still finding the
            // function zoxide and friends install under that name.
            s.push_str(&format!("\\cd -- \"${STEP_DIR_VAR}\" || exit 1\n"));
            s.push_str(cmd);
            s
        }
        Dialect::Fish => {
            // fish never splits a variable into words, so the bare expansion is
            // already safe — and `--` would be read as a directory name.
            format!("cd \"${STEP_DIR_VAR}\"; or exit 1\n{cmd}")
        }
    }
}

/// Where the shell should start, so that the prologue's `cd` is a real one.
///
/// A `cd` to the directory you are already in changes nothing, and a `chpwd`
/// hook that never fires is the entire bug we are fixing — so start outside and
/// walk in. `fallback` is used when the prologue cannot `cd` for us.
pub fn start_dir(plan: &Plan, root: &Path, fallback: &Path) -> PathBuf {
    if plan.cds() && root.is_dir() {
        root.to_path_buf()
    } else {
        fallback.to_path_buf()
    }
}

/// Drop the environment a project-local toolchain leaves pinned to whichever
/// project the shell that launched gwt was standing in.
///
/// Only ever called for a shell that is about to read the user's rc files:
/// everything here is either set by an activation the new worktree knows
/// nothing about, or set by an rc that is about to run again and set it back.
/// Strip these without an rc to restore them and we would be breaking a working
/// setup rather than fixing a broken one.
pub fn unpin(cmd: &mut Command) {
    let mut drop_from_path: Vec<PathBuf> = Vec::new();
    for (var, sub) in [("VIRTUAL_ENV", "bin"), ("GEM_HOME", "bin")] {
        if let Some(dir) = std::env::var_os(var) {
            drop_from_path.push(Path::new(&dir).join(sub));
            // Windows venvs put the executables here instead.
            drop_from_path.push(Path::new(&dir).join("Scripts"));
        }
    }
    for var in PINNED {
        cmd.env_remove(var);
    }
    // Bundler exports the pre-`bundle exec` values under this prefix, and npm
    // exports a whole project's package.json under its own; both name a project
    // that is not the one we are setting up.
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("BUNDLER_ORIG_") || name.starts_with("npm_") {
            cmd.env_remove(&key);
        }
    }
    if !drop_from_path.is_empty() {
        if let Some(path) = std::env::var_os("PATH") {
            cmd.env("PATH", without(&path, &drop_from_path));
        }
    }
}

/// Every variable [`unpin`] removes, listed rather than pattern-matched so that
/// what gwt takes away from a command is something you can read.
const PINNED: &[&str] = &[
    // Ruby: `bundle exec` and `rbenv shell` both pin a version and a Gemfile,
    // and both win over the `.ruby-version` sitting in the new worktree.
    "BUNDLE_GEMFILE",
    "BUNDLE_BIN_PATH",
    "BUNDLE_APP_CONFIG",
    "BUNDLER_VERSION",
    "BUNDLER_SETUP",
    "RUBYOPT",
    "RUBYLIB",
    "GEM_HOME",
    "GEM_PATH",
    "RBENV_VERSION",
    "RBENV_DIR",
    // Python: an activated virtualenv, and pyenv's equivalent of the above.
    "VIRTUAL_ENV",
    "VIRTUAL_ENV_PROMPT",
    "PYENV_VERSION",
    "PYENV_DIR",
    "PYTHONHOME",
    // Node: nodenv's version pin. (nvm is a shell function, so running the rc
    // is what fixes it — there is nothing to remove.)
    "NODENV_VERSION",
    "NODENV_DIR",
];

/// `path` with every entry in `drop` removed.
fn without(path: &OsStr, drop: &[PathBuf]) -> OsString {
    let kept: Vec<PathBuf> = std::env::split_paths(path)
        .filter(|p| !drop.iter().any(|d| d == p))
        .collect();
    std::env::join_paths(kept).unwrap_or_else(|_| path.to_os_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_named_shell_gets_a_c_flag_unless_it_has_one() {
        let p = named("bash -euo pipefail");
        assert_eq!(p.program, OsString::from("bash"));
        assert_eq!(
            p.args,
            vec![
                OsString::from("-euo"),
                OsString::from("pipefail"),
                OsString::from("-c")
            ]
        );
        assert!(!p.reads_rc, "no -l and no -i means no rc was asked for");

        let p = named("bash -lc");
        assert_eq!(p.args, vec![OsString::from("-lc")], "-lc is not two flags");
        assert!(p.reads_rc, "a cluster still asks for the login rc");
        assert!(!p.interactive);

        let p = named("zsh -l -i -c");
        assert_eq!(p.args.len(), 3, "the -c already there must not be doubled");
        assert!(p.reads_rc && p.interactive);
    }

    #[test]
    fn a_fish_shell_is_recognised_wherever_it_lives() {
        assert_eq!(named("/opt/homebrew/bin/fish").dialect, Dialect::Fish);
        assert_eq!(named("bash").dialect, Dialect::Posix);
    }

    #[test]
    fn the_prologue_walks_into_the_worktree_in_each_dialect() {
        let posix = Plan {
            program: "zsh".into(),
            args: vec!["-l".into(), "-i".into(), "-c".into()],
            dialect: Dialect::Posix,
            reads_rc: true,
            interactive: true,
            source_rc: None,
        };
        let s = script(&posix, "bundle install");
        assert!(s.starts_with("set +m"), "{s:?}");
        assert!(s.contains("\\cd -- \"$GWT_STEP_DIR\" || exit 1"), "{s:?}");
        assert!(s.ends_with("bundle install"), "{s:?}");

        let fish = Plan {
            dialect: Dialect::Fish,
            interactive: false,
            ..posix.clone()
        };
        let s = script(&fish, "bundle install");
        assert!(s.starts_with("cd \"$GWT_STEP_DIR\"; or exit 1"), "{s:?}");
        assert!(!s.contains("set +m"), "job control is not a fish idea");

        // cmd.exe gets the command and nothing else.
        let cmd = Plan {
            dialect: Dialect::Cmd,
            ..posix
        };
        assert_eq!(script(&cmd, "npm ci"), "npm ci");
    }

    #[test]
    fn posix_is_the_old_behaviour_exactly() {
        let p = plan(&Shell::Posix, true);
        assert!(!p.reads_rc, "sh -c reads no rc, so nothing may be stripped");
        assert_eq!(script(&p, "npm ci"), "npm ci", "and it starts where it is");
    }

    #[test]
    fn a_shell_name_round_trips_through_the_recipe() {
        for s in [Shell::Auto, Shell::Login, Shell::Posix] {
            assert_eq!(Shell::parse(s.as_str()), Some(s.clone()));
        }
        assert_eq!(
            Shell::parse("bash -l"),
            Some(Shell::Named("bash -l".into()))
        );
        assert_eq!(Shell::parse("  "), None);
    }

    /// `-l` is not "more rc": bash skips ~/.bashrc for a login shell, and that
    /// is exactly where Linux puts rbenv, nvm and asdf. So the flags follow the
    /// platform, and an interactive shell is only asked for when there is a
    /// terminal for it to be interactive on.
    #[test]
    #[cfg(not(windows))]
    fn the_flags_follow_the_platform_and_the_terminal() {
        let flags = |sh: &str, terminal: bool| {
            flags_for(OsStr::new(sh), true, terminal)
                .args
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        let want = if cfg!(target_os = "macos") {
            vec!["-l", "-i", "-c"]
        } else {
            vec!["-i", "-c"]
        };
        assert_eq!(flags("/bin/zsh", true), want);
        // No terminal — a script, a hook, CI — so the login shell it is: still
        // an rc, and none of bash's job-control complaints.
        assert_eq!(flags("/bin/zsh", false), vec!["-l", "-c"]);
    }

    /// The whole point of the login-shell fallback is the setup a terminal
    /// would have run, and `-l` alone does not get it: zsh reads `~/.zshrc`
    /// only when interactive, and bash skips `~/.bashrc` for a login shell. So
    /// the prologue sources it — before the `cd`, so the `chpwd` hooks it
    /// installs are there when the `cd` fires.
    #[test]
    #[cfg(not(windows))]
    fn a_login_shell_standing_in_for_a_terminal_reads_the_interactive_rc() {
        let p = flags_for(OsStr::new("/bin/zsh"), true, false);
        assert_eq!(p.source_rc.as_deref(), Some("${ZDOTDIR:-$HOME}/.zshrc"));
        let s = script(&p, "bundle install");
        let sourced = s
            .find("[ -r \"${ZDOTDIR:-$HOME}/.zshrc\" ] && . \"${ZDOTDIR:-$HOME}/.zshrc\"")
            .unwrap_or_else(|| panic!("{s:?}"));
        assert!(
            sourced < s.find("\\cd --").unwrap(),
            "the rc comes first: {s:?}"
        );
        assert!(s.ends_with("bundle install"), "{s:?}");

        assert_eq!(
            flags_for(OsStr::new("/bin/bash"), true, false)
                .source_rc
                .as_deref(),
            Some("$HOME/.bashrc")
        );
        // An interactive shell reads it on its own, and sourcing it twice is
        // not what a new terminal does.
        assert_eq!(
            flags_for(OsStr::new("/bin/zsh"), true, true).source_rc,
            None
        );
        // fish reads config.fish either way, so there is nothing to make up for.
        assert_eq!(
            flags_for(OsStr::new("/usr/local/bin/fish"), true, false).source_rc,
            None
        );
    }

    /// `shell = "login"` is a choice — an interactive rc too slow or too chatty
    /// to run in front of every command — so it must not be handed one anyway.
    #[test]
    #[cfg(not(windows))]
    fn an_explicit_login_shell_is_the_login_rc_and_nothing_more() {
        for terminal in [true, false] {
            let p = flags_for(OsStr::new("/bin/zsh"), false, terminal);
            assert_eq!(p.args, vec![OsString::from("-l"), OsString::from("-c")]);
            assert!(!p.interactive);
            assert_eq!(p.source_rc, None, "asked for login, got login");
            assert!(!script(&p, "npm ci").contains(".zshrc"));
        }
    }

    /// A `$SHELL` with no interactive setup to reproduce is not worth the
    /// risk of starting: `sh -c` is what it would have amounted to anyway.
    #[test]
    #[cfg(not(windows))]
    fn an_unknown_login_shell_falls_back_to_plain_sh() {
        for sh in ["/usr/sbin/nologin", "/bin/dash", ""] {
            let p = flags_for(OsStr::new(sh), true, true);
            assert_eq!(p.program, OsString::from("sh"), "{sh}");
            assert!(!p.reads_rc && !p.interactive, "{sh}");
        }
    }

    #[test]
    fn a_venv_leaves_the_path_with_it() {
        let path = std::env::join_paths(["/usr/bin", "/home/me/app/.venv/bin", "/bin"]).unwrap();
        let out = without(&path, &[PathBuf::from("/home/me/app/.venv/bin")]);
        let kept: Vec<PathBuf> = std::env::split_paths(&out).collect();
        assert_eq!(
            kept,
            vec![PathBuf::from("/usr/bin"), PathBuf::from("/bin")],
            "the activated venv must not follow us into another project"
        );
    }
}
