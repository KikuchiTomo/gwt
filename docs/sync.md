# Sync: what every worktree needs that git does not carry

A worktree starts with the tracked files and nothing else. The recipe at
`<repo-root>/.gwt/sync.toml` says what to add, as an ordered list of steps:

| kind | what it does |
| --- | --- |
| `link` | symlink one real file into every worktree |
| `copy` | copy it instead, for files a tool rewrites in place |
| `run` | run a command in a worktree, by default only when it is created |
| `cache` | mount a build cache from outside the worktree (see [caches](cache.md)) |

Order matters and is preserved: put `.env` in place before the command that
reads it. It is editable after the fact — `J` / `K` in `git wt sync`, or
`git wt sync move <from> <to>` — because the step you needed first is rarely the
one you thought of first.

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

## Running it from a worktree

Every `git wt` command works from anywhere inside the repo — the root, a
worktree, or a directory inside one. The root is found the way git finds it, so
being one `cd` in is the normal case rather than an error. A relative `src` is
read from where you are standing (`../secrets/.env` from a worktree names the
same file as `secrets/.env` from the root) and is stored root-relative either
way, so the recipe reads the same however it was written.

## Why `run` is safe to have

`.gwt/` sits beside `.bare/` at the repo root, inside no worktree, so git does
not track it. Nobody can add a command to your recipe with a push, and `git
pull` cannot bring one in. A recipe is something you wrote on this machine.

`run` steps also stay out of the way of ordinary repair work: `git wt sync
apply` re-links and re-copies without re-running anyone's `npm ci`. Ask for it
with `--run`, or put `apply` in the step's `when`.

The command runs with `GWT_ROOT`, `GWT_WORKTREE`, `GWT_WORKTREE_NAME` and
`GWT_BRANCH` set, plus `GWT_SYNC=1` — a marker for an rc file that wants to keep
its heavier startup for terminals a person is actually looking at.

A `cmd` with newlines in it is **one shell script**, not a line-at-a-time list:
it reaches the shell whole, so `set -e` holds for the rest of it and a variable
set on one line is still there on the next.

## It runs the way you would run it

A directory that pins its toolchain — rbenv, nodenv, pyenv, nvm, asdf, mise —
does it from an *interactive* rc file. `sh -c "bundle install"` reads no rc at
all, so the shims never reach `PATH`, and `nvm`, which is a shell function and
nothing else, does not exist. That is the old "works when I type it, fails from
the recipe" bug, and it is why a `run` step starts **your** shell:

- **Your `$SHELL`, with the flags your terminal uses** — interactive, and login
  too on macOS, where terminals start login shells. So `~/.zshrc`, `~/.bashrc`
  and `config.fish` run, exactly as they do in a new tab.
- **The environment another project pinned is dropped.** Launch gwt from a shell
  with a virtualenv active, or from inside `bundle exec`, and `BUNDLE_GEMFILE`,
  `GEM_HOME`, `RUBYOPT`, `RBENV_VERSION`, `VIRTUAL_ENV` and friends would
  otherwise outrank the `.ruby-version` in the worktree being set up. A new
  terminal never has them, so neither does the command.
- **It `cd`s in rather than starting there**, so `chpwd` hooks fire and direnv,
  mise and the `.nvmrc` switchers get their chance to notice where they are.

With no terminal in sight — a script, a git hook, CI, output on its way into a
pipe — an interactive shell has nothing to be interactive on, so a login shell
runs instead, and it sources your interactive rc itself (`~/.zshrc`, or
`$ZDOTDIR/.zshrc`; `~/.bashrc` for bash) before it `cd`s in. A login shell alone
would not: zsh reads `~/.zshrc` only when it is interactive, and bash skips
`~/.bashrc` outright — and on macOS `/etc/zprofile` runs `path_helper`, which
pushes the rbenv shims we inherited *behind* `/usr/bin`. That combination is how
`bundle install` ends up as `/usr/bin/bundle` on the system Ruby, failing with

```
Could not find 'bundler' (2.5.6) required by your Gemfile.lock.
```

while the same line typed in the same directory works. Whatever the rc prints on
its way through is dropped, so it cannot get mixed into the step's own output.

Per step, `shell` says otherwise:

```toml
[[step]]
type = "run"
cmd = "npm ci"
shell = "auto"    # default: your shell, the way your terminal starts it
# shell = "login"                # login shell only — skip a slow interactive rc
# shell = "posix"                # plain `sh -c`, environment untouched
# shell = "bash -euo pipefail"   # or name one; `-c` is appended if missing
```

