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

fn check_output(out: Output) -> Result<String> {
    if !out.status.success() {
        return Err(Error::GitCommand {
            code: out.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
