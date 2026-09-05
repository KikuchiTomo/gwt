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
//!    `-i` is what carries that, and it needs no terminal to do it: it is what
//!    puts `i` in `$-` and makes `[[ -o interactive ]]` true, which is the
//!    question the rc files actually ask. A terminal would be the fuller
//!    answer, and it is the wrong one — see [`crate::stream`], where a shell
//!    that can be *asked* something turns a recipe into a wait for an answer
//!    nobody is there to give.
//!
//!    What is left is the shell that will not read the interactive rc even so:
//!    bash skips `~/.bashrc` for a login shell however interactive it is, and a
//!    login shell is the normal way macOS starts a terminal, so this is not a
//!    corner case there but the usual one. `PATH` then ends up without the
//!    shims, and worse than untouched, because `/etc/zprofile` runs
//!    `path_helper` and rebuilds `PATH` with the system directories first — so
//!    the shims we inherited move *behind* `/usr/bin`, `bundle` becomes
//!    `/usr/bin/bundle` on the system Ruby 2.6, and it cannot find the bundler
//!    its `Gemfile.lock` asks for. So an rc the shell will not reach, the
//!    prologue sources for it — see [`Plan::source_rc`].
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
    /// rc file that sets up a version manager runs — interactive whether or not
    /// anyone is watching, since that is the question the rc files ask.
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
    /// Two shells need it, for different reasons. One is a login shell standing
    /// in for an interactive one we could not start. The other is bash started
    /// as a login shell — which is what macOS terminals do — because bash reads
    /// `~/.bash_profile` for those and never `~/.bashrc`, however interactive
    /// it is.
    ///
    /// `shell = "login"`, asked for by name, means the login rc and nothing
    /// else, and gets none of this.
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

/// Two markers written around the command, so the step's output is the
/// command's and nothing else.
///
/// A step's two streams are read as one (see [`crate::stream`]), and everything
/// the shell says on its way in and out of the session lands in the middle of
/// it: a `~/.zshrc` that echoes which architecture it just detected, an MOTD, a
/// version manager announcing itself, the complaints an interactive shell makes
/// about the terminal it could not find, and — because a login shell runs
/// `~/.zlogout` on the way out — prezto's parting fortune, printed *after* the
/// command has finished. None of that is the step's output, and a log that
/// opens with "So long and thanks for all the fish" is a log nobody reads
/// twice.
///
/// So the script says where the command starts and where it ends, and the
/// reader keeps what is between. The nonce is what keeps a command that happens
/// to print the marker from being able to cut its own output short.
///
/// If the begin marker never arrives the fence is abandoned and everything is
/// reported: a shell that died in its rc file has only the preamble to explain
/// itself, and swallowing that would turn a bad rc into a silent, empty
/// failure.
#[derive(Debug, Clone)]
pub struct Fence {
    pub begin: String,
    pub end: String,
}

impl Fence {
    pub fn new() -> Fence {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let nonce = format!("{:08x}{:08x}{:04x}", std::process::id(), nanos, n & 0xffff);
        // \x1e is RS, a control character no build tool prints and no terminal
        // acts on, so the marker cannot be mistaken for output or for an escape
        // sequence the reader is about to strip.
        Fence {
            begin: format!("\x1egwt-b{nonce}\x1e"),
            end: format!("\x1egwt-e{nonce}\x1e"),
        }
    }
}

