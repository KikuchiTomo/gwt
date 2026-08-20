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
  | sh -s -- --version v0.7.0 --yes
```

Supported targets: **macOS arm64**, **Linux x86_64 (musl / gnu)**, **Windows x86_64**.

Every release runs its Linux binaries on **Ubuntu 22.04, 24.04 and 26.04**
before publishing — both the musl and the gnu build. On each one, a real bash
and a real zsh open the picker in a pty and press `Enter`, and the release is
blocked unless the shell actually changed directory — also from a shell that
already aliases `gwt`, `git`, `cat`, `mktemp` and `cd`.

On Linux the installer picks the **musl** build. It is statically linked, so it
runs on any distro regardless of age. The `gnu` build is also published and can
be requested explicitly, but it inherits the glibc version of the machine that
built it:

```sh
curl -fsSL .../install.sh | sh -s -- --target x86_64-unknown-linux-gnu
```

If you have an older `git-wt` that fails with ``version `GLIBC_2.xx' not
found``, re-running the installer replaces it with the static build.

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
- Existing aliases are fine. The snippet is written so a `gwt`, `git`, `cat`,
  `mktemp`, `rm` or `cd` alias can neither shadow it nor be pulled into it.

### Uninstall

```sh
curl -fsSL https://raw.githubusercontent.com/KikuchiTomo/gwt/main/install.sh | sh -s -- --uninstall
```

It lists the binary and the rc block it is about to delete, then asks. Add
`--yes` to skip the question, `--prefix DIR` if you installed somewhere custom.
Your worktrees, branches and secret files are never touched, and neither is any
repo's `.gwt/`.

By hand, it is three things:

```sh
rm -f ~/.local/bin/git-wt                      # 1. the binary
                                               # 2. the `# >>> git-wt setup ... <<<`
                                               #    block in your rc file
rm -rf ~/.config/gwt                           # 3. settings (language) — optional
```

The `gwt` and `git` shell functions stay defined in shells that are already
open; start a new shell, or `unset -f gwt git __gwt_run`.

### If `Enter` doesn't change directory

A subprocess cannot change its parent shell's directory, so the `cd` is done by
the shell function from `shellinit`. If that function isn't active, `git wt`
now says so instead of appearing to ignore `Enter`:

```
git wt: shell integration is not active, so the directory was not changed.
        picked: /repo/feature-a
        add to ~/.zshrc:  eval "$(git-wt shellinit zsh)"
        then open a new shell, and use `gwt` or `git wt`.
