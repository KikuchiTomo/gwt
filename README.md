# gwt — git worktree, the comfy way

`git wt` is a cross-platform Rust replacement for ad-hoc `git worktree`
wrappers. It ships as a `git` subcommand and adds three things on top of plain
worktree management:

- an **inline, fzf-style picker** of the worktrees in the repo — pick one and
  the shell `cd`s into it;
- a **recipe** per repo, so a new worktree arrives with the `.env`, the config
  and the `npm ci` that git does not carry;
- **build caches** that live outside the worktrees and outlive them.

```
╭ git wt · 3/3 ──────────────────────────────────────────────────────────────╮
│  NAME       BRANCH          REMOTE   DIRTY  STASH  PATH                    │
│▌ default    main            ↑0 ↓2    0      0      /repo/default           │
│  feature-a  feat/login      ↑1 ↓0    3      1      /repo/feature-a         │
│  review-88  fix/flaky-test  =        0      0      /repo/review-88         │
╰ ↑↓:nav  enter:cd  p/P:pull/push  d:del  n:new  r:review  f:filter  ?:keys ─╯
```

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/KikuchiTomo/gwt/main/install.sh | sh
```

The installer drops the binary at `~/.local/bin/git-wt` and offers to add a
block to your shell rc that wires up `PATH` and the `gwt` shell function — which
is what lets `Enter` actually change your directory. Open a new shell afterwards,
then run `git wt`.

macOS arm64, Linux x86_64 (musl / gnu), Windows x86_64. Manual setup, `fish`,
uninstalling and the "`Enter` did nothing" checklist are in
[docs/install.md](docs/install.md).

## Usage

| command | what it does |
| --- | --- |
| `git wt` | inline picker |
| `git wt --display` | fullscreen live dashboard |
| `git wt clone <url> [dir]` | clone into a bare-style root + a `default` worktree |
| `git wt list` / `ls` | table: branch, ahead/behind, dirty, stash |
| `git wt add <branch> <name>` | adopt an existing branch as a worktree |
| `git wt new <base> <branch> <name>` | branch off `<base>` into a new worktree |
| `git wt review <branch>` | fetch `origin/<branch>` and make a tracking worktree |
| `git wt remove <name>` / `rm` | remove the worktree and delete its local branch |
| `git wt check <branch> [--fetch]` | compare local `<branch>` against `origin/<branch>` |
| `git wt sync` | interactive manager for the worktree recipe |
| `git wt sync add/copy/run/rm/ls/move/apply/edit` | the same, non-interactively |
| `git wt cache ls` / `gc` / `init` / `env` / `hooks` | build caches |
| `git wt relativize [name]` | make worktree gitdir pointers portable |
| `git wt config [lang <en\|ja>]` | show or set the interface language |
| `git wt shellinit <shell>` | emit the shell function for `cd` integration |

Every command works from anywhere inside the repo — the root, a worktree, or a
directory inside one.

### The picker

| key | action |
| --- | --- |
| `↑↓` / `jk` / `^n^p` | move |
| `Enter` | `cd` to the selected worktree |
| `Tab` / `Space` / `a` | multi-select / select all |
| `e` / `n` (`E` / `N`) | new worktree from a base branch (also naming the dir) |
| `r` | review — pick a remote branch, create a worktree |
| `p` / `P` | pull / push |
| `d` / `D` | delete / force delete |
| `f` / `/` | filter |
| `?` | every key binding, in the current language |

Nothing in the picker waits on git. The list draws before the counts are in, the
branch list is prefetched, and picking a base branch takes you straight to the
name prompt while origin is asked about the base underneath it. Details, and the
rest of the keys, in [docs/picker.md](docs/picker.md).

### Sync

A worktree starts with the tracked files and nothing else. `.gwt/sync.toml` says
what to add, as an ordered list of steps — `link`, `copy`, `run`, `cache`:

```toml
version = 1

[[step]]
type = "link"
src  = "secrets/.env"   # relative to the REPO ROOT
dst  = ".env"           # relative to EACH WORKTREE ROOT

[[step]]
type = "run"
cmd  = "npm ci"
when = ["create"]
```

`.gwt/` sits beside `.bare/` at the repo root, inside no worktree, so git does
not track it: nobody can add a command to your recipe with a push. A `run` step
starts *your* shell the way your terminal does, so rbenv/nvm/mise shims are
where the command expects them. See [docs/sync.md](docs/sync.md).

### Caches

```sh
git wt cache init                    # detect what this project is built with
git wt sync cache target --key Cargo.lock --env CARGO_TARGET_DIR
git wt cache ls                      # buckets, sizes, who uses them
git wt cache gc                      # delete buckets nobody points at
```

A cache step moves the directory out of the worktree into a bucket keyed on the
*contents* of the files that would make sharing unsafe — so two branches share a
warm `target/` until one of them touches the lockfile. See
[docs/cache.md](docs/cache.md).

### Language

English and Japanese (日本語), resolved most-specific-first:

```
--lang  >  $GWT_LANG  >  ~/.config/gwt/config  >  $LC_ALL / $LC_MESSAGES / $LANG  >  English
```

```sh
git wt config lang ja
```

A `ja_JP.UTF-8` locale needs no configuration at all. Column alignment is
computed in terminal cells, so double-width text lines up with ASCII paths.

## Building from source

```sh
cargo build --release --locked --bin git-wt
cargo test --workspace --locked
```

## License

MIT
