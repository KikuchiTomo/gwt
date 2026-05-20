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
curl -fsSL https://raw.githubusercontent.com/KikuchiTomo/gwt/main/install.sh | sh
```

The installer:

1. Downloads the latest release binary for your OS/arch (verifies sha256).
2. Installs to `~/.local/bin/git-wt` (override with `--prefix`).
3. Detects your shell and offers to add a managed block to your rc that
   wires up `PATH` and the `gwt` shell function (lets `Enter` actually `cd`).

Re-running the installer detects an existing version and prompts to update.

```sh
# explicit version, no prompts:
curl -fsSL https://raw.githubusercontent.com/KikuchiTomo/gwt/main/install.sh \
  | sh -s -- --version v0.1.0 --yes
```

Supported targets: **macOS arm64**, **Linux x86_64 (gnu / musl)**, **Windows x86_64**.

### Manual setup

If you used `--no-setup`, add this to your rc by hand:

```sh
export PATH="$HOME/.local/bin:$PATH"
eval "$(git-wt shellinit zsh)"   # or: bash / fish
```

## Usage

| command                      | what it does                                          |
| ---------------------------- | ----------------------------------------------------- |
| `git wt`                     | inline picker (height ~15 lines, fzf-style)           |
| `git wt --display`           | fullscreen live dashboard, auto-refresh               |
| `git wt list`                | tab-separated plain list (for scripts)                |
| `git wt add <branch>`        | create a worktree at `<repo-root>/<branch>`           |
| `git wt remove <path>`       | remove a worktree                                     |
| `git wt review <origin/br>`  | create a tracking worktree for code review            |
| `git wt shellinit <shell>`   | emit shell function for `cd` integration              |

### Picker key bindings

| key                     | action                                              |
| ----------------------- | --------------------------------------------------- |
| `↑` / `k` / `Ctrl-P`    | move up                                             |
| `↓` / `j` / `Ctrl-N`    | move down                                           |
| `Enter`                 | `cd` to the selected worktree                       |
| `d`                     | delete (asks `y/N`)                                 |
| `e`                     | new worktree from a branch name                     |
| `r`                     | review — pick a remote branch, create a worktree    |
| `q` / `Esc`             | close                                               |

## Building from source

```sh
cargo build --release --locked --bin git-wt
```

## License

MIT
