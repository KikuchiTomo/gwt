//! Shell out to `git` rather than linking libgit2: keeps worktree semantics
//! identical to the user's git and avoids a C build dep on every platform.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;

use crate::error::{Error, Result};

/// Where `git` lives, looked up once.
///
/// This is called for every single git invocation, and the picker opens with a
/// good handful of them; each `which` is a walk of every directory on `PATH`,
/// which on a machine with a long PATH costs more than the git call it is about
/// to make.
fn git_bin() -> Result<&'static Path> {
    static BIN: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
    BIN.get_or_init(|| which::which("git").ok())
        .as_deref()
        .ok_or(Error::GitNotFound)
}

pub fn run<I, S>(cwd: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let out = command(cwd, args)?.output()?;
    check_output(out)
}

pub fn command<I, S>(cwd: &Path, args: I) -> Result<Command>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut cmd = Command::new(git_bin()?);
    cmd.current_dir(cwd)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped());
    Ok(cmd)
}

/// Run git, handing every line it writes to stderr to `on_line` as it arrives,
/// and return its stdout.
///
/// `run` waits for the whole process, which is why cloning a large repo looked
/// frozen: git's progress goes to stderr, in `\r`-separated updates, and none
/// of it was read until the clone had already finished.
pub fn stream<I, S>(cwd: &Path, args: I, on_line: &mut dyn FnMut(&str)) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    use std::io::Read;

    let mut child = command(cwd, args)?.spawn()?;
    let mut stderr = child.stderr.take().expect("stderr is piped");
    // stdout is drained on its own thread: a command that fills that pipe while
    // we are busy reading stderr would otherwise block forever.
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let collector = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });

    // git separates progress updates with `\r` and finishes a phase with `\n`,
    // so both are line endings here.
    let mut tail: Vec<String> = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = stderr.read(&mut buf)?;
        if n == 0 {
            break;
        }
        for &b in &buf[..n] {
            if b == b'\r' || b == b'\n' {
                emit(&mut cur, &mut tail, on_line);
            } else {
                cur.push(b);
            }
        }
    }
    emit(&mut cur, &mut tail, on_line);

    let status = child.wait()?;
    let out = collector.join().unwrap_or_default();
    if !status.success() {
        return Err(Error::GitCommand {
            code: status.code().unwrap_or(-1),
            stderr: tail.join("\n"),
        });
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

fn emit(cur: &mut Vec<u8>, tail: &mut Vec<String>, on_line: &mut dyn FnMut(&str)) {
    if cur.is_empty() {
        return;
    }
    let line = String::from_utf8_lossy(cur).into_owned();
    cur.clear();
    on_line(&line);
    // Keep only the end of the transcript: that is where git puts the reason it
    // failed, and a progress-filled error message helps nobody.
    tail.push(line);
    if tail.len() > 10 {
        tail.remove(0);
    }
}

/// One of git's `\r`-separated progress updates, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// "Receiving objects", "Resolving deltas", …
    pub phase: String,
    pub percent: u8,
    /// The part after the counts: "12.34 MiB | 5.00 MiB/s", or "done.".
    pub detail: String,
}

/// Recognise a progress line. Anything else — "Cloning into…", a warning, the
/// remote's own chatter — returns `None`.
///
/// ```text
/// remote: Compressing objects:  66% (2/3)
/// Receiving objects:  42% (42/100), 1.20 MiB | 2.00 MiB/s
/// ```
pub fn parse_progress(line: &str) -> Option<Progress> {
    let line = line.trim();
    let line = line.strip_prefix("remote: ").unwrap_or(line);
    let (phase, rest) = line.split_once(": ")?;
    let rest = rest.trim();
    let percent: u8 = rest.split('%').next()?.trim().parse().ok()?;
    let detail = rest
        .split_once(", ")
        .map(|(_, d)| d.trim().to_string())
        .unwrap_or_default();
    Some(Progress {
        phase: phase.trim().to_string(),
        percent: percent.min(100),
        detail,
    })
}

/// `(major, minor)` of the git we are shelling out to, if it can be read.
pub fn version(cwd: &Path) -> Option<(u32, u32)> {
    let raw = run(cwd, ["--version"]).ok()?;
    // "git version 2.43.0" — and on macOS "git version 2.39.5 (Apple Git-154)".
    let nums = raw
        .split_whitespace()
        .find(|w| w.starts_with(|c: char| c.is_ascii_digit()))?;
    let mut parts = nums.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

/// git 2.48 added `worktree.useRelativePaths`, and with it the ability to read a
/// relative path out of `.bare/worktrees/<id>/gitdir`. Older git treats that
/// string as the worktree's own path, which breaks every consumer of
/// `git worktree list` and marks the worktree prunable.
pub fn understands_relative_gitdirs(cwd: &Path) -> bool {
    matches!(version(cwd), Some((major, minor)) if (major, minor) >= (2, 48))
}

fn check_output(out: Output) -> Result<String> {
    if !out.status.success() {
        return Err(Error::GitCommand {
            code: out.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_shapes_git_actually_prints() {
        assert_eq!(
            parse_progress("Receiving objects:  42% (42/100), 1.20 MiB | 2.00 MiB/s"),
            Some(Progress {
                phase: "Receiving objects".into(),
                percent: 42,
                detail: "1.20 MiB | 2.00 MiB/s".into(),
            })
        );
        assert_eq!(
            parse_progress("remote: Compressing objects: 100% (3/3), done."),
            Some(Progress {
                phase: "Compressing objects".into(),
                percent: 100,
                detail: "done.".into(),
            })
        );
        assert_eq!(
            parse_progress("Resolving deltas:   0% (0/10)"),
            Some(Progress {
                phase: "Resolving deltas".into(),
                percent: 0,
                detail: String::new(),
            })
        );
    }

    #[test]
    fn everything_else_is_not_progress() {
        // No percentage, so no bar to draw — but these still belong in the
        // transcript we quote back when git fails.
        for line in [
            "Cloning into bare repository '.bare'...",
            "remote: Enumerating objects: 128, done.",
            "warning: You appear to have cloned an empty repository.",
            "fatal: invalid reference: main",
            "",
        ] {
            assert_eq!(parse_progress(line), None, "{line:?}");
        }
    }
}