`GWT_SYNC_SHELL` sets the same thing for one machine, and applies only to steps
that did not choose for themselves.

## The interactive manager

`git wt sync` with no subcommand opens a manager built like the worktree picker:

```
╭ git wt sync · 3/3 ───────────────────────────────────────────────────────────╮
│  #  KIND  SOURCE (<repo-root>/…) or COMMAND  DEST (<worktree>/…)  STATE APPLIED│
│▌ 1  link  secrets/.env                       .env                 ok    2/2  │
│  2  copy  secrets/env.sample                 .env.local           ok    2/2  │
│  3  run   npm ci                             -                    create  -  │
│ recipe /repo/.gwt/sync.toml                                                  │
╰ ↑↓:nav  J/K:reorder  a:add  e:edit  d:remove  r:apply  f:filter  ?:keys  q ──╯
```

| key | action |
| --- | --- |
| `a` | add a step — pick `link` / `copy` / `run` / `cache`, then fill it in |
| `e` | edit the selected step's destination, or its command and directory |
| `J` / `K`, `shift`+`↑↓` | move the step later / earlier in the recipe |
| `d` | remove the step and undo it everywhere (asks `y/N`) |
| `r` | re-apply the recipe to all worktrees |
| `f` / `/` | filter |
| `?` | show every key binding |
| `j/k ↑↓`, `g`/`G` | navigate |
| `q` / `Esc` | quit |

The `#` column is the position in the recipe, which is the order the steps run
in — and the number `git wt sync move` takes. With a filter on, `J`/`K` step
over the neighbour you can *see*, which is the only reading of "move it down"
that matches what is on the screen.

`a` never asks you to type a source path: for `link` and `copy` it lists the
real files under the repo root (worktrees and `.bare` excluded) and you pick one.
Then it asks for the destination with the other root spelled out — which is the
whole src/dst confusion, removed rather than documented. While describing a
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
directory and back; leave it empty for the worktree root.

Under the list, a detail strip names the worktrees behind the `APPLIED` count —
`✓ applied in api, default   ✗ missing in web` — so a partial count tells you
which worktree to go fix. A `run` step shows `when`, `timeout`, `dir`, `only_if`
and how many lines the script has instead; a `cache` shows each bucket with its
size and the worktrees sharing it.

## When the recipe runs a command, it gets the screen

A worktree whose recipe only links and copies is built on a worker thread: the
picker keeps its spinner and the whole thing is over in a moment.

A recipe with an `npm ci` in it is a different animal. It runs for minutes and
prints more in a second than a status line holds — and the interesting part is
never the last line. So the TUI gets out of the way: the viewport comes down,
the command runs with the real terminal (its own colors, its own progress bars,
and a keyboard to answer with if it stops to ask), and the picker is drawn again
underneath the output when it finishes.

```
creating feature-x
· npm ci
added 412 packages in 9s
  ✓ npm ci (9s)
```

Nothing is hidden: a step that failed holds the screen until you press enter,
and so does every run under the alt-screen fallback (tmux), where the output
would otherwise be wiped by the redraw. On the command line a command gets the
terminal too; piped into a file or run by CI, gwt echoes the output line by line
as it arrives instead. A command's own stdout is redirected to stderr on the
way, because stdout is how `cd "$(git wt)"` learns where to go.

## Non-interactively

```sh
git wt sync add  secrets/.env .env                  # link (alias: sync link)
git wt sync copy secrets/env.sample .env --render
git wt sync run  'npm ci' --only-if package.json --timeout 10m
git wt sync run  'make setup' --shell posix          # plain `sh -c`
git wt sync ls
git wt sync move 3 1                                # 3rd step now runs first
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
- `rm` names a step by its destination, its source, or its command line, and
  removes only a symlink still pointing at that source or a copy still
  byte-identical to it. Anything else is left alone and reported with the
  reason. The source file itself is never deleted.

Comments in a hand-edited `sync.toml` travel with the step they were written
above, including across a `move`.

## Coming from `git wt secret`

Nothing to do. `secret` and `relink` still work — they print the new name and
run `sync` and `sync apply`. An existing `secrets/manifest` is read as a list of
`link` steps, and the first change you make writes `.gwt/sync.toml` with those
rows carried over.
