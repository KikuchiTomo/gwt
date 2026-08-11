//! Build caches that survive the worktree they were built in.
//!
//! A worktree is a fresh directory, so every build system starts from nothing:
//! six worktrees of one repo means six `target/` directories and six cold
//! builds. Moving the cache out of the worktree fixes that, but "share it"
//! is the wrong default — two branches with different lockfiles must not write
//! to the same `node_modules`.
//!
//! So the cache lives outside every worktree, in buckets, and which bucket a
//! worktree binds to is decided by the *contents* of the files that would make
//! sharing unsafe:
//!
//! ```text
//! <repo-root>/.gwt/cache/target/a3f19c02b7e4/     <- bucket, keyed on Cargo.lock
//! <repo-root>/feature-a/target -> ../.gwt/cache/target/a3f19c02b7e4
//! <repo-root>/feature-b/target -> ../.gwt/cache/target/a3f19c02b7e4   same lock
//! <repo-root>/bump-deps/target -> ../.gwt/cache/target/71dd4e0af8c1   changed it
//! ```
//!
//! Nobody has to declare "these two branches are compatible". Change the
//! lockfile and the worktree moves to its own bucket by itself; change it back
//! and it returns to the shared one, still warm.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::error::Result;
use crate::layout::BareLayout;

pub const CACHE_DIR: &str = "cache";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheMode {
    /// One bucket per distinct value of `key`. The safe default.
    Keyed,
    /// One bucket for the whole repo. For caches that cannot be poisoned —
    /// download caches, content-addressed stores.
    Shared,
    /// One bucket per worktree. Shares nothing, but the cache outlives the
    /// worktree, so deleting and re-creating one keeps it warm.
    Private,
}

impl CacheMode {
    pub fn as_str(self) -> &'static str {
        match self {
            CacheMode::Keyed => "keyed",
            CacheMode::Shared => "shared",
            CacheMode::Private => "private",
        }
    }

    pub fn parse(s: &str) -> Option<CacheMode> {
        match s.trim() {
            "keyed" => Some(CacheMode::Keyed),
            "shared" => Some(CacheMode::Shared),
            "private" => Some(CacheMode::Private),
            _ => None,
        }
    }

    pub const ALL: [CacheMode; 3] = [CacheMode::Keyed, CacheMode::Shared, CacheMode::Private];
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheStep {
    /// Where the cache appears inside each worktree, relative to its root.
    pub path: String,
    pub mode: CacheMode,
    /// Worktree-relative files whose contents pick the bucket. `keyed` only.
    pub key: Vec<String>,
    /// Fill a brand-new bucket from the most recently used one, by
    /// copy-on-write where the filesystem supports it.
    pub seed: bool,
    /// Name of an environment variable that points a tool at the bucket, for
    /// `git wt cache env`. Some tools take the path directly and then never
    /// bake the worktree path into their own keys.
    pub env: Option<String>,
}

