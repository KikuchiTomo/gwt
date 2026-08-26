# Build caches that outlive the worktree

A worktree is a fresh directory, so every build system starts cold. Six
worktrees of one repo means six `target/` directories and six full builds — and
deleting a worktree throws its cache away with it.

The obvious fix is to share one cache, and the obvious fix is wrong: two
branches with different lockfiles must not write to the same `node_modules`. So
a cache step moves the directory out of the worktree into a **bucket**, and
which bucket a worktree binds to is decided by the *contents* of the files that
would make sharing unsafe:

```
<repo-root>/.gwt/cache/target/a3f19c02b7e4/     <- bucket, keyed on Cargo.lock
<repo-root>/feature-a/target -> ../.gwt/cache/target/a3f19c02b7e4
<repo-root>/feature-b/target -> ../.gwt/cache/target/a3f19c02b7e4   same lock
<repo-root>/bump-deps/target -> ../.gwt/cache/target/71dd4e0af8c1   changed it
```

Nobody declares "these two branches are compatible". Change the lockfile and
that worktree moves to its own bucket by itself; change it back and it returns
to the shared one, still warm.

```sh
git wt cache init                    # detect what this project is built with
git wt sync cache target --key Cargo.lock --env CARGO_TARGET_DIR
git wt cache ls                      # buckets, sizes, who uses them
git wt cache gc [--older-than 30]    # delete buckets nobody points at
```

| mode | one bucket per | use it for |
| --- | --- | --- |
| `keyed` (default) | distinct contents of `key` | anything a lockfile governs |
| `shared` | the repo | caches that cannot be poisoned — download caches, content-addressed stores |
| `private` | worktree | shares nothing, but survives deleting the worktree |

`seed` is on by default: a brand-new bucket is filled from the most recently
used one by copy-on-write. On APFS, btrfs and XFS that costs neither time nor
space, so a worktree that has just been split off by a lockfile change starts
warm instead of empty.

## What it does to your working tree

An existing directory at the mount point is **adopted** — moved into its bucket,
not deleted — so bringing a warm 4 GB `target/` under management costs nothing.
If both the worktree and the bucket already hold a cache, gwt stops and says so
rather than merging them, because merging them is exactly the accident this
design exists to prevent.

The mount point is added to the clone-local `info/exclude`, not to `.gitignore`:
git stays quiet and the project's own file is left alone.

```
$ git wt cache ls
CACHE         BUCKET        SIZE     USED BY
------------  ------------  -------  ------------------
target        a3f19c02b7e4  4.1 GiB  feature-a, default
target        71dd4e0af8c1  2.7 GiB  bump-deps
node_modules  8c1e0b3d2af5  412 MiB  (unused)
```

## The part symlinks cannot fix

Sharing the directory solves one half. The other half is that some tools bake an
absolute path into their own cache keys, so the same bucket read from a
different worktree path can still miss. cargo's dep-info and incremental data
are like this: a shared `target/` used from two paths may rebuild anyway.

Where a tool accepts the cache location as an environment variable, that is the
better answer, because then the worktree path never enters into it:

```sh
git wt sync cache target --key Cargo.lock --env CARGO_TARGET_DIR
eval "$(git wt cache env)"     # export CARGO_TARGET_DIR=…/.gwt/cache/target/a3f…
```

Otherwise the honest advice is to lean on `keyed` plus `seed`, which gives you
"not shared, but warm" and sidesteps the question entirely.

## Keeping the binding current

A keyed bucket is chosen from a lockfile, so switching branches can invalidate
the choice. `git wt sync apply` re-checks and re-points every mount, and

```sh
git wt cache hooks
```

installs `post-checkout` and `post-merge` in the bare repo's `hooks/` — shared
by every worktree — so it happens on its own.
