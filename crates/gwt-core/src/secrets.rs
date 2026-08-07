// The secrets manifest maps ONE real file to ONE path inside EVERY worktree.
//
// The two columns use different bases, which is the only subtle part:
//
//   src  — relative to the REPO ROOT (`layout.root`, where .git / .bare / secrets/ live)
//   dst  — relative to EACH WORKTREE ROOT
//
//   <repo-root>/
//   ├── secrets/.env              <- src = "secrets/.env"
//   ├── default/.env   -> symlink to ../secrets/.env
//   └── feature-a/.env -> symlink to ../secrets/.env
//                 ^^^^            <- dst = ".env"
//
// Both columns are stored normalized (forward slashes, no `.`/`..`, no leading
// slash) so the manifest is stable regardless of how the user typed the path.

use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use crate::error::{Error, Result};
use crate::layout::{strip_slash, BareLayout};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretEntry {
    /// Path of the real file, relative to the repo root.
    pub src: String,
    /// Path the symlink takes inside each worktree, relative to the worktree root.
    pub dst: String,
}

impl SecretEntry {
    /// Absolute path of the real file this entry points at.
    pub fn src_abs(&self, layout: &BareLayout) -> PathBuf {
        layout.root.join(&self.src)
    }

    /// Absolute path of the symlink this entry creates inside `worktree_dir`.
    pub fn dst_abs(&self, worktree_dir: &Path) -> PathBuf {
        worktree_dir.join(&self.dst)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkOutcome {
    Linked,
    Skipped { reason: &'static str },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnlinkOutcome {
    /// The symlink we manage was removed.
    Removed,
    /// Nothing was there — already clean.
    Absent,
    /// Something else occupies the path; we refuse to delete it.
    Kept { reason: &'static str },
}

/// Normalize a user-supplied source path into a repo-root-relative manifest path.
///
/// Accepts an absolute path inside the root, or a path relative to the root
/// (which is also the cwd, since every `secret` subcommand runs from the root).
pub fn normalize_src(layout: &BareLayout, input: &str) -> Result<String> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err(Error::SecretSrcInvalid {
            path: input.to_string(),
            reason: "path is empty",
        });
    }
    let p = Path::new(raw);
    // An absolute source is fine as long as it names a file inside the root —
    // shell tab-completion produces these, so accept and fold them back.
    let rel: PathBuf = if p.is_absolute() {
        // Compare against the canonical root so /var vs /private/var (macOS) and
        // symlinked checkouts don't spuriously look "outside".
        let root = fs::canonicalize(&layout.root).unwrap_or_else(|_| layout.root.clone());
        let cand = canonicalize_lexically_existing(p);
        cand.strip_prefix(&root)
            .map(Path::to_path_buf)
            .map_err(|_| Error::SecretSrcInvalid {
                path: input.to_string(),
                reason: "absolute path is outside the repo root",
            })?
    } else {
        PathBuf::from(raw)
    };

    lexical_normalize(&rel).ok_or_else(|| Error::SecretSrcInvalid {
        path: input.to_string(),
        reason: "path escapes the repo root",
    })
}

/// Normalize a user-supplied destination into a worktree-relative manifest path.
///
/// The destination is always relative to a worktree root, so an absolute path is
/// a category error rather than something to reinterpret — reject it loudly.
pub fn normalize_dst(input: &str) -> Result<String> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err(Error::SecretDstInvalid {
            path: input.to_string(),
            reason: "path is empty",
        });
    }
    if Path::new(raw).is_absolute() {
        return Err(Error::SecretDstInvalid {
            path: input.to_string(),
            reason: "must be relative to the worktree root, not absolute",
        });
    }
    lexical_normalize(Path::new(raw)).ok_or_else(|| Error::SecretDstInvalid {
        path: input.to_string(),
        reason: "path escapes the worktree root",
    })
}

/// Collapse `.` / `..` textually and re-emit with `/` separators.
/// Returns `None` if the path escapes its base or resolves to nothing.
fn lexical_normalize(p: &Path) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

/// Canonicalize as much of `p` as exists, keeping the non-existent tail. Plain
/// `fs::canonicalize` fails outright when the file isn't created yet, but we want
/// to register mappings for files that will appear later.
fn canonicalize_lexically_existing(p: &Path) -> PathBuf {
    if let Ok(c) = fs::canonicalize(p) {
        return c;
    }
    match (p.parent(), p.file_name()) {
        (Some(parent), Some(name)) => canonicalize_lexically_existing(parent).join(name),
        _ => p.to_path_buf(),
    }
}

pub fn read_manifest(layout: &BareLayout) -> Result<Vec<SecretEntry>> {
    if !layout.manifest.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(&layout.manifest)?;
    Ok(parse_manifest(&raw))
}

pub fn parse_manifest(raw: &str) -> Vec<SecretEntry> {
    raw.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return None;
            }
            // We always write a tab, so prefer it — that keeps paths containing
            // spaces intact. Fall back to any whitespace for manifests written
            // by the original bash version (or edited by hand).
            let (src, dst) = match trimmed.split_once('\t') {
                Some(pair) => pair,
                None => trimmed.split_once(char::is_whitespace)?,
            };
            // Leading slashes are legacy noise from the bash version — both
            // columns were always relative, so drop them rather than reject.
            let src = lexical_normalize(Path::new(strip_slash(src.trim())))?;
            let dst = lexical_normalize(Path::new(strip_slash(dst.trim())))?;
            Some(SecretEntry { src, dst })
        })
        .collect()
}

pub fn write_manifest(layout: &BareLayout, entries: &[SecretEntry]) -> Result<()> {
    fs::create_dir_all(&layout.secrets_dir)?;
    let mut f = fs::File::create(&layout.manifest)?;
    for e in entries {
        writeln!(f, "{}\t{}", e.src, e.dst)?;
    }
    Ok(())
}