```

Check with `type gwt` — it must print *shell function*, not *alias*. If your rc
defines a `gwt` alias **after** the git-wt block, move the block below it.

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
| `git wt sync`                        | **interactive** manager for the worktree recipe (below) |
| `git wt sync add/copy/run/rm/ls`     | same thing, non-interactively                           |
| `git wt sync apply`                  | re-apply the recipe to every worktree                   |
| `git wt sync edit`                   | open `.gwt/sync.toml` in `$EDITOR`                      |
| `git wt sync cache <dir>`            | mount a build cache from outside the worktree           |
| `git wt cache ls` / `gc` / `init`    | inspect, collect, and detect build caches               |
| `git wt cache env` / `hooks`         | env vars for the buckets; hooks that keep them current  |
| `git wt relativize [name]`           | make worktree gitdir pointers portable (see below)      |
| `git wt config`                      | show the resolved language and where it came from        |
| `git wt config lang <en\|ja>`         | set the interface language                              |
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
| `?`                     | show every key binding                              |
| `Esc`                   | clear the selection, then close                     |
| `q`                     | close                                               |

`?` opens a full key list in the current language, so nothing depends on
remembering the one-line footer.

The list hides `.bare` and the repo root — neither is somewhere you can work —
and puts `default` first, then the rest alphabetically.

### Picking a base branch

`e` / `n` open the branch list with the **default branch first**, tagged
`· default`, then the rest of the local branches alphabetically, then the remote
ones. It is what most new work is cut from, so it should not have to be hunted
for. The default branch is `origin/HEAD` where that exists, and the bare repo's
own HEAD in a `git wt clone` layout, which never grows one.

Filtering matches a remote branch on its **branch name**, falling back to the
full ref. Scoring `origin/feature` whole let the `/` earn a word-boundary bonus
that the local `feature` could not, so typing `feature` used to put the remote
above the branch of the same name — the wrong one to branch from. Typing
`origin/fea` still finds it.

Then, before asking for a name, `git wt` checks that base against origin:

```
╭ the base branch is behind origin · Y/n ─────────────────────────────────────╮
│ ↓ main is 3 commit(s) behind origin/main                                    │
│                                                                             │
│   pull it before branching?                                                 │
│   pulls in the 'default' worktree, which has it checked out                 │
│   fast-forward only — nothing is merged or rebased                          │
│ pull main from origin first ? Y/n                                           │
╰ y/enter: pull first   n: use it as-is   esc: cancel ────────────────────────╯
```

This is the one prompt that defaults to **yes**: branching off a week-old `main`
is a mistake that is cheap to avoid here and expensive to fix once the worktree
exists. `n` branches from the base exactly as it stands, and `Esc` backs out
altogether. Either way the answer is shown on the name prompt, so nothing
happens silently.

It fetches the one branch to find out, on a worker thread — the picker keeps
animating. Being offline just means the question does not come up. The update
itself is fast-forward only, through the worktree holding the branch when one
does, and on the branch directly when none does; a base that has diverged is
refused rather than merged behind your back.

The filter matches the **worktree name and the branch independently**. A
worktree `aaaa-bbbb` holding `fix/aaaa-bbbb` is found by typing either `aaaa`
or `fix`, and the column that matched is the one highlighted. (Matching is not
allowed to run across the two fields, so a query that exists in neither on its
own will not produce a phantom hit.)

### Pull and push

`p` runs `git pull --ff-only`. Fast-forward-only is deliberate: a merge or
rebase could leave the worktree mid-conflict with no way to resolve it from the
picker, so divergent history is reported as an error instead.

A bare-style clone copies every origin head into `refs/heads`, so worktrees
usually start with no upstream. The first `p` sets `origin/<branch>` as the
upstream instead of failing with git's "no tracking information" hint.

`P` publishes to the remote, so it always asks for confirmation first. A branch
with no upstream is pushed with `-u`.

### Cloning

`git wt clone` shows what git is doing while it does it:

```
  Counting objects     ████████████████████████ 100%
  Compressing objects  ████████████████████████ 100%
  Receiving objects    ████████████████████████ 100%  128.4 MiB | 11.2 MiB/s, done.
```

git writes that progress to stderr in `\r`-separated updates and only when it
is talking to a terminal, so `git wt` asks for it explicitly and reads it as it
arrives. Piped or in CI the bar would be thousands of useless lines, so only the
phase changes are printed. A clone from a plain local path stays silent because
git copies the objects directly and reports nothing to show.

**A remote with no commits** — one you created on the host a minute ago — used
to fail with `fatal: invalid reference: main` and leave a root with no worktree
in it. HEAD names a branch that does not exist as a ref yet. Now the `default`
worktree is created on that unborn branch, exactly where `git clone` would leave
you, and the reason is spelled out:

```
$ git wt clone git@github.com:you/brand-new.git
note: git@github.com:you/brand-new.git has no commits yet — 'default' is on the
      unborn branch 'main'. commit and `git push -u origin main` to start it.
```

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

### Portable gitdir pointers, and old git

A worktree and the bare repo point at each other. `git wt` writes the
worktree's own `.git` pointer relative, so a checkout survives being mounted at
a different absolute path (a VM share, a container bind mount).

The pointer back — `.bare/worktrees/<id>/gitdir` — can only be relative on
**git 2.48+**, which added `worktree.useRelativePaths`. Older git reads that
file as the worktree's location, so `git worktree list` reports
`../../../default` and marks the worktree *prunable*, which means an ordinary
`git gc` may delete its metadata. On git older than 2.48, `git wt` therefore
writes that half absolute, and repairs any relative pointer it finds — so a
repo created by an earlier version heals the first time you run `git wt`.

## Sync: what every worktree needs that git does not carry

A worktree starts with the tracked files and nothing else. The recipe at
`<repo-root>/.gwt/sync.toml` says what to add, as an ordered list of steps:

| kind | what it does |
| --- | --- |
| `link` | symlink one real file into every worktree (this was `secret`) |
| `copy` | copy it instead, for files a tool rewrites in place |
| `run`  | run a command in a worktree, by default only when it is created |
| `cache` | mount a build cache from outside the worktree (see below) |

Order matters, and is preserved: put `.env` in place before the command that
reads it.

```toml
version = 1

