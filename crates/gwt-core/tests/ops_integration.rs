//! End-to-end tests for the operations that create, adopt, and destroy
//! worktrees/branches. These shell out to a real `git`, against a real bare-style
//! layout, because that is the only way to be sure the destructive paths behave.

use std::path::{Path, PathBuf};
use std::process::Command;

use gwt_core::layout::BareLayout;
use gwt_core::{ops, secrets};

/// Unique-enough scratch dir without pulling in a tempdir crate.
fn scratch(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!(
        "gwt-it-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    base
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn commit(cwd: &Path, file: &str, body: &str, msg: &str) {
    std::fs::write(cwd.join(file), body).unwrap();
    git(cwd, &["add", "-A"]);
    git(cwd, &["commit", "-qm", msg]);
}

/// An `origin` repo with `main` plus a `feature` branch, cloned into a
/// bare-style root. Returns (origin, layout).
fn fixture(tag: &str) -> (PathBuf, BareLayout) {
    let base = scratch(tag);
    let origin = base.join("origin");
    std::fs::create_dir_all(&origin).unwrap();
    git(&origin, &["init", "-q", "-b", "main", "."]);
    commit(&origin, "a.txt", "one\n", "init");
    git(&origin, &["branch", "feature"]);
    // A non-bare origin refuses pushes to the checked-out branch; park it.
    git(&origin, &["config", "receive.denyCurrentBranch", "ignore"]);

    let root = ops::clone(origin.to_str().unwrap(), Some("repo"), &base).unwrap();
    let layout = BareLayout::require(&root).unwrap();
    (origin, layout)
}

fn branch_of(wt: &Path) -> String {
    ops::current_branch(wt).unwrap()
}

#[test]
fn adopts_an_existing_local_branch_into_a_new_worktree() {
    let (_origin, layout) = fixture("adopt");
    // Make `feature` exist locally without a worktree.
    ops::add(&layout, "feature", "feat").unwrap();
    ops::remove(&layout, "feat").unwrap();
    git(
        &layout.root,
        &["--git-dir", ".bare", "branch", "feature", "origin/feature"],
    );
    assert!(ops::branch_exists_local(&layout, "feature").unwrap());

    let dest = ops::add_existing_branch(&layout, "feature", "feat2").unwrap();
    assert!(dest.is_dir());
    assert_eq!(branch_of(&dest), "feature");
}

#[test]
fn reports_which_worktree_holds_a_branch() {
    let (_origin, layout) = fixture("holder");
    let dest = ops::add(&layout, "feature", "feat").unwrap();

    let holder = ops::worktree_holding_branch(&layout, "feature").unwrap();
    // git reports resolved paths; on macOS the temp dir is behind a symlink.
    assert_eq!(
        holder.map(|p| std::fs::canonicalize(p).unwrap()),
        Some(std::fs::canonicalize(&dest).unwrap())
    );
    assert_eq!(
        ops::worktree_holding_branch(&layout, "nope").unwrap(),
        None,
        "an unknown branch is held by nobody"
    );
}

#[test]
fn recreating_a_branch_from_remote_discards_local_commits() {
    let (_origin, layout) = fixture("recreate-branch");
    let wt = ops::add(&layout, "feature", "feat").unwrap();
    commit(&wt, "local.txt", "local only\n", "local work");
    let local_head = git(&wt, &["rev-parse", "HEAD"]).trim().to_string();

    // Drop the worktree but keep the (now diverged) local branch behind.
    git(&layout.root, &["worktree", "remove", "--force", "feat"]);
    assert!(ops::branch_exists_local(&layout, "feature").unwrap());

    let dest = ops::recreate_branch_from_remote(&layout, "feature", "feat").unwrap();
    let new_head = git(&dest, &["rev-parse", "HEAD"]).trim().to_string();
    let origin_head = git(
        &layout.root,
        &["--git-dir", ".bare", "rev-parse", "origin/feature"],
    )
    .trim()
    .to_string();

    assert_ne!(new_head, local_head, "the local commit should be gone");
    assert_eq!(new_head, origin_head, "should match origin/feature");
}

#[test]
fn recreating_a_worktree_replaces_it_and_reapplies_secrets() {
    let (_origin, layout) = fixture("recreate-wt");
    std::fs::create_dir_all(&layout.secrets_dir).unwrap();
    std::fs::write(layout.secrets_dir.join(".env"), "TOKEN=1\n").unwrap();
    secrets::add_entry(&layout, "secrets/.env", ".env").unwrap();

    let wt = ops::add(&layout, "feature", "feat").unwrap();
    std::fs::write(wt.join("junk.txt"), "uncommitted\n").unwrap();
    assert!(wt.join("junk.txt").exists());

    let dest = ops::recreate_worktree(&layout, "feat", "feature", None).unwrap();
    assert!(dest.is_dir());
    assert!(
        !dest.join("junk.txt").exists(),
        "the old working tree should be gone"
    );
    assert_eq!(branch_of(&dest), "feature");
    assert!(
        dest.join(".env").symlink_metadata().unwrap().is_symlink(),
        "secrets are re-linked into the rebuilt worktree"
    );
}

#[test]
fn recreating_a_worktree_clears_a_stray_directory() {
    let (_origin, layout) = fixture("stray");
    // A plain directory git knows nothing about still blocks `worktree add`.
    let stray = layout.root.join("feat");
    std::fs::create_dir_all(stray.join("nested")).unwrap();
    std::fs::write(stray.join("nested/file.txt"), "junk\n").unwrap();

    let dest = ops::recreate_worktree(&layout, "feat", "feature", None).unwrap();
    assert!(dest.join(".git").exists(), "should be a real worktree now");
    assert!(!dest.join("nested").exists());
    assert_eq!(branch_of(&dest), "feature");
}

#[test]
fn recreating_a_worktree_from_a_base_creates_a_fresh_branch() {
    let (_origin, layout) = fixture("recreate-base");
    ops::new(&layout, "main", "wip", "wip").unwrap();

    let dest = ops::recreate_worktree(&layout, "wip", "wip", Some("main")).unwrap();
    assert_eq!(branch_of(&dest), "wip");
    let main_head = git(
        &layout.root,
        &["--git-dir", ".bare", "rev-parse", "origin/main"],
    )
    .trim()
    .to_string();
    assert_eq!(git(&dest, &["rev-parse", "HEAD"]).trim(), main_head);
}

#[test]
fn pull_is_fast_forward_only_and_push_sets_upstream() {
    let (origin, layout) = fixture("sync");
    let wt = ops::add(&layout, "feature", "feat").unwrap();

    // A bare-style clone leaves branches with no upstream; the first pull adopts
    // origin/<branch> instead of failing on git's "no tracking information".
    let msg = ops::pull(&wt).unwrap();
    assert!(
        msg.to_lowercase().contains("up to date"),
        "unexpected pull message: {msg}"
    );
    assert!(msg.contains("tracking origin/feature"), "got: {msg}");
    // Second pull finds the upstream already set.
    let msg = ops::pull(&wt).unwrap();
    assert!(!msg.contains("tracking origin/"), "got: {msg}");

    // A brand-new branch has no upstream — push must create it.
    let fresh = ops::new(&layout, "main", "pushme", "pushme").unwrap();
    commit(&fresh, "b.txt", "two\n", "work");
    ops::push(&fresh).unwrap();
    let pushed = git(&origin, &["rev-parse", "refs/heads/pushme"])
        .trim()
        .to_string();
    assert_eq!(git(&fresh, &["rev-parse", "HEAD"]).trim(), pushed);

    // Second push goes through the plain `git push` path.
    commit(&fresh, "c.txt", "three\n", "more work");
    ops::push(&fresh).unwrap();
    assert_eq!(
        git(&fresh, &["rev-parse", "HEAD"]).trim(),
        git(&origin, &["rev-parse", "refs/heads/pushme"]).trim()
    );

    // And a real fast-forward gets picked up by pull.
    commit(&origin, "d.txt", "four\n", "upstream work"); // on main
    let main_wt = layout.root.join("default");
    let before = git(&main_wt, &["rev-parse", "HEAD"]).trim().to_string();
    ops::pull(&main_wt).unwrap();
    assert_ne!(git(&main_wt, &["rev-parse", "HEAD"]).trim(), before);
}

#[test]
fn pull_refuses_to_merge_divergent_history() {
    let (origin, layout) = fixture("diverge");
    let wt = layout.root.join("default");
    commit(&wt, "mine.txt", "mine\n", "local work");
    commit(&origin, "theirs.txt", "theirs\n", "their work");

    // --ff-only means this is an error, not a merge commit or a conflict.
    assert!(
        ops::pull(&wt).is_err(),
        "divergent history must not be merged behind the user's back"
    );
}
