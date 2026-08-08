//! Shell out to `git` rather than linking libgit2: keeps worktree semantics
//! identical to the user's git and avoids a C build dep on every platform.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use crate::error::{Error, Result};

fn git_bin() -> Result<std::path::PathBuf> {
    which::which("git").map_err(|_| Error::GitNotFound)
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