[[step]]
type = "link"
src  = "secrets/.env"          # relative to the REPO ROOT
dst  = ".env"                  # relative to EACH WORKTREE ROOT

[[step]]
type = "copy"
src  = "secrets/env.sample"
dst  = ".env"
overwrite = false              # an edited copy is never clobbered
render    = true               # substitute {{branch}} etc.

[[step]]
type = "run"
cmd  = "npm ci"
when = ["create"]              # create | apply | manual
only_if = "package.json"       # only where this exists in the worktree
timeout = "10m"
dir  = "api"                   # run it here, not at the worktree root

[[step]]
type = "run"
cmd  = '''
set -e
pnpm install --frozen-lockfile
pnpm run build'''              # one script, not three steps
dir  = "packages/web"
```

The two path columns use different bases, which is the only fiddly part:

```
src   relative to the REPO ROOT      (where .git / .bare / secrets/ live)
dst   relative to EACH WORKTREE ROOT (created in every worktree)

<repo-root>/
├── .gwt/sync.toml                        <- the recipe
├── secrets/.env                          <- src = secrets/.env
├── default/.env    -> ../secrets/.env    <- dst = .env
└── feature-a/.env  -> ../secrets/.env    <- dst = .env
```

### Why `run` is safe to have

`.gwt/` sits beside `.bare/` at the repo root, inside no worktree, so git does
not track it. Nobody can add a command to your recipe with a push, and `git
pull` cannot bring one in. A recipe is something you wrote on this machine.

`run` steps also stay out of the way of ordinary repair work: `git wt sync
apply` re-links and re-copies without re-running anyone's `npm ci`. Ask for it
with `--run`, or put `apply` in the step's `when`.

The command goes through the shell, from the worktree root, with `GWT_ROOT`,
`GWT_WORKTREE`, `GWT_WORKTREE_NAME` and `GWT_BRANCH` set. Its output is echoed
line by line as it arrives, so a slow install is visibly alive.

`dir` moves it somewhere else in the worktree — the package that actually needs
installing, in a monorepo — and the path is relative to that worktree's root
like every other `dst`.

A `cmd` with newlines in it is **one shell script**, not a line-at-a-time list:
it reaches `sh -c` whole, so `set -e` holds for the rest of it and a variable
set on one line is still there on the next. Writing it as three separate `run`
steps would give you three shells and none of that.

### The interactive manager

`git wt sync` with no subcommand opens a manager built like the worktree picker:

```
╭ git wt sync · 3/3 ─────────────────────────────────────────────────────────╮
│  KIND  SOURCE (<repo-root>/…) or COMMAND  DEST (<worktree>/…)  STATE  APPLIED│
│▌ link  secrets/.env                       .env                 ok     2/2   │
│  copy  secrets/env.sample                 .env.local           ok     2/2   │
│  run   npm ci                             -                    create   -   │
│ recipe /repo/.gwt/sync.toml                                                 │
╰ ↑↓:nav  a:add  e:edit  d:remove  r:apply  f:filter  ?:keys  q:quit ─────────╯
```

| key | action |
| --- | --- |
| `a` | add a step — pick `link` / `copy` / `run` / `cache`, then fill it in |
| `e` | edit the selected step's destination, or its command and directory |
| `d` | remove the step and undo it everywhere (asks `y/N`) |
| `r` | re-apply the recipe to all worktrees |
| `f` / `/` | filter |
| `?` | show every key binding |
| `j/k ↑↓`, `g`/`G` | navigate |
| `q` / `Esc` | quit |

`a` never asks you to type a source path: for `link` and `copy` it lists the
real files under the repo root (worktrees and `.bare` excluded) and you pick
one. Then it asks for the destination with the other root spelled out — which is
the whole src/dst confusion, removed rather than documented. While describing a
`copy`, `^o` toggles overwrite and `^r` toggles render.

A `run` step gets an editor rather than a prompt, because a command is often a
script:

```
╭ sync · command · in every worktree ─────────────────────────────────────────╮
│  what should run inside a new worktree?                                     │
│  1 set -e                                                                   │
│  2 pnpm install --frozen-lockfile                                           │
│  3 pnpm run build▏                                                          │
│                                                                             │
│  only_if and timeout are set in .gwt/sync.toml                              │
│ dir › packages/web   ^d to change                                           │
╰ enter:new line  ^s:save  ^d:dir  ↑↓←→:move  esc:cancel ─────────────────────╯
```

`Enter` starts a new line, so saving is `^s`. `^d` moves to the working
directory and back; leave it empty for the worktree root. Arrow keys, Home/End
and Delete work, so fixing line 1 does not mean retyping line 3.

Under the list, a detail strip names the worktrees behind the `APPLIED` count —
`✓ applied in api, default   ✗ missing in web` — so a partial count tells you
which worktree to go fix. For a `run` step it shows `when`, `timeout`, `dir`,
`only_if` and how many lines the script has instead, since a command leaves no
mark to count; for a `cache` it
shows each bucket with its size and the worktrees sharing it, which is the one
thing the count cannot say.

Creating a worktree runs on a worker thread, so a recipe with an `npm ci` in it
shows the command and its output on the status line while it works instead of
freezing the picker.

### Non-interactively

```sh
git wt sync add  secrets/.env .env                  # link (alias: sync link)
git wt sync add  secrets/gcp.json config/gcp.json
git wt sync copy secrets/env.sample .env --render
git wt sync run  'npm ci' --only-if package.json --timeout 10m
git wt sync ls
git wt sync rm   .env
git wt sync apply [--run]
git wt sync edit                                    # $EDITOR, then re-parsed
```

- `link`, `copy` and `rm` take effect **immediately** in every existing
  worktree. `apply` is only for repairing them, or after creating a source file
  that didn't exist when you registered it.
- `run` is registered, not executed: firing someone's `npm ci` in six worktrees
  because they typed one command would be its own surprise.
- Source paths may be absolute as long as they are inside the repo root, so
  shell tab-completion works.
- `rm` names a step by its destination, its source, or its command line.
- `rm` removes only a symlink still pointing at that source, or a copy still
  byte-identical to it. Anything else is left alone and reported with the
  reason. The source file itself is never deleted.

`sync ls` shows both bases, whether the source exists, and how many worktrees
currently carry each step:

```
KIND  SOURCE (<repo-root>/…) or COMMAND  DEST (<worktree>/…)  STATE    APPLIED
----  ---------------------------------  -------------------  -------  -------
link  secrets/.env                       .env                 ok       2/2
copy  secrets/env.sample                 .env.local           MISSING  0/2
run   npm ci                             -                    create   -
```

## Build caches that outlive the worktree

A worktree is a fresh directory, so every build system starts cold. Six
worktrees of one repo means six `target/` directories and six full builds — and
deleting a worktree throws its cache away with it.

The obvious fix is to share one cache, and the obvious fix is wrong: two
branches with different lockfiles must not write to the same `node_modules`.
So a cache step moves the directory out of the worktree into a **bucket**, and
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

### What it does to your working tree

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

### The part symlinks cannot fix

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

### Keeping the binding current

A keyed bucket is chosen from a lockfile, so switching branches can invalidate
the choice. `git wt sync apply` re-checks and re-points every mount, and

```sh
git wt cache hooks
```

installs `post-checkout` and `post-merge` in the bare repo's `hooks/` — shared
by every worktree — so it happens on its own.

### Upgrading from `git wt secret`

Nothing to do. `secret` and `relink` still work — they print the new name and
run `sync` and `sync apply`. An existing `secrets/manifest` is read as a list of
`link` steps, and the first change you make writes `.gwt/sync.toml` with those
rows carried over. Comments in a hand-edited `sync.toml` survive edits made from
the TUI.

## Language

The interface speaks English and Japanese (日本語).

```sh
git wt config lang ja     # persist to ~/.config/gwt/config
git wt config             # show what is in effect, and why
git wt --lang en list     # one-off override
```

Resolution order, most specific first:

```
--lang  >  $GWT_LANG  >  ~/.config/gwt/config  >  $LC_ALL / $LC_MESSAGES / $LANG  >  English
```

If your locale is already `ja_JP.UTF-8`, Japanese is picked up with no
configuration at all. Column alignment is computed in terminal cells, so
double-width text lines up with ASCII paths.

## Building from source

```sh
cargo build --release --locked --bin git-wt
cargo test --workspace --locked
```

## License

MIT
