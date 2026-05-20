use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::layout::{strip_slash, BareLayout};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretEntry {
    pub src: String,
    pub dst: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkOutcome {
    Linked,
    Skipped { reason: &'static str },
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
            // Original bash supported tab OR whitespace separator; honor both.
            let mut parts = trimmed.splitn(2, |c: char| c == '\t' || c.is_whitespace());
            let src = parts.next()?.trim();
            let dst = parts.next()?.trim();
            if src.is_empty() || dst.is_empty() {
                return None;
            }
            Some(SecretEntry {
                src: strip_slash(src).to_string(),
                dst: strip_slash(dst).to_string(),
            })
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

pub fn add_entry(layout: &BareLayout, src: &str, dst: &str) -> Result<bool> {
    let mut entries = read_manifest(layout)?;
    let src = strip_slash(src).to_string();
    let dst = strip_slash(dst).to_string();
    let mut updated = false;
    let mut inserted = false;
    for e in &mut entries {
        if e.src == src {
            e.dst = dst.clone();
            updated = true;
            break;
        }
    }
    if !updated {
        entries.push(SecretEntry { src, dst });
        inserted = true;
    }
    write_manifest(layout, &entries)?;
    Ok(inserted)
}

pub fn remove_entry(layout: &BareLayout, src: &str) -> Result<bool> {
    let mut entries = read_manifest(layout)?;
    let src = strip_slash(src);
    let before = entries.len();
    entries.retain(|e| e.src != src);
    if entries.len() == before {
        return Ok(false);
    }
    write_manifest(layout, &entries)?;
    Ok(true)
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
        let src_abs: PathBuf = layout.root.join(&e.src);
        let dst_abs: PathBuf = worktree_dir.join(&e.dst);
        if !src_abs.exists() {
            out.push((
                e,
                LinkOutcome::Skipped {
                    reason: "source missing",
                },
            ));
            continue;
        }
        if let Some(parent) = dst_abs.parent() {
            fs::create_dir_all(parent)?;
        }
        // Replace any existing file/symlink at the destination, mirroring `ln -sf`.
        if dst_abs.symlink_metadata().is_ok() {
            let _ = fs::remove_file(&dst_abs);
        }
        symlink(&src_abs, &dst_abs)?;
        out.push((e, LinkOutcome::Linked));
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
}
