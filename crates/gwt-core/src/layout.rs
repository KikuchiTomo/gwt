use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::git;

pub const BARE_DIR: &str = ".bare";
pub const SECRETS_DIR: &str = "secrets";
pub const MANIFEST_FILE: &str = "manifest";
pub const DEFAULT_WT_NAME: &str = "default";

#[derive(Debug, Clone)]
pub struct BareLayout {
    pub root: PathBuf,
    pub bare_dir: PathBuf,
    pub secrets_dir: PathBuf,
    pub manifest: PathBuf,
}

impl BareLayout {
    pub fn require(cwd: &Path) -> Result<Self> {
        let dot_git = cwd.join(".git");
        if !dot_git.is_file() {
            return Err(Error::NotBareRoot {
                cwd: cwd.to_path_buf(),
                reason: "no .git file in cwd",
            });
        }
        let contents = std::fs::read_to_string(&dot_git)?;
        if !contents.contains(BARE_DIR) {
            return Err(Error::NotBareRoot {
                cwd: cwd.to_path_buf(),
                reason: ".git does not point to .bare",
            });
        }
        let bare_dir = cwd.join(BARE_DIR);
        if !bare_dir.is_dir() {
            return Err(Error::NotBareRoot {
                cwd: cwd.to_path_buf(),
                reason: ".bare/ directory missing",
            });
        }
        let secrets_dir = cwd.join(SECRETS_DIR);
        let manifest = secrets_dir.join(MANIFEST_FILE);
        Ok(Self {
            root: cwd.to_path_buf(),
            bare_dir,
            secrets_dir,
            manifest,
        })
    }

    /// HEAD branch name of the bare repo (origin's default branch).
    pub fn default_branch(&self) -> Result<String> {
        let raw = git::run(&self.root, ["--git-dir", BARE_DIR, "symbolic-ref", "HEAD"])?;
        Ok(raw
            .trim()
            .strip_prefix("refs/heads/")
            .unwrap_or(raw.trim())
            .to_string())
    }
}

pub fn strip_slash(p: &str) -> &str {
    p.strip_prefix('/').unwrap_or(p)
}