impl CacheStep {
    /// How the left-hand column of `sync ls` describes this cache.
    pub fn summary(&self) -> String {
        match self.mode {
            CacheMode::Keyed if !self.key.is_empty() => self.key.join(" "),
            CacheMode::Keyed => "(no key — one bucket)".into(),
            CacheMode::Shared => "(shared by every worktree)".into(),
            CacheMode::Private => "(one per worktree)".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindOutcome {
    /// The worktree now points at `bucket`.
    Bound {
        bucket: String,
        /// Content already in the worktree was moved into the bucket.
        adopted: bool,
        /// A new bucket was filled from an existing one.
        seeded: bool,
    },
    /// Already pointing at the right bucket.
    Unchanged { bucket: String },
    /// Something is in the way that we will not touch.
    Blocked { reason: &'static str },
}

// ---------------------------------------------------------------------------
// where things live
// ---------------------------------------------------------------------------

pub fn cache_root(layout: &BareLayout) -> PathBuf {
    layout.gwt_dir.join(CACHE_DIR)
}

/// One directory per cached path. `/` cannot appear in a directory name, and
/// nesting `.next/cache` under `.next/` would put a bucket named `cache` next
/// to real buckets — so the separator is escaped instead.
pub fn slot_name(path: &str) -> String {
    path.replace('+', "++").replace('/', "+")
}

pub fn slot_dir(layout: &BareLayout, step: &CacheStep) -> PathBuf {
    cache_root(layout).join(slot_name(&step.path))
}

/// Which bucket `worktree_dir` belongs in right now.
///
/// For `keyed`, this is a hash of the key files' contents, so it changes the
/// moment the user edits a lockfile — that is the whole mechanism.
pub fn bucket_id(step: &CacheStep, worktree_dir: &Path) -> String {
    match step.mode {
        CacheMode::Shared => "shared".to_string(),
        CacheMode::Private => sanitize(
            &worktree_dir
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "worktree".into()),
        ),
        CacheMode::Keyed => {
            let mut h = Fnv::new();
            h.write(step.path.as_bytes());
            for k in &step.key {
                h.write(b"\0");
                h.write(k.as_bytes());
                h.write(b"\0");
                match fs::read(worktree_dir.join(k)) {
                    Ok(bytes) => h.write(&bytes),
                    // A key file that does not exist is itself a distinguishing
                    // state: a worktree from before the lockfile was added must
                    // not share with one that has it.
                    Err(_) => h.write(b"<absent>"),
                }
            }
            h.hex()
        }
    }
}

pub fn bucket_dir(layout: &BareLayout, step: &CacheStep, worktree_dir: &Path) -> PathBuf {
    slot_dir(layout, step).join(bucket_id(step, worktree_dir))
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// FNV-1a. The bucket id only has to be stable and collision-shy across a
/// handful of lockfiles, which does not justify pulling in a hash crate.
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 ^= *b as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn hex(&self) -> String {
        format!("{:016x}", self.0)[..12].to_string()
    }
}

// ---------------------------------------------------------------------------
// binding
// ---------------------------------------------------------------------------

/// Point `<worktree>/<path>` at the bucket this worktree belongs in.
pub fn bind(layout: &BareLayout, worktree_dir: &Path, step: &CacheStep) -> Result<BindOutcome> {
    let bucket = bucket_id(step, worktree_dir);
    let bucket_abs = slot_dir(layout, step).join(&bucket);
    let mount = worktree_dir.join(&step.path);

    // Already correct? Then this is a no-op, which matters: binding runs on
    // every `sync apply` and every worktree creation.
    if let Ok(meta) = mount.symlink_metadata() {
        if meta.file_type().is_symlink() {
            if fs::read_link(&mount).is_ok_and(|t| t == bucket_abs) {
                fs::create_dir_all(&bucket_abs)?;
                return Ok(BindOutcome::Unchanged { bucket });
            }
            // Pointing at another bucket: the key changed. Re-point it; the old
            // bucket stays on disk, warm, in case the key changes back.
            fs::remove_file(&mount)?;
        }
    }

    let mut adopted = false;
    let mut seeded = false;
    let existing = mount.symlink_metadata().is_ok();
    if existing {
        if !mount.is_dir() {
            return Ok(BindOutcome::Blocked {
                reason: "a file is in the way",
            });
        }
        if bucket_abs.exists() && !is_empty_dir(&bucket_abs) {
            // Both sides hold real content. Merging them is exactly the mixing
            // this design exists to prevent, so stop and let the user choose.
            return Ok(BindOutcome::Blocked {
                reason: "both the worktree and the bucket already hold a cache",
            });
        }
        // Adopt: a 4 GB target/ that is already warm should not be thrown away
        // just because it is being brought under management.
        fs::create_dir_all(slot_dir(layout, step))?;
        let _ = fs::remove_dir(&bucket_abs);
        if fs::rename(&mount, &bucket_abs).is_err() {
            // A rename across filesystems fails outright, and a bind mount or a
            // container volume under the worktree makes that possible. Copying
            // is slower but keeps the data, which is the whole promise here.
            copy_tree(&mount, &bucket_abs)?;
            fs::remove_dir_all(&mount)?;
        }
        adopted = true;
    } else if !bucket_abs.exists() {
        fs::create_dir_all(slot_dir(layout, step))?;
        if step.seed {
            if let Some(donor) = newest_bucket(layout, step, &bucket) {
                seeded = copy_tree(&donor, &bucket_abs).is_ok();
            }
        }
        if !bucket_abs.exists() {
            fs::create_dir_all(&bucket_abs)?;
        }
    }

    if let Some(parent) = mount.parent() {
        fs::create_dir_all(parent)?;
    }
    symlink_dir(&bucket_abs, &mount)?;
    ignore_in_repo(layout, &step.path)?;
    Ok(BindOutcome::Bound {
        bucket,
        adopted,
        seeded,
    })
}

/// Remove the symlink, leaving the bucket where it is. The data is the
/// expensive part; `cache gc` is how it goes away.
pub fn unbind(layout: &BareLayout, worktree_dir: &Path, step: &CacheStep) -> Result<bool> {
    let mount = worktree_dir.join(&step.path);
    let Ok(meta) = mount.symlink_metadata() else {
        return Ok(false);
    };
    if !meta.file_type().is_symlink() {
        return Ok(false);
    }
    let target = fs::read_link(&mount)?;
    if !target.starts_with(cache_root(layout)) {
        return Ok(false);
    }
    fs::remove_file(&mount)?;
    Ok(true)
}

pub fn is_bound(layout: &BareLayout, worktree_dir: &Path, step: &CacheStep) -> bool {
    let mount = worktree_dir.join(&step.path);
    mount
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
        && fs::read_link(&mount).is_ok_and(|t| t == bucket_dir(layout, step, worktree_dir))
}

fn is_empty_dir(p: &Path) -> bool {
    fs::read_dir(p)
        .map(|mut d| d.next().is_none())
        .unwrap_or(true)
}

/// The bucket in this slot used most recently, to seed a new one from.
fn newest_bucket(layout: &BareLayout, step: &CacheStep, exclude: &str) -> Option<PathBuf> {
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for e in fs::read_dir(slot_dir(layout, step)).ok()?.flatten() {
        if e.file_name().to_string_lossy() == exclude {
            continue;
        }
        let p = e.path();
        if !p.is_dir() || is_empty_dir(&p) {
            continue;
        }
        let t = e
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        if best.as_ref().is_none_or(|(bt, _)| t > *bt) {
            best = Some((t, p));
        }
    }
    best.map(|(_, p)| p)
}

/// Copy a tree, sharing the blocks where the filesystem can.
///
/// On APFS and on btrfs/XFS this is close to free in both time and space, which
/// is what makes seeding worth doing at all: a new bucket starts warm without
/// two copies of a multi-gigabyte build directory.
fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let cow = std::process::Command::new("cp")
        .args(["-c", "-R"])
        .arg(src)
        .arg(dst)
        .status();
    #[cfg(all(unix, not(target_os = "macos")))]
    let cow = std::process::Command::new("cp")
        .args(["-a", "--reflink=auto"])
        .arg(src)
        .arg(dst)
        .status();
    #[cfg(unix)]
    if matches!(cow, Ok(s) if s.success()) {
        return Ok(());
    }
    copy_tree_slow(src, dst)
}

fn copy_tree_slow(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for e in fs::read_dir(src)? {
        let e = e?;
        let from = e.path();
        let to = dst.join(e.file_name());
        let ft = e.file_type()?;
        if ft.is_dir() {
            copy_tree_slow(&from, &to)?;
        } else if ft.is_symlink() {
            // A cache full of symlinks (node_modules/.bin) must keep them as
            // symlinks; following them would copy the world.
            let target = fs::read_link(&from)?;
            let _ = symlink_any(&target, &to);
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// keeping git quiet
// ---------------------------------------------------------------------------

/// Make sure the mount point is ignored, without touching a tracked file.
///
/// `.gitignore` belongs to the project; editing it would show up in every
/// diff. `info/exclude` in the common git dir is local to this clone and
/// applies to every worktree, which is exactly the scope of a cache mount.
pub fn ignore_in_repo(layout: &BareLayout, path: &str) -> Result<()> {
    const HEADER: &str = "# gwt cache mounts (managed by `git wt cache`)";
    let exclude = layout.bare_dir.join("info").join("exclude");
    let current = fs::read_to_string(&exclude).unwrap_or_default();
    let entry = format!("/{path}");
    if current.lines().any(|l| l.trim() == entry) {
        return Ok(());
    }
    fs::create_dir_all(exclude.parent().unwrap_or(&layout.bare_dir))?;
    let mut out = current;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if !out.contains(HEADER) {
        out.push_str(&format!("\n{HEADER}\n"));
    }
    out.push_str(&entry);
    out.push('\n');
    fs::write(&exclude, out)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// inspecting
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Bucket {
    pub slot: String,
    pub id: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub modified: SystemTime,
    /// Worktrees currently pointing at it, by directory name.
    pub used_by: Vec<String>,
}

/// Every bucket on disk, whether or not the recipe still mentions its slot.
///
/// Listing from the filesystem rather than the recipe is deliberate: removing a
/// cache step leaves the data behind, and that data is exactly what someone
/// running `cache ls` is trying to find.
pub fn buckets(layout: &BareLayout, worktrees: &[PathBuf]) -> Vec<Bucket> {
    let mut out = Vec::new();
    // Walk each worktree once and remember where its symlinks point, rather
    // than re-walking every worktree for every bucket.
    let mounts: Vec<(String, Vec<PathBuf>)> = worktrees
        .iter()
        .map(|wt| {
            (
                wt.file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                mount_targets(wt),
            )
        })
        .collect();

    let Ok(slots) = fs::read_dir(cache_root(layout)) else {
        return out;
    };
    for slot in slots.flatten().filter(|e| e.path().is_dir()) {
        let slot_name = slot.file_name().to_string_lossy().into_owned();
        let Ok(entries) = fs::read_dir(slot.path()) else {
            continue;
        };
        for b in entries.flatten().filter(|e| e.path().is_dir()) {
            let path = b.path();
            let used_by = mounts
                .iter()
                .filter(|(_, targets)| targets.contains(&path))
                .map(|(name, _)| name.clone())
                .collect();
            out.push(Bucket {
                slot: slot_name.clone(),
                id: b.file_name().to_string_lossy().into_owned(),
                bytes: dir_size(&path),
                modified: b
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH),
                used_by,
                path,
            });
        }
    }
    out.sort_by(|a, b| a.slot.cmp(&b.slot).then(b.bytes.cmp(&a.bytes)));
    out
}

/// Where the symlinks inside `worktree` point.
///
/// A mount can sit at a nested path (`.next/cache`), so this walks shallowly
/// rather than assuming the recipe still describes the link that is there —
/// a removed cache step leaves its symlink behind, and that link is exactly
/// what stops `gc` from deleting the data under it.
fn mount_targets(worktree: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if depth > 3 {
            return;
        }
        let Ok(rd) = fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                if let Ok(t) = fs::read_link(e.path()) {
                    out.push(t);
                }
            } else if ft.is_dir() && e.file_name() != ".git" {
                walk(&e.path(), depth + 1, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(worktree, 0, &mut out);
    out
}

pub fn dir_size(p: &Path) -> u64 {
    let Ok(rd) = fs::read_dir(p) else {
        return 0;
    };
    rd.flatten()
        .map(|e| match e.file_type() {
            Ok(ft) if ft.is_dir() => dir_size(&e.path()),
            // A symlink's own size is noise, and following it would double-count.
            Ok(ft) if ft.is_symlink() => 0,
            _ => e.metadata().map(|m| m.len()).unwrap_or(0),
        })
        .sum()
}

pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// Delete buckets nobody points at. `older_than_days` also spares recent ones,
/// because "nothing points at it" is true of every bucket you will need again
/// the moment you switch back to that branch.
pub fn gc(
    layout: &BareLayout,
    worktrees: &[PathBuf],
    older_than_days: Option<u64>,
) -> Result<Vec<Bucket>> {
    let cutoff = older_than_days.map(|d| {
        SystemTime::now()
            .checked_sub(std::time::Duration::from_secs(d * 86_400))
            .unwrap_or(SystemTime::UNIX_EPOCH)
    });
    let mut removed = Vec::new();
    for b in buckets(layout, worktrees) {
        if !b.used_by.is_empty() {
            continue;
        }
        if cutoff.is_some_and(|c| b.modified > c) {
            continue;
        }
        fs::remove_dir_all(&b.path)?;
        removed.push(b);
    }
    // A slot with no buckets left is just clutter.
    if let Ok(slots) = fs::read_dir(cache_root(layout)) {
        for slot in slots.flatten().filter(|e| e.path().is_dir()) {
            if is_empty_dir(&slot.path()) {
                let _ = fs::remove_dir(slot.path());
            }
        }
    }
    Ok(removed)
}

// ---------------------------------------------------------------------------
// presets
// ---------------------------------------------------------------------------

/// Cache steps worth having, inferred from what is checked into a worktree.
///
/// Every entry here is a directory a build tool rebuilds from scratch in a new
/// worktree, paired with the file that decides whether two worktrees may share
/// it. Nothing is written until the caller says so.
pub fn presets(worktree_dir: &Path) -> Vec<CacheStep> {
    let has = |p: &str| worktree_dir.join(p).exists();
    let mut out = Vec::new();
    let mut add = |path: &str, mode, key: &[&str], env: Option<&str>| {
        out.push(CacheStep {
            path: path.to_string(),
            mode,
            key: key.iter().map(|s| s.to_string()).collect(),
            seed: true,
            env: env.map(str::to_string),
        });
    };

    if has("Cargo.toml") {
        add(
            "target",
            CacheMode::Keyed,
            &["Cargo.lock", "rust-toolchain.toml"],
            Some("CARGO_TARGET_DIR"),
        );
    }
    if has("package.json") {
        let lock = ["pnpm-lock.yaml", "package-lock.json", "yarn.lock"]
            .into_iter()
            .find(|f| has(f))
            .unwrap_or("package.json");
        add("node_modules", CacheMode::Keyed, &[lock], None);
        if has("next.config.js") || has("next.config.mjs") || has("next.config.ts") {
            // Next.js documents this one as safe to carry between builds.
            add(".next/cache", CacheMode::Shared, &[], None);
        }
        if has("turbo.json") {
            add(".turbo", CacheMode::Shared, &[], None);
        }
    }
    if has("pyproject.toml") || has("requirements.txt") {
        let lock = ["uv.lock", "poetry.lock", "requirements.txt"]
            .into_iter()
            .find(|f| has(f))
            .unwrap_or("pyproject.toml");
        add(".venv", CacheMode::Keyed, &[lock], None);
    }
    if has("go.mod") {
        // Go's build cache is already global; what a worktree rebuilds is this.
        add("bin", CacheMode::Private, &[], None);
    }
    if has("build.gradle") || has("build.gradle.kts") {
        add(
            ".gradle",
            CacheMode::Keyed,
            &["gradle/wrapper/gradle-wrapper.properties"],
            None,
        );
    }
    out
}

// ---------------------------------------------------------------------------
// platform bits
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn symlink_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

#[cfg(windows)]
fn symlink_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(src, dst)
}

#[cfg(unix)]
fn symlink_any(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

#[cfg(windows)]
fn symlink_any(src: &Path, dst: &Path) -> std::io::Result<()> {
    if src.is_dir() {
        std::os::windows::fs::symlink_dir(src, dst)
    } else {
        std::os::windows::fs::symlink_file(src, dst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(mode: CacheMode, key: &[&str]) -> CacheStep {
        CacheStep {
            path: "target".into(),
            mode,
            key: key.iter().map(|s| s.to_string()).collect(),
            seed: false,
            env: None,
        }
    }

    #[test]
    fn a_slot_name_survives_a_nested_path() {
        assert_eq!(slot_name("target"), "target");
        assert_eq!(slot_name(".next/cache"), ".next+cache");
        assert_eq!(slot_name("a+b/c"), "a++b+c");
    }

    #[test]
    fn the_key_decides_the_bucket_not_the_worktree() {
        let dir = std::env::temp_dir().join(format!("gwt-cache-key-{}", std::process::id()));
        let (a, b, c) = (dir.join("a"), dir.join("b"), dir.join("c"));
        for d in [&a, &b, &c] {
            fs::create_dir_all(d).unwrap();
        }
        fs::write(a.join("Cargo.lock"), "same\n").unwrap();
        fs::write(b.join("Cargo.lock"), "same\n").unwrap();
        fs::write(c.join("Cargo.lock"), "different\n").unwrap();

        let s = step(CacheMode::Keyed, &["Cargo.lock"]);
        assert_eq!(
            bucket_id(&s, &a),
            bucket_id(&s, &b),
            "same lock, same bucket"
        );
        assert_ne!(
            bucket_id(&s, &a),
            bucket_id(&s, &c),
            "changed lock, own bucket"
        );

        // A missing key file is its own state, not "matches everything".
        let d = dir.join("d");
        fs::create_dir_all(&d).unwrap();
        assert_ne!(bucket_id(&s, &a), bucket_id(&s, &d));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn shared_and_private_ignore_the_key() {
        let wt = Path::new("/repo/feature-a");
        assert_eq!(bucket_id(&step(CacheMode::Shared, &["x"]), wt), "shared");
        assert_eq!(
            bucket_id(&step(CacheMode::Private, &["x"]), wt),
            "feature-a"
        );
    }

    #[test]
    fn sizes_read_the_way_people_write_them() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1024 * 1024 * 3 / 2), "1.5 MiB");
    }
}