impl Default for Fence {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolve which shell to start.
///
/// [`Shell::Auto`] is after everything a terminal would have set up, so it asks
/// for the interactive rc; [`Shell::Login`] asked for the login rc on purpose
/// and gets that and no more.
///
/// `$GWT_SYNC_SHELL` overrides [`Shell::Auto`] only: a step that names a shell
/// has a reason to, and a machine-wide preference must not silently hand a
/// POSIX script to fish.
pub fn plan(shell: &Shell) -> Plan {
    match shell {
        Shell::Auto | Shell::Login => match env_override() {
            Some(s) if *shell == Shell::Auto => plan(&s),
            _ => user_shell(*shell == Shell::Auto),
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
fn user_shell(_wants_interactive_rc: bool) -> Plan {
    // Windows has no login/interactive rc for the shell a recipe would use, so
    // there is nothing to reproduce: `cmd /C` *is* the faithful answer.
    posix_plan()
}

#[cfg(not(windows))]
fn user_shell(wants_interactive_rc: bool) -> Plan {
    flags_for(
        &std::env::var_os("SHELL").unwrap_or_default(),
        wants_interactive_rc,
    )
}

/// A shell we know how to start the way a terminal would.
#[cfg(not(windows))]
struct Known {
    /// The file name `$SHELL` ends in.
    name: &'static str,
    dialect: Dialect,
    /// The rc it reads *only* when interactive, written as a shell word for
    /// the prologue to expand — whoever will not reach it has to source it
    /// itself. `None` for a shell that reads its config either way and so
    /// misses nothing.
    interactive_rc: Option<&'static str>,
    /// Whether a shell that is interactive *and* a login shell still reads
    /// [`Known::interactive_rc`].
    ///
    /// The one place `-l` subtracts instead of adding. zsh reads `~/.zshrc`
    /// whenever it is interactive, login or not. bash does not: `~/.bashrc` is
    /// for the interactive shells that are *not* login shells, which is the
    /// entire reason it exists next to `~/.bash_profile` — and macOS terminals
    /// start a login shell, so this is the common case there, not the corner.
    login_reads_interactive_rc: bool,
}

/// Every shell whose rc files are worth reading, and everything that differs
/// between them, in one place: which dialect its prologue is written in and
/// which rc a login shell of its own would skip are the same piece of
/// knowledge, and splitting them across two `match`es is how they drift.
#[cfg(not(windows))]
const KNOWN: &[Known] = &[
    Known {
        name: "zsh",
        dialect: Dialect::Posix,
        // `~/.zshenv` is allowed to move it, and it runs before we could look.
        interactive_rc: Some("${ZDOTDIR:-$HOME}/.zshrc"),
        login_reads_interactive_rc: true,
    },
    Known {
        name: "bash",
        dialect: Dialect::Posix,
        interactive_rc: Some("$HOME/.bashrc"),
        login_reads_interactive_rc: false,
    },
    Known {
        name: "fish",
        dialect: Dialect::Fish,
        // fish reads config.fish whether or not it is interactive.
        interactive_rc: None,
        login_reads_interactive_rc: true,
    },
];

#[cfg(not(windows))]
impl Known {
    fn named(name: &str) -> Option<&'static Known> {
        KNOWN.iter().find(|k| k.name == name)
    }
}

/// The flags that start `raw` the way a terminal would.
///
/// `wants_interactive_rc` is the difference between [`Shell::Auto`], which is
/// after everything a terminal would set up, and [`Shell::Login`], which asked
/// for the login rc on purpose. `terminal` is whether there is one to be
/// interactive on.
#[cfg(not(windows))]
fn flags_for(raw: &OsStr, wants_interactive_rc: bool) -> Plan {
    let name = Path::new(raw)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Anything we do not know — /bin/sh, dash, a login shell set to
    // /usr/sbin/nologin — has no interactive setup to reproduce, so the plain
    // POSIX invocation is both faithful and safer.
    let Some(known) = Known::named(&name) else {
        return posix_plan();
    };
    // Which rc files run is decided by exactly these two flags, and `-l` is not
    // simply "more": bash *skips* ~/.bashrc for a login shell, which is where
    // Linux puts every version manager's setup. So match what the platform's
    // terminals actually start — a login shell on macOS, a plain interactive
    // one elsewhere. With no terminal to be interactive on, a login shell is
    // what we fall back to.
    let interactive = wants_interactive_rc;
    let login = !interactive || cfg!(target_os = "macos");
    let mut args: Vec<OsString> = Vec::with_capacity(3);
    if login {
        args.push("-l".into());
    }
    if interactive {
        args.push("-i".into());
    }
    args.push("-c".into());
    // Whether the shell we are about to start will reach the interactive rc by
    // itself. Two ways it does not, and they are different failures with the
    // same cure: there was no terminal to be interactive on, or it is bash
    // started as a login shell — which is what macOS terminals do, and bash
    // reads `~/.bash_profile` rather than `~/.bashrc` for those.
    let reads_own_rc = interactive && (!login || known.login_reads_interactive_rc);
    Plan {
        program: raw.to_os_string(),
        args,
        dialect: known.dialect,
        reads_rc: true,
        interactive,
        // Whatever the shell will not read on its own, the prologue reads for
        // it — so the toolchain is there either way.
        source_rc: (wants_interactive_rc && !reads_own_rc)
            .then_some(known.interactive_rc)
            .flatten()
            .map(str::to_string),
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
pub fn script(plan: &Plan, cmd: &str, fence: Option<&Fence>) -> String {
    // cmd.exe takes a single line and has no hooks to fire; the working
    // directory it is spawned with is the whole story.
    if !plan.cds() || plan.dialect == Dialect::Cmd {
        // Nothing to fire and nowhere to walk from: the command is the script,
        // and a fence would be the only thing we had added to it.
        return match fence {
            Some(f) if plan.dialect != Dialect::Cmd => fenced(plan.dialect, cmd, f, ""),
            _ => cmd.to_string(),
        };
    }
    let mut s = String::new();
    match plan.dialect {
        Dialect::Posix => {
            // Job control would put the command in a process group of its
            // own, where a killed shell leaves it running. Off, everything the
            // step started shares one group and the timeout can end all of it.
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
        }
        // fish never splits a variable into words, so the bare expansion is
        // already safe — and `--` would be read as a directory name.
        Dialect::Fish => s.push_str(&format!("cd \"${STEP_DIR_VAR}\"; or exit 1\n")),
        Dialect::Cmd => unreachable!("handled above"),
    }
    match fence {
        Some(f) => fenced(plan.dialect, cmd, f, &s),
        None => {
            s.push_str(cmd);
            s
        }
    }
}

/// `cmd` with a marker on either side, and its exit status carried across the
/// closing one.
///
/// The status is the reason this is not simply three `printf`s: the shell's
/// exit status is the step's, and it has to survive a marker being printed
/// after the command that set it.
fn fenced(dialect: Dialect, cmd: &str, f: &Fence, prologue: &str) -> String {
    let (begin, end) = (&f.begin, &f.end);
    // A command need not end in a newline, and without one the line that
    // follows it would be read as part of the last one — a trailing `#`
    // comment would swallow the marker whole.
    let nl = if cmd.ends_with('\n') { "" } else { "\n" };
    match dialect {
        Dialect::Fish => format!(
            "{prologue}printf '%s' '{begin}'\n{cmd}{nl}\
             set __gwt_status $status\nprintf '%s' '{end}'\nexit $__gwt_status\n"
        ),
        _ => format!(
            "{prologue}printf '%s' '{begin}'\n{cmd}{nl}\
             __gwt_status=$?\nprintf '%s' '{end}'\nexit $__gwt_status\n"
        ),
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
        let s = script(&posix, "bundle install", None);
        assert!(s.starts_with("set +m"), "{s:?}");
        assert!(s.contains("\\cd -- \"$GWT_STEP_DIR\" || exit 1"), "{s:?}");
        assert!(s.ends_with("bundle install"), "{s:?}");

        let fish = Plan {
            dialect: Dialect::Fish,
            interactive: false,
            ..posix.clone()
        };
        let s = script(&fish, "bundle install", None);
        assert!(s.starts_with("cd \"$GWT_STEP_DIR\"; or exit 1"), "{s:?}");
        assert!(!s.contains("set +m"), "job control is not a fish idea");

        // cmd.exe gets the command and nothing else.
        let cmd = Plan {
            dialect: Dialect::Cmd,
            ..posix
        };
        assert_eq!(script(&cmd, "npm ci", None), "npm ci");
    }

    #[test]
    fn posix_is_the_old_behaviour_exactly() {
        let p = plan(&Shell::Posix);
        assert!(!p.reads_rc, "sh -c reads no rc, so nothing may be stripped");
        assert_eq!(
            script(&p, "npm ci", None),
            "npm ci",
            "and it starts where it is"
        );
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
    /// is exactly where Linux puts rbenv, nvm and asdf. So the flags follow
    /// what the platform's own terminals start, and nothing else — there is no
    /// quieter variant to fall back to, because `-i` needs no terminal to make
    /// a shell interactive.
    #[test]
    #[cfg(not(windows))]
    fn the_flags_follow_the_platform() {
        let flags = |sh: &str| {
            flags_for(OsStr::new(sh), true)
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
        assert_eq!(flags("/bin/zsh"), want);
        assert_eq!(flags("/bin/bash"), want);
    }

    /// Whatever the shell will not read for itself, the prologue reads for it —
    /// and it goes before the `cd`, so the `chpwd` hooks the rc installs are in
    /// place when the `cd` fires.
    #[test]
    #[cfg(not(windows))]
    fn an_rc_the_shell_will_not_reach_is_sourced_by_the_prologue() {
        // bash as a login shell is the case that needs it, so ask for one.
        let p = flags_for(OsStr::new("/bin/bash"), false);
        let mut p = Plan {
            source_rc: Some("$HOME/.bashrc".into()),
            ..p
        };
        p.interactive = true;
        let s = script(&p, "bundle install", None);
        let sourced = s
            .find("[ -r \"$HOME/.bashrc\" ] && . \"$HOME/.bashrc\"")
            .unwrap_or_else(|| panic!("{s:?}"));
        assert!(
            sourced < s.find("\\cd --").unwrap(),
            "the rc comes first: {s:?}"
        );
        assert!(s.ends_with("bundle install"), "{s:?}");

        // An interactive zsh reads ~/.zshrc on its own, login or not, and
        // sourcing it twice is not what a new terminal does.
        assert_eq!(flags_for(OsStr::new("/bin/zsh"), true).source_rc, None);
        // fish reads config.fish either way, so there is nothing to make up for.
        assert_eq!(
            flags_for(OsStr::new("/usr/local/bin/fish"), true).source_rc,
            None
        );
    }

    /// The one place `-l` takes something away, and the reason a terminal is
    /// not on its own enough: `~/.bashrc` is for interactive shells that are
    /// *not* login shells. macOS terminals start a login shell, so a bash user
    /// there gets an interactive shell that still never reads the file every
    /// version manager writes its setup into — unless the prologue reads it.
    #[test]
    #[cfg(not(windows))]
    fn an_interactive_bash_login_shell_still_has_to_be_handed_bashrc() {
        let p = flags_for(OsStr::new("/bin/bash"), true);
        let is_login = p.args.iter().any(|a| a == "-l");
        assert_eq!(is_login, cfg!(target_os = "macos"));
        assert!(p.interactive, "there is a terminal, so it is interactive");
        assert_eq!(
            p.source_rc.as_deref(),
            is_login.then_some("$HOME/.bashrc"),
            "a login bash needs ~/.bashrc handed to it; a plain interactive one reads it"
        );

        // zsh is the contrast that makes the rule visible: it reads ~/.zshrc
        // whenever it is interactive, `-l` or not.
        assert_eq!(flags_for(OsStr::new("/bin/zsh"), true).source_rc, None);
    }

    /// `shell = "login"` is a choice — an interactive rc too slow or too chatty
    /// to run in front of every command — so it must not be handed one anyway.
    #[test]
    #[cfg(not(windows))]
    fn an_explicit_login_shell_is_the_login_rc_and_nothing_more() {
        let p = flags_for(OsStr::new("/bin/zsh"), false);
        assert_eq!(p.args, vec![OsString::from("-l"), OsString::from("-c")]);
        assert!(!p.interactive);
        assert_eq!(p.source_rc, None, "asked for login, got login");
        assert!(!script(&p, "npm ci", None).contains(".zshrc"));
    }

    /// A `$SHELL` with no interactive setup to reproduce is not worth the
    /// risk of starting: `sh -c` is what it would have amounted to anyway.
    #[test]
    #[cfg(not(windows))]
    fn an_unknown_login_shell_falls_back_to_plain_sh() {
        for sh in ["/usr/sbin/nologin", "/bin/dash", ""] {
            let p = flags_for(OsStr::new(sh), true);
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