/// Insert or update the mapping for `src`. Returns the entry as stored plus the
/// previous entry it replaced, so the caller can unlink the stale destination.
pub fn add_entry(
    layout: &BareLayout,
    src: &str,
    dst: &str,
) -> Result<(SecretEntry, Option<SecretEntry>)> {
    let src = normalize_src(layout, src)?;
    let dst = normalize_dst(dst)?;
    let entry = SecretEntry { src, dst };

    let mut entries = read_manifest(layout)?;
    let mut previous = None;
    match entries.iter_mut().find(|e| e.src == entry.src) {
        Some(existing) => {
            previous = Some(existing.clone());
            existing.dst = entry.dst.clone();
        }
        None => entries.push(entry.clone()),
    }
    write_manifest(layout, &entries)?;
    Ok((entry, previous))
}

/// Drop the mapping for `src`. Returns the removed entry so the caller knows
/// which destination to unlink.
pub fn remove_entry(layout: &BareLayout, src: &str) -> Result<Option<SecretEntry>> {
    let src = normalize_src(layout, src)?;
    let mut entries = read_manifest(layout)?;
    let Some(pos) = entries.iter().position(|e| e.src == src) else {
        return Ok(None);
    };
    let removed = entries.remove(pos);
    write_manifest(layout, &entries)?;
    Ok(Some(removed))
}

/// Create the symlink for one entry inside `worktree_dir`.
pub fn apply_entry(
    layout: &BareLayout,
    worktree_dir: &Path,
    e: &SecretEntry,
) -> Result<LinkOutcome> {
    let src_abs = e.src_abs(layout);
    let dst_abs = e.dst_abs(worktree_dir);
    if !src_abs.exists() {
        return Ok(LinkOutcome::Skipped {
            reason: "source missing",
        });
    }
    if let Some(parent) = dst_abs.parent() {
        fs::create_dir_all(parent)?;
    }
    // Replace any existing file/symlink at the destination, mirroring `ln -sf`.
    if dst_abs.symlink_metadata().is_ok() {
        let _ = fs::remove_file(&dst_abs);
    }
    symlink(&src_abs, &dst_abs)?;
    Ok(LinkOutcome::Linked)
}

/// Remove the symlink for one entry from `worktree_dir`.
///
/// Only ever unlinks a symlink that still points at this entry's source — a real
/// file (or someone else's link) sitting at that path is left untouched, since
/// `secret rm` should never be able to destroy the user's own work.
pub fn unlink_entry(
    layout: &BareLayout,
    worktree_dir: &Path,
    e: &SecretEntry,
) -> Result<UnlinkOutcome> {
    let dst_abs = e.dst_abs(worktree_dir);
    let Ok(meta) = dst_abs.symlink_metadata() else {
        return Ok(UnlinkOutcome::Absent);
    };
    if !meta.file_type().is_symlink() {
        return Ok(UnlinkOutcome::Kept {
            reason: "not a symlink",
        });
    }
    let target = fs::read_link(&dst_abs)?;
    if target != e.src_abs(layout) {
        return Ok(UnlinkOutcome::Kept {
            reason: "symlink points elsewhere",
        });
    }
    fs::remove_file(&dst_abs)?;
    Ok(UnlinkOutcome::Removed)
}

/// Apply every manifest entry to `worktree_dir`, creating symlinks. Returns
/// (linked, skipped, results) where each result describes one entry.
pub fn apply_links(
    layout: &BareLayout,
    worktree_dir: &Path,
) -> Result<Vec<(SecretEntry, LinkOutcome)>> {
    let entries = read_manifest(layout)?;
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        let outcome = apply_entry(layout, worktree_dir, &e)?;
        out.push((e, outcome));
    }
    Ok(out)
}

#[cfg(unix)]
fn symlink(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

#[cfg(windows)]
fn symlink(src: &Path, dst: &Path) -> std::io::Result<()> {
    // Files are the dominant case for the secrets manifest; if the user pointed
    // at a directory, fall back to a directory symlink (needs Dev Mode / admin).
    if src.is_dir() {
        std::os::windows::fs::symlink_dir(src, dst)
    } else {
        std::os::windows::fs::symlink_file(src, dst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tabs_and_whitespace_and_comments() {
        let raw = "# comment\nfoo\tbar\n\nbaz   qux\n/leading\t/slash\n";
        let v = parse_manifest(raw);
        assert_eq!(v.len(), 3);
        assert_eq!(
            v[0],
            SecretEntry {
                src: "foo".into(),
                dst: "bar".into()
            }
        );
        assert_eq!(
            v[1],
            SecretEntry {
                src: "baz".into(),
                dst: "qux".into()
            }
        );
        assert_eq!(
            v[2],
            SecretEntry {
                src: "leading".into(),
                dst: "slash".into()
            }
        );
    }

    #[test]
    fn tab_separated_paths_may_contain_spaces() {
        let v = parse_manifest("secrets/my env\tconfig/my env\n");
        assert_eq!(
            v[0],
            SecretEntry {
                src: "secrets/my env".into(),
                dst: "config/my env".into()
            }
        );
    }

    #[test]
    fn normalizes_dot_segments() {
        assert_eq!(
            lexical_normalize(Path::new("./secrets/../secrets/.env")).unwrap(),
            "secrets/.env"
        );
    }

    #[test]
    fn rejects_escaping_and_absolute_destinations() {
        assert!(normalize_dst("../outside").is_err());
        assert!(normalize_dst("/etc/passwd").is_err());
        assert!(normalize_dst("").is_err());
        assert_eq!(normalize_dst("./config/.env").unwrap(), "config/.env");
    }
}
