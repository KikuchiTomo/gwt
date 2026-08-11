use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::git;

pub const BARE_DIR: &str = ".bare";
pub const GWT_DIR: &str = ".gwt";
pub const SYNC_FILE: &str = "sync.toml";
pub const SECRETS_DIR: &str = "secrets";
pub const MANIFEST_FILE: &str = "manifest";
pub const DEFAULT_WT_NAME: &str = "default";

#[derive(Debug, Clone)]
pub struct BareLayout {
    pub root: PathBuf,
    pub bare_dir: PathBuf,
    /// `<root>/.gwt` — gwt's own state, beside `.bare` and inside no worktree.
    pub gwt_dir: PathBuf,
    /// `<root>/.gwt/sync.toml` — the sync recipe.
    pub sync_config: PathBuf,
    /// Where the real files a recipe points at conventionally live.
    pub secrets_dir: PathBuf,
    /// `<root>/secrets/manifest` — the pre-0.7 manifest, still read if the
    /// TOML recipe does not exist yet.
    pub legacy_manifest: PathBuf,
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
        let gwt_dir = cwd.join(GWT_DIR);
        let sync_config = gwt_dir.join(SYNC_FILE);
        let secrets_dir = cwd.join(SECRETS_DIR);
        let legacy_manifest = secrets_dir.join(MANIFEST_FILE);
        Ok(Self {
            root: cwd.to_path_buf(),
            bare_dir,
            gwt_dir,
            sync_config,
            secrets_dir,
            legacy_manifest,
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
