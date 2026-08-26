# The picker

`git wt` opens an inline, fzf-style list of the worktrees in the repo. `Enter`
`cd`s into the one under the cursor. `git wt --display` is the same information
as a fullscreen dashboard that refreshes itself.

| key | action |
| --- | --- |
| `↑` / `k` / `Ctrl-P/K` | move up |
| `↓` / `j` / `Ctrl-N/J` | move down |
| `g` / `G` | jump to top / bottom |
| `Enter` | `cd` to the selected worktree |
| `Tab` / `Space` | toggle multi-select |
| `a` | select all / clear all |
| `p` / `P` | pull / push the selected worktree |
| `d` / `D` | delete / force delete (asks `y/N`, acts on the multi-selection) |
| `e` / `n` | new worktree: pick a base branch, then type a name |
| `E` / `N` | same, but also prompts for the directory name |
| `r` | review — pick a remote branch, create a worktree |
| `f` / `/` | filter |
| `?` | every key binding, in the current language |
| `Esc` | clear the selection, then close |
| `q` | close |

The list hides `.bare` and the repo root — neither is somewhere you can work —
and puts `default` first, then the rest alphabetically.

The filter matches the **worktree name and the branch independently**. A
worktree `aaaa-bbbb` holding `fix/aaaa-bbbb` is found by typing either `aaaa` or
`fix`, and the column that matched is the one highlighted. Matching never runs
across the two fields, so a query that exists in neither on its own produces no
phantom hit.

## Nothing waits on git

The rule the picker is built on: **no git call ever happens on the UI thread,
and no screen waits for one before it opens.** A git call costs a process, and a
process costs milliseconds that land squarely between a keypress and the frame
that answers it. So the loop draws what it has, and what it does not have yet is
already on its way.

- The list appears as soon as git can enumerate the worktrees and waits on
  nothing else.
- `REMOTE`, `DIRTY` and `STASH` show `·` while they are counted, because each is
  a `git status` waiting on the disk. The columns are reserved at full width
  from the first frame, so nothing shifts under the cursor as they arrive, and
  `Enter` works before any land.
- The branch list and the trunk are fetched from the moment the picker opens, so
  `n`, `e` and `r` are answered by the next frame rather than by
  `for-each-ref`.
- Picking a base branch opens the **name prompt**, not a spinner; origin is
  asked about the base underneath it (see below).
- Deleting, creating, pulling and pushing all run on workers. The screen keeps
  animating, and the list re-reads itself in the background afterwards.

The principle behind all of it: once you have finished asking for something, you
should be looking at the next screen — not at a spinner standing in front of it.

## Picking a base branch

`e` / `n` open the branch list with the **default branch first**, tagged
`· default`, then local branches alphabetically, then remote ones. The default
branch is `origin/HEAD` where that exists, and the bare repo's own HEAD in a
`git wt clone` layout, which never grows one.

Filtering matches a remote branch on its **branch name**, falling back to the
full ref — so typing `feature` does not put `origin/feature` above the local
branch of the same name. `origin/fea` still finds the remote.

Choosing a base takes you straight to the name prompt. Meanwhile gwt fetches
that one branch and compares it with origin, and the answer arrives underneath
what you are typing:

```
╭ new · from main ───────────────────────────────────────────────────────────╮
│  branching from main  → new branch name will also be the worktree dir name │
│                                                                            │
│ ↓ main is 3 commit(s) behind origin/main                                   │
│   main will be fast-forwarded before the branch is cut                     │
│   pulls in the 'default' worktree, which has it checked out                │
│   fast-forward only — nothing is merged or rebased                         │
│   ^f: branch from it as it stands                                          │
│ branch › feature-x▏                                                        │
╰ type:name  enter:create worktree  esc:cancel ──────────────────────────────╯
```

Branching off a week-old `main` is a mistake that is cheap to avoid here and
expensive to fix once the worktree exists, so the fast-forward is on by default.
`^f` turns it off and back on at any point before you press `Enter`; nothing has
happened yet either way. The fast-forward then runs as the first step of the
creation, and a base that has diverged is reported rather than merged behind
your back — it is fast-forward only, through the worktree holding the branch
when one does and on the branch directly when none does.

Being offline just means the question never comes up.

## Pull and push

`p` runs `git pull --ff-only`. Fast-forward-only is deliberate: a merge or
rebase could leave the worktree mid-conflict with no way to resolve it from the
picker, so divergent history is reported as an error instead.

A bare-style clone copies every origin head into `refs/heads`, so worktrees
usually start with no upstream. The first `p` sets `origin/<branch>` as the
upstream instead of failing with git's "no tracking information" hint.

`P` publishes to the remote, so it always asks first. A branch with no upstream
is pushed with `-u`.

## When something already exists

Creating a worktree over an existing branch or directory is a question, not a
dead end:

```
! local branch 'feature' already exists (origin/feature exists too)
▌ [u] use the existing 'feature' branch        — checks it out in the new worktree
  [R] delete 'feature' and re-pull from origin ⚠ destructive — local-only commits are lost
  [c] cancel
```

If the branch is checked out in another worktree it can be neither adopted nor
deleted, so the picker offers to take you there instead. Every destructive
choice goes through a second `y/N` confirmation.

On the command line the same answers are given up front as flags on `add` /
`new` / `review`:

| flag | what it does |
| --- | --- |
| `--reuse` | check out the existing local branch in the new worktree |
| `--recreate` | **delete** the existing worktree/branch and build it again from the remote (or `<base>`) |
| `--yes` / `-y` | skip the confirmation `--recreate` would otherwise ask for |

`--recreate` prints exactly what it is about to destroy and asks first. The
prompt is read from `/dev/tty`, so it works when stdout is captured; with no
terminal at all it refuses rather than guessing. Without a flag, the error names
the ways out instead of just failing.

## Cloning

`git wt clone` shows what git is doing while it does it:

```
  Counting objects     ████████████████████████ 100%
  Compressing objects  ████████████████████████ 100%
  Receiving objects    ████████████████████████ 100%  128.4 MiB | 11.2 MiB/s, done.
```

git writes that progress to stderr in `\r`-separated updates and only when it is
talking to a terminal, so gwt asks for it explicitly and reads it as it arrives.
Piped or in CI only the phase changes are printed. A clone from a plain local
path stays silent because git copies the objects directly and reports nothing.

A remote with **no commits yet** does not fail: the `default` worktree is
created on the unborn branch, exactly where `git clone` would leave you, and the
reason is spelled out.

## Portable gitdir pointers, and old git

A worktree and the bare repo point at each other. `git wt` writes the worktree's
own `.git` pointer relative, so a checkout survives being mounted at a different
absolute path (a VM share, a container bind mount).

The pointer back — `.bare/worktrees/<id>/gitdir` — can only be relative on
**git 2.48+**, which added `worktree.useRelativePaths`. Older git reads that file
as the worktree's location, reports `../../../default` from `git worktree list`
and marks the worktree *prunable*, which means an ordinary `git gc` may delete
its metadata. On git older than 2.48 gwt therefore writes that half absolute,
and repairs any relative pointer it finds — so a repo created by a newer git
heals the first time you run `git wt` under an older one.
