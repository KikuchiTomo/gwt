# gwt — git worktree, the comfy way

`git wt` is a cross-platform Rust replacement for ad-hoc `git worktree`
wrappers. It ships as a `git` subcommand and adds two TUIs on top of plain
worktree management:

- `git wt` — an **inline, fzf-style picker** of the worktrees in the current
  repo. Pick one with `Enter` and the shell `cd`s into it.
- `git wt --display` — a **fullscreen live dashboard** of every worktree
  with branch, status, and the one you're currently in.

## Install

```sh
curl -fsSL "https://raw.githubusercontent.com/KikuchiTomo/gwt/main/install.sh?cb=$(date +%s)" | sh
```

The `?cb=...` busts `raw.githubusercontent.com`'s CDN cache; drop it once you
trust the cache state (default TTL is ~5 minutes).

The installer:

1. Downloads the latest release binary for your OS/arch (verifies sha256).
2. Installs to `~/.local/bin/git-wt` (override with `--prefix`).
3. Detects your shell and offers to add a managed block to your rc that
   wires up `PATH` and the `gwt` shell function (lets `Enter` actually `cd`).
4. Verifies the binary runs and tells you exactly what to `source`.

Re-running the installer detects an existing version and prompts to update.

```sh
# explicit version, no prompts:
curl -fsSL https://raw.githubusercontent.com/KikuchiTomo/gwt/main/install.sh \
  | sh -s -- --version v0.5.1 --yes
```

Supported targets: **macOS arm64**, **Linux x86_64 (gnu / musl)**, **Windows x86_64**.

### After installing

The rc block only takes effect in a **new** shell. In the shell you ran the
installer from, either open a new terminal or `source` your rc — otherwise
`git-wt` is genuinely not on `PATH` yet:

```sh
source ~/.zshrc      # or ~/.bashrc, ~/.bash_profile, ~/.config/fish/config.fish
```

### Manual setup

If you used `--no-setup`, add this to your rc by hand. The `command -v` guard
matters: without it, every new shell prints `git-wt: command not found` when the
binary is missing or moved.

```sh
export PATH="$HOME/.local/bin:$PATH"
command -v git-wt >/dev/null 2>&1 && eval "$(git-wt shellinit zsh)"   # or: bash
```

fish:

```fish
set -gx PATH $HOME/.local/bin $PATH
type -q git-wt; and git-wt shellinit fish | source
```

Notes:

- On macOS, bash reads `~/.bash_profile` (login shell), not `~/.bashrc`.
- `PREFIX` / `GWT_PREFIX` in your environment override the install prefix. The
  installer prints which one it used, so check that line if `git-wt` lands
  somewhere unexpected.

## Usage

| command                              | what it does                                            |
| ------------------------------------ | ------------------------------------------------------- |
| `git wt`                             | inline picker (height ~15 lines, fzf-style)             |
| `git wt --display`                   | fullscreen live dashboard, auto-refresh                 |
| `git wt clone <url> [dir]`           | clone into a bare-style root + a `default` worktree     |
| `git wt list` / `ls`                 | rich table: branch, ahead/behind, dirty, stash          |
| `git wt add <branch> <name>`         | adopt an existing branch as a worktree at `<name>`      |
| `git wt new <base> <branch> <name>`  | create a new branch from `<base>` in worktree `<name>`  |
| `git wt review <branch>`             | fetch `origin/<branch>` and make a tracking worktree    |
| `git wt remove <name>` / `rm`        | remove worktree `<name>` and delete its local branch    |
| `git wt check <branch> [--fetch]`    | compare local `<branch>` against `origin/<branch>`      |
| `git wt secret`                      | **interactive** secrets manager (see below)             |
| `git wt secret add/rm/ls`            | same thing, non-interactively                           |
| `git wt relink`                      | re-apply secret links to every worktree                 |
| `git wt relativize [name]`           | convert worktree gitdir pointers to relative paths      |
| `git wt shellinit <shell>`           | emit the shell function for `cd` integration            |

### Picker key bindings

| key                     | action                                              |
| ----------------------- | --------------------------------------------------- |
| `↑` / `k` / `Ctrl-P/K`  | move up                                             |
| `↓` / `j` / `Ctrl-N/J`  | move down                                           |
| `g` / `G`               | jump to top / bottom                                |
| `Enter`                 | `cd` to the selected worktree                       |
| `Tab` / `Space`         | toggle multi-select                                 |
| `a`                     | select all / clear all                              |
| `p`                     | **pull** the selected worktree (fast-forward only)  |
| `P`                     | **push** the selected worktree (asks first)         |
| `d`                     | delete (asks `y/N`; acts on the multi-selection)    |
| `D`                     | force delete (asks `y/N`)                           |
| `e` / `n`               | new worktree: pick a base branch, then type a name  |
| `E` / `N`               | same, but also prompts for the directory name       |
| `r`                     | review — pick a remote branch, create a worktree    |
| `f` / `/`               | filter                                              |
| `Esc`                   | clear the selection, then close                     |
| `q`                     | close                                               |

### Pull and push

