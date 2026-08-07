use std::path::PathBuf;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("git executable not found in PATH")]
    GitNotFound,

    #[error("not inside a git repository (cwd: {0})")]
    NotARepo(PathBuf),

    #[error("not a bare-style worktree root ({reason}; cwd: {cwd})")]
    NotBareRoot { cwd: PathBuf, reason: &'static str },

    #[error("worktree path '{0}' already exists")]
    PathExists(PathBuf),

    #[error("branch '{0}' already exists")]
    BranchExists(String),

    #[error("'{0}' is not a valid base (branch, tag, or commit)")]
    InvalidBase(String),

    #[error("remote branch 'origin/{0}' not found")]
    RemoteBranchMissing(String),

    #[error("branch '{0}' has no upstream and there is no origin/{0} to track")]
    NoUpstream(String),

    #[error("git command failed ({code}): {stderr}")]
    GitCommand { code: i32, stderr: String },

    #[error("failed to parse git output: {0}")]
    Parse(String),

    #[error("worktree '{0}' not found")]
    WorktreeNotFound(String),

    #[error("invalid secret source '{path}': {reason} (it must name a file inside the repo root)")]
    SecretSrcInvalid { path: String, reason: &'static str },

    #[error(
        "invalid secret destination '{path}': {reason} (it is the path the link takes inside each worktree)"
    )]
    SecretDstInvalid { path: String, reason: &'static str },

    #[error(transparent)]
    Io(#[from] std::io::Error),
}
