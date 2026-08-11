//! End-to-end tests for the operations that create, adopt, and destroy
//! worktrees/branches. These shell out to a real `git`, against a real bare-style
//! layout, because that is the only way to be sure the destructive paths behave.

use std::path::{Path, PathBuf};
use std::process::Command;

use gwt_core::cache::{self, CacheMode, CacheStep};
use gwt_core::layout::BareLayout;
use gwt_core::ops;
use gwt_core::sync::{self, CopyStep, LinkStep, Phase, RunStep, Step};

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

/// git hands back canonical paths, and macOS's /var is a symlink to
/// /private/var — compare what the filesystem says, not the strings.
fn same_path(a: &Path, b: &Path) -> bool {
    let c = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    c(a) == c(b)
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
    ops::add(&layout, "feature", "feat", &mut sync::noop).unwrap();
    ops::remove(&layout, "feat").unwrap();
    git(
        &layout.root,
        &["--git-dir", ".bare", "branch", "feature", "origin/feature"],
    );
    assert!(ops::branch_exists_local(&layout, "feature").unwrap());

    let dest = ops::add_existing_branch(&layout, "feature", "feat2", &mut sync::noop).unwrap();
    assert!(dest.is_dir());
    assert_eq!(branch_of(&dest), "feature");
}

#[test]
fn reports_which_worktree_holds_a_branch() {
    let (_origin, layout) = fixture("holder");
    let dest = ops::add(&layout, "feature", "feat", &mut sync::noop).unwrap();

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
    let wt = ops::add(&layout, "feature", "feat", &mut sync::noop).unwrap();
    commit(&wt, "local.txt", "local only\n", "local work");
    let local_head = git(&wt, &["rev-parse", "HEAD"]).trim().to_string();

    // Drop the worktree but keep the (now diverged) local branch behind.
    git(&layout.root, &["worktree", "remove", "--force", "feat"]);
    assert!(ops::branch_exists_local(&layout, "feature").unwrap());

    let dest =
        ops::recreate_branch_from_remote(&layout, "feature", "feat", &mut sync::noop).unwrap();
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
fn recreating_a_worktree_replaces_it_and_reruns_the_recipe() {
    let (_origin, layout) = fixture("recreate-wt");
    std::fs::create_dir_all(&layout.secrets_dir).unwrap();
    std::fs::write(layout.secrets_dir.join(".env"), "TOKEN=1\n").unwrap();
    ops::sync_add(
        &layout,
        Step::Link(LinkStep {
            src: "secrets/.env".into(),
            dst: ".env".into(),
        }),
    )
    .unwrap();

    let wt = ops::add(&layout, "feature", "feat", &mut sync::noop).unwrap();
    std::fs::write(wt.join("junk.txt"), "uncommitted\n").unwrap();
    assert!(wt.join("junk.txt").exists());

    let dest = ops::recreate_worktree(&layout, "feat", "feature", None, &mut sync::noop).unwrap();
    assert!(dest.is_dir());
    assert!(
        !dest.join("junk.txt").exists(),
        "the old working tree should be gone"
    );
    assert_eq!(branch_of(&dest), "feature");
    assert!(
        dest.join(".env").symlink_metadata().unwrap().is_symlink(),
        "the recipe is re-applied to the rebuilt worktree"
    );
}

#[test]
fn recreating_a_worktree_clears_a_stray_directory() {
    let (_origin, layout) = fixture("stray");
    // A plain directory git knows nothing about still blocks `worktree add`.
    let stray = layout.root.join("feat");
    std::fs::create_dir_all(stray.join("nested")).unwrap();
    std::fs::write(stray.join("nested/file.txt"), "junk\n").unwrap();

    let dest = ops::recreate_worktree(&layout, "feat", "feature", None, &mut sync::noop).unwrap();
    assert!(dest.join(".git").exists(), "should be a real worktree now");
    assert!(!dest.join("nested").exists());
    assert_eq!(branch_of(&dest), "feature");
}

#[test]
fn recreating_a_worktree_from_a_base_creates_a_fresh_branch() {
    let (_origin, layout) = fixture("recreate-base");
    ops::new(&layout, "main", "wip", "wip", &mut sync::noop).unwrap();

    let dest =
        ops::recreate_worktree(&layout, "wip", "wip", Some("main"), &mut sync::noop).unwrap();
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
    let wt = ops::add(&layout, "feature", "feat", &mut sync::noop).unwrap();

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
    let fresh = ops::new(&layout, "main", "pushme", "pushme", &mut sync::noop).unwrap();
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

/// gwt ≤ 0.6.3 wrote a relative pointer into `.bare/worktrees/<id>/gitdir`
/// unconditionally, but only git 2.48+ reads one back. On Ubuntu 22.04/24.04
/// git reported the worktree as `../../../default` — which the picker then
/// handed to the shell, so `Enter` moved nowhere — and marked it prunable, so
/// a routine `git gc` was entitled to delete the metadata. Whatever this git
/// does with such a repo, the paths we hand out must be absolute and real.
#[test]
fn a_relative_gitdir_pointer_still_yields_usable_worktree_paths() {
    let (_origin, layout) = fixture("relgitdir");
    let meta = std::fs::read_dir(layout.bare_dir.join("worktrees"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::write(meta.join("gitdir"), "../../../default/.git\n").unwrap();

    let repo = gwt_core::Repo::discover(&layout.root).unwrap();
    let wts = repo.list_worktrees().unwrap();
    let default = wts
        .iter()
        .find(|w| w.name() == "default")
        .expect("the default worktree is still listed");
    assert!(
        default.path.is_absolute(),
        "handed a relative path to the caller: {}",
        default.path.display()
    );
    assert!(default.path.join(".git").exists());
    assert!(same_path(&default.path, &layout.root.join("default")));

    // And the pointer itself is left in a state this git can read, so `git gc`
    // cannot decide the worktree is prunable.
    let pointer = std::fs::read_to_string(meta.join("gitdir")).unwrap();
    let readable = Path::new(pointer.trim()).is_absolute()
        || gwt_core::git::understands_relative_gitdirs(&layout.root);
    assert!(readable, "left an unreadable pointer: {pointer}");
}

/// The writer half: on a git that cannot read relative pointers we must not
/// create them in the first place.
#[test]
fn relativize_never_writes_a_pointer_this_git_cannot_read() {
    let (_origin, layout) = fixture("relwrite");
    gwt_core::relativize::relativize_one(&layout, Path::new("default")).unwrap();

    let meta = std::fs::read_dir(layout.bare_dir.join("worktrees"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let pointer = std::fs::read_to_string(meta.join("gitdir")).unwrap();
    if !gwt_core::git::understands_relative_gitdirs(&layout.root) {
        assert!(
            Path::new(pointer.trim()).is_absolute(),
            "git {:?} cannot read {pointer}",
            gwt_core::git::version(&layout.root)
        );
    }
    // The worktree side stays relative either way: that is the portable half,
    // and every git version has understood it.
    let dot_git = std::fs::read_to_string(layout.root.join("default/.git")).unwrap();
    assert!(dot_git.contains("gitdir: ../.bare/worktrees/"), "{dot_git}");

    // Whatever we wrote, git must still resolve the worktree.
    let repo = gwt_core::Repo::discover(&layout.root).unwrap();
    let wts = repo.list_worktrees().unwrap();
    assert!(wts
        .iter()
        .any(|w| same_path(&w.path, &layout.root.join("default"))));
}

#[test]
fn a_copy_step_lands_a_real_file_and_leaves_edits_alone() {
    let (_origin, layout) = fixture("copy-step");
    std::fs::create_dir_all(&layout.secrets_dir).unwrap();
    std::fs::write(layout.secrets_dir.join("env.sample"), "BRANCH={{branch}}\n").unwrap();
    ops::sync_add(
        &layout,
        Step::Copy(CopyStep {
            src: "secrets/env.sample".into(),
            dst: ".env".into(),
            overwrite: false,
            render: true,
        }),
    )
    .unwrap();

    let wt = ops::add(&layout, "feature", "feat", &mut sync::noop).unwrap();
    let landed = wt.join(".env");
    assert!(
        !landed.symlink_metadata().unwrap().is_symlink(),
        "a copy must be a real file, not a link"
    );
    assert_eq!(
        std::fs::read_to_string(&landed).unwrap(),
        "BRANCH=feature\n",
        "{{{{branch}}}} should have been rendered"
    );

    // Re-applying must not eat the edit the user made in the worktree.
    std::fs::write(&landed, "BRANCH=feature\nEDITED=1\n").unwrap();
    ops::sync_apply(&layout, Phase::Apply, &mut sync::noop).unwrap();
    assert_eq!(
        std::fs::read_to_string(&landed).unwrap(),
        "BRANCH=feature\nEDITED=1\n"
    );
}

#[test]
fn a_run_step_fires_on_create_but_not_on_a_plain_apply() {
    let (_origin, layout) = fixture("run-step");
    ops::sync_add(
        &layout,
        Step::Run(RunStep {
            // Proves both that the command runs in the worktree and that it is
            // told which worktree it is in.
            cmd: "echo \"$GWT_BRANCH\" >> ran.txt".into(),
            when: vec![Phase::Create],
            only_if: None,
            timeout: std::time::Duration::from_secs(30),
            dir: None,
        }),
    )
    .unwrap();

    let wt = ops::add(&layout, "feature", "feat", &mut sync::noop).unwrap();
    let marker = wt.join("ran.txt");
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "feature\n");

    // `when = ["create"]`, so re-applying the recipe must not run it again.
    ops::sync_apply(&layout, Phase::Apply, &mut sync::noop).unwrap();
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "feature\n");

    // Asking for the apply phase explicitly is how you opt in.
    ops::sync_apply(&layout, Phase::Create, &mut sync::noop).unwrap();
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap(),
        "feature\nfeature\n"
    );
}

#[test]
fn a_failing_run_step_is_reported_without_destroying_the_worktree() {
    let (_origin, layout) = fixture("run-fails");
    ops::sync_add(
        &layout,
        Step::Run(RunStep {
            cmd: "exit 3".into(),
            when: vec![Phase::Create],
            only_if: None,
            timeout: std::time::Duration::from_secs(30),
            dir: None,
        }),
    )
    .unwrap();

    let wt = ops::add(&layout, "feature", "feat", &mut sync::noop).unwrap();
    assert!(wt.is_dir(), "a failed command must not undo the worktree");

    let outcomes = sync::apply_quiet(&layout, &wt, Phase::Create).unwrap();
    assert!(outcomes.iter().any(|(_, o)| o.is_failure()));
}

#[test]
fn an_old_secrets_manifest_is_still_honoured_and_upgraded_on_write() {
    let (_origin, layout) = fixture("legacy");
    std::fs::remove_file(&layout.sync_config).unwrap();
    std::fs::create_dir_all(&layout.secrets_dir).unwrap();
    std::fs::write(layout.secrets_dir.join(".env"), "TOKEN=1\n").unwrap();
    std::fs::write(&layout.legacy_manifest, "secrets/.env\t.env\n").unwrap();

    let cfg = sync::load(&layout).unwrap();
    assert_eq!(cfg.origin, sync::Origin::Legacy);
    assert_eq!(cfg.steps.len(), 1);

    let wt = ops::add(&layout, "feature", "feat", &mut sync::noop).unwrap();
    assert!(wt.join(".env").symlink_metadata().unwrap().is_symlink());

    // The first write moves the recipe to TOML, carrying the old rows along.
    ops::sync_add(
        &layout,
        Step::Link(LinkStep {
            src: "secrets/.env".into(),
            dst: "config/.env".into(),
        }),
    )
    .unwrap();
    let cfg = sync::load(&layout).unwrap();
    assert_eq!(cfg.origin, sync::Origin::Toml);
    assert_eq!(cfg.steps.len(), 2, "the legacy row survived the upgrade");
}

#[test]
fn editing_a_recipe_keeps_the_comments_around_it() {
    let (_origin, layout) = fixture("comments");
    std::fs::write(
        &layout.sync_config,
        "# my recipe\nversion = 1\n\n# the api token\n[[step]]\ntype = \"link\"\nsrc = \"secrets/.env\"\ndst = \".env\"\n",
    )
    .unwrap();

    ops::sync_add(
        &layout,
        Step::Link(LinkStep {
            src: "secrets/gcp.json".into(),
            dst: "config/gcp.json".into(),
        }),
    )
    .unwrap();

    let raw = std::fs::read_to_string(&layout.sync_config).unwrap();
    assert!(raw.contains("# my recipe"), "{raw}");
    assert!(raw.contains("# the api token"), "{raw}");
    assert!(raw.contains("config/gcp.json"), "{raw}");
}

/// git answers `worktree list` with resolved paths, so on macOS the bare dir
/// comes back as `/private/var/…` while the layout holds `/var/…`. A string
/// comparison missed it, and the recipe was applied inside `.bare` itself.
#[test]
fn the_bare_dir_is_never_treated_as_a_worktree() {
    let (_origin, layout) = fixture("bare-excluded");
    let dirs = ops::worktree_dirs(&layout).unwrap();
    for d in &dirs {
        assert!(
            !d.ends_with(".bare"),
            "the bare dir must not be in the worktree list: {d:?}"
        );
    }
    assert!(dirs.iter().any(|d| d.ends_with("default")));

    std::fs::create_dir_all(&layout.secrets_dir).unwrap();
    std::fs::write(layout.secrets_dir.join(".env"), "TOKEN=1\n").unwrap();
    ops::sync_add(
        &layout,
        Step::Link(LinkStep {
            src: "secrets/.env".into(),
            dst: ".env".into(),
        }),
    )
    .unwrap();
    assert!(
        !layout.bare_dir.join(".env").exists(),
        "nothing belongs inside .bare"
    );
}

// ---------------------------------------------------------------------------
// build caches
// ---------------------------------------------------------------------------

fn cache_step(path: &str, mode: CacheMode, key: &[&str], seed: bool) -> Step {
    Step::Cache(CacheStep {
        path: path.into(),
        mode,
        key: key.iter().map(|s| s.to_string()).collect(),
        seed,
        env: None,
    })
}

fn read_link_name(p: &Path) -> String {
    std::fs::read_link(p)
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

/// The whole point of `keyed`: two worktrees share a cache while it is safe to,
/// and separate themselves the moment it is not — with nobody declaring it.
#[test]
fn a_keyed_cache_is_shared_until_the_key_diverges() {
    let (_origin, layout) = fixture("cache-keyed");
    let a = ops::add(&layout, "feature", "a", &mut sync::noop).unwrap();
    let b = ops::new(&layout, "main", "b", "b", &mut sync::noop).unwrap();
    for wt in [&a, &b] {
        std::fs::write(wt.join("Cargo.lock"), "v1\n").unwrap();
    }

    ops::sync_add(
        &layout,
        cache_step("target", CacheMode::Keyed, &["Cargo.lock"], false),
    )
    .unwrap();
    ops::sync_apply(&layout, Phase::Apply, &mut sync::noop).unwrap();

    assert!(a.join("target").symlink_metadata().unwrap().is_symlink());
    assert_eq!(
        read_link_name(&a.join("target")),
        read_link_name(&b.join("target")),
        "identical lockfiles must land in the same bucket"
    );

    // Writing through one symlink is visible through the other: it is one dir.
    std::fs::write(a.join("target/built.bin"), "artifact\n").unwrap();
    assert!(b.join("target/built.bin").exists());

    // Now `b` bumps a dependency. It must stop sharing, by itself.
    std::fs::write(b.join("Cargo.lock"), "v2\n").unwrap();
    ops::sync_apply(&layout, Phase::Apply, &mut sync::noop).unwrap();
    assert_ne!(
        read_link_name(&a.join("target")),
        read_link_name(&b.join("target")),
        "a changed lockfile must not keep writing to the shared cache"
    );
    assert!(a.join("target/built.bin").exists(), "a keeps its cache");
    assert!(!b.join("target/built.bin").exists(), "b starts clean");

    // And going back to the old lockfile returns to the old bucket, still warm.
    std::fs::write(b.join("Cargo.lock"), "v1\n").unwrap();
    ops::sync_apply(&layout, Phase::Apply, &mut sync::noop).unwrap();
    assert!(
        b.join("target/built.bin").exists(),
        "returning to a known key should return to its cache"
    );
}

#[test]
fn shared_and_private_modes_do_what_they_say() {
    let (_origin, layout) = fixture("cache-modes");
    let a = ops::add(&layout, "feature", "a", &mut sync::noop).unwrap();
    let b = ops::new(&layout, "main", "b", "b", &mut sync::noop).unwrap();

    ops::sync_add(&layout, cache_step(".turbo", CacheMode::Shared, &[], false)).unwrap();
    ops::sync_add(&layout, cache_step("bin", CacheMode::Private, &[], false)).unwrap();
    ops::sync_apply(&layout, Phase::Apply, &mut sync::noop).unwrap();

    assert_eq!(
        read_link_name(&a.join(".turbo")),
        read_link_name(&b.join(".turbo"))
    );
    assert_eq!(read_link_name(&a.join("bin")), "a");
    assert_eq!(read_link_name(&b.join("bin")), "b");

    // A private bucket outlives its worktree, which is the reason to use one.
    std::fs::write(a.join("bin/tool"), "compiled\n").unwrap();
    ops::remove(&layout, "a").unwrap();
    let a2 = ops::add(&layout, "feature", "a", &mut sync::noop).unwrap();
    assert!(
        a2.join("bin/tool").exists(),
        "re-creating the worktree should find the cache still there"
    );
}

/// Bringing a warm directory under management must not throw it away.
#[test]
fn an_existing_cache_directory_is_adopted_not_deleted() {
    let (_origin, layout) = fixture("cache-adopt");
    let a = ops::add(&layout, "feature", "a", &mut sync::noop).unwrap();
    std::fs::create_dir_all(a.join("target/debug")).unwrap();
    std::fs::write(a.join("target/debug/app"), "expensive\n").unwrap();

    ops::sync_add(&layout, cache_step("target", CacheMode::Shared, &[], false)).unwrap();

    assert!(a.join("target").symlink_metadata().unwrap().is_symlink());
    assert_eq!(
        std::fs::read_to_string(a.join("target/debug/app")).unwrap(),
        "expensive\n"
    );
}

/// Two real caches must never be silently merged.
#[test]
fn a_cache_that_would_have_to_be_merged_is_refused() {
    let (_origin, layout) = fixture("cache-blocked");
    let a = ops::add(&layout, "feature", "a", &mut sync::noop).unwrap();
    let b = ops::new(&layout, "main", "b", "b", &mut sync::noop).unwrap();
    for (wt, body) in [(&a, "from a\n"), (&b, "from b\n")] {
        std::fs::create_dir_all(wt.join("target")).unwrap();
        std::fs::write(wt.join("target/app"), body).unwrap();
    }

    ops::sync_add(&layout, cache_step("target", CacheMode::Shared, &[], false)).unwrap();
    let outcomes = sync::apply_quiet(&layout, &b, Phase::Apply).unwrap();

    assert!(
        outcomes
            .iter()
            .any(|(_, o)| matches!(o, sync::Outcome::Blocked { .. })),
        "got {outcomes:?}"
    );
    // Whichever was adopted first keeps its data; the other keeps its own.
    assert_eq!(
        std::fs::read_to_string(b.join("target/app")).unwrap(),
        "from b\n",
        "the refused worktree must keep what it had"
    );
}

#[test]
fn a_new_bucket_can_be_seeded_from_the_last_one() {
    let (_origin, layout) = fixture("cache-seed");
    let a = ops::add(&layout, "feature", "a", &mut sync::noop).unwrap();
    std::fs::write(a.join("Cargo.lock"), "v1\n").unwrap();
    ops::sync_add(
        &layout,
        cache_step("target", CacheMode::Keyed, &["Cargo.lock"], true),
    )
    .unwrap();
    ops::sync_apply(&layout, Phase::Apply, &mut sync::noop).unwrap();
    std::fs::create_dir_all(a.join("target/deps")).unwrap();
    std::fs::write(a.join("target/deps/libfoo.rlib"), "compiled\n").unwrap();

    // A dependency bump moves it to a fresh bucket — which starts warm.
    std::fs::write(a.join("Cargo.lock"), "v2\n").unwrap();
    ops::sync_apply(&layout, Phase::Apply, &mut sync::noop).unwrap();
    assert_eq!(
        std::fs::read_to_string(a.join("target/deps/libfoo.rlib")).unwrap(),
        "compiled\n",
        "the new bucket should have been seeded from the old one"
    );
}

#[test]
fn gc_removes_only_the_buckets_nothing_points_at() {
    let (_origin, layout) = fixture("cache-gc");
    let a = ops::add(&layout, "feature", "a", &mut sync::noop).unwrap();
    std::fs::write(a.join("Cargo.lock"), "v1\n").unwrap();
    ops::sync_add(
        &layout,
        cache_step("target", CacheMode::Keyed, &["Cargo.lock"], false),
    )
    .unwrap();
    ops::sync_apply(&layout, Phase::Apply, &mut sync::noop).unwrap();
    std::fs::write(a.join("target/v1.bin"), "one\n").unwrap();

    std::fs::write(a.join("Cargo.lock"), "v2\n").unwrap();
    ops::sync_apply(&layout, Phase::Apply, &mut sync::noop).unwrap();
    std::fs::write(a.join("target/v2.bin"), "two\n").unwrap();

    // Three buckets: `a` on v2, `a`'s abandoned v1, and `default`, which has no
    // Cargo.lock at all and is therefore its own case rather than everyone's.
    let worktrees = ops::worktree_dirs(&layout).unwrap();
    assert_eq!(cache::buckets(&layout, &worktrees).len(), 3);

    let removed = cache::gc(&layout, &worktrees, None).unwrap();
    assert_eq!(removed.len(), 1, "only the abandoned bucket goes");
    assert!(
        a.join("target/v2.bin").exists(),
        "the live bucket must survive"
    );
    assert_eq!(cache::buckets(&layout, &worktrees).len(), 2);

    // And nothing is deleted while a worktree still points at it, however old.
    assert!(cache::gc(&layout, &worktrees, None).unwrap().is_empty());
}

/// The mount point must not show up as an untracked file, and saying so must
/// not mean editing a file the project tracks.
#[test]
fn a_cache_mount_is_ignored_without_touching_gitignore() {
    let (_origin, layout) = fixture("cache-ignore");
    let a = ops::add(&layout, "feature", "a", &mut sync::noop).unwrap();
    ops::sync_add(&layout, cache_step("target", CacheMode::Shared, &[], false)).unwrap();

    assert!(
        git(&a, &["status", "--porcelain"]).trim().is_empty(),
        "the cache mount should not be reported as untracked"
    );
    assert!(
        !a.join(".gitignore").exists(),
        "the project file is ours to leave alone"
    );
    let exclude = std::fs::read_to_string(layout.bare_dir.join("info/exclude")).unwrap();
    assert!(exclude.contains("/target"), "{exclude}");
}

/// A cache mount lives beside the worktrees, not inside one.
#[test]
fn cache_data_never_lands_inside_a_worktree() {
    let (_origin, layout) = fixture("cache-location");
    let a = ops::add(&layout, "feature", "a", &mut sync::noop).unwrap();
    ops::sync_add(&layout, cache_step("target", CacheMode::Shared, &[], false)).unwrap();

    let target = std::fs::read_link(a.join("target")).unwrap();
    assert!(
        target.starts_with(layout.gwt_dir.join("cache")),
        "{target:?} should be under .gwt/cache"
    );
    assert!(!target.starts_with(&a), "and not inside the worktree");
}