`p` runs `git pull --ff-only`. Fast-forward-only is deliberate: a merge or
rebase could leave the worktree mid-conflict with no way to resolve it from the
picker, so divergent history is reported as an error instead.

A bare-style clone copies every origin head into `refs/heads`, so worktrees
usually start with no upstream. The first `p` sets `origin/<branch>` as the
upstream instead of failing with git's "no tracking information" hint.

`P` publishes to the remote, so it always asks for confirmation first. A branch
with no upstream is pushed with `-u`.

### When something already exists

Creating a worktree used to fail outright if the branch or the directory was
already taken. Now the picker asks what you want to do:

**The directory already exists**

```
! /repo/feat already exists
▌ [g] go to 'feat'                      — cd into the worktree that is already there
  [R] delete 'feat' and re-create it    ⚠ destructive — discards it, then re-pulls origin
  [c] cancel                            — leave everything as it is
```

**The local branch already exists**

```
! local branch 'feature' already exists (origin/feature exists too)
▌ [u] use the existing 'feature' branch      — checks it out in the new worktree
  [R] delete 'feature' and re-pull from origin  ⚠ destructive — local-only commits are lost
  [c] cancel
```

If the branch is checked out in another worktree it can be neither adopted nor
deleted, so the picker offers to take you there instead.

Every destructive choice goes through a second `y/N` confirmation before
anything is removed.

**On the command line**, the same answers are given up front as flags on
`add` / `new` / `review`:

| flag | what it does |
| --- | --- |
| `--reuse` | check out the existing local branch in the new worktree |
| `--recreate` | **delete** the existing worktree/branch and build it again from the remote (or `<base>`) |
| `--yes` / `-y` | skip the confirmation that `--recreate` would otherwise ask for |

`--recreate` prints exactly what it is about to destroy and asks before doing
it. The prompt is read from `/dev/tty`, so it still works when stdout is being
captured; with no terminal at all it refuses rather than guessing:

```
$ git wt new main feature wip --recreate
about to DELETE and re-create:
  worktree  /repo/wip
  branch    feature (local commits not on origin are lost)
proceed? [y/N]
```

Without a flag, the error names the ways out instead of just failing:

```
$ git wt new main feature wip
git wt: branch 'feature' already exists
  · --reuse      check out the existing branch in wip
  · --recreate   delete it and re-create from origin (asks first)
  · or run `git wt` and choose interactively
```

## Secrets

The real file lives **once** in the repo root; every worktree gets a symlink to
it. The two paths are relative to different places, which is the only fiddly
part:

```
SOURCE            relative to the REPO ROOT      (where .git / .bare / secrets/ live)
DEST_IN_WORKTREE  relative to EACH WORKTREE ROOT (created in every worktree)

<repo-root>/
├── secrets/.env                          <- SOURCE            = secrets/.env
├── default/.env    -> ../secrets/.env    <- DEST_IN_WORKTREE  = .env
└── feature-a/.env  -> ../secrets/.env    <- DEST_IN_WORKTREE  = .env
```

### The interactive manager

`git wt secret` with no subcommand opens a manager built like the worktree
picker:

```
╭ git wt secret · 2/2 ──────────────────────────────────────────────╮
│  SOURCE (<repo-root>/…)   DEST (<worktree>/…)   SOURCE   LINKED   │
│▌ secrets/.env             .env                  ok       2/2      │
│  secrets/gcp.json         config/gcp.json       ok       2/2      │
│ manifest /repo/secrets/manifest                                   │
╰ j/k ↑↓:nav  a:add  d:remove  r:relink  f:filter  q:quit ──────────╯
```

| key | action |
| --- | --- |
| `a` | add a mapping — **fuzzy-pick the real file**, then type the destination |
| `d` | remove the mapping and unlink it everywhere (asks `y/N`) |
| `r` | relink all worktrees |
| `f` / `/` | filter |
| `j/k ↑↓`, `g`/`G` | navigate |
| `q` / `Esc` | quit |

`a` never asks you to type the source: it lists the real files under the repo
root (worktrees and `.bare` excluded) and you pick one. Then it asks for the
destination with the other root spelled out — which is the whole src/dst
confusion, removed rather than documented.

### Non-interactively

```sh
git wt secret add secrets/.env .env
git wt secret add secrets/gcp.json config/gcp.json
git wt secret ls
git wt secret rm secrets/.env
```

- `add` and `rm` take effect **immediately** in every existing worktree — no
  `relink` needed. `relink` is only for repairing links, or after creating a
  source file that didn't exist when you registered it.
- `add` accepts an absolute path as long as it is inside the repo root, so shell
  tab-completion works.
- `rm` removes only symlinks that still point at that source. A real file
  sitting at the destination is left alone and reported.
- The source file itself is never deleted.

`secret ls` shows both bases, whether the source exists, and how many worktrees
currently carry the link:

```
SOURCE (<repo-root>/…)    DEST (<worktree>/…)    SOURCE    LINKED
------------------------  ---------------------  --------  ------
secrets/.env              .env                   ok        2/2
secrets/gcp.json          config/gcp.json        MISSING   0/2
```

## Building from source

```sh
cargo build --release --locked --bin git-wt
cargo test --workspace --locked
```

## License

MIT
