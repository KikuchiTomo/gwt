# Installing

```sh
curl -fsSL https://raw.githubusercontent.com/KikuchiTomo/gwt/main/install.sh | sh
```

The installer downloads the release binary for your OS/arch (verifying its
sha256), puts it at `~/.local/bin/git-wt`, and offers to add a managed block to
your shell rc that wires up `PATH` and the `gwt` shell function. Re-running it
detects an existing install and prompts to update.

```sh
# a specific tag, no prompts
curl -fsSL https://raw.githubusercontent.com/KikuchiTomo/gwt/main/install.sh \
  | sh -s -- --version <tag> --yes
```

If `raw.githubusercontent.com` serves you a stale script, append
`?cb=$(date +%s)` to the URL to bust its CDN cache (TTL is ~5 minutes).

## Targets

macOS arm64, Linux x86_64 (musl and gnu), Windows x86_64.

On Linux the installer picks the **musl** build: it is statically linked, so it
runs on any distro regardless of age. The `gnu` build is published too and
inherits the glibc version of the machine that built it, so ask for it only if
you need it:

```sh
curl -fsSL .../install.sh | sh -s -- --target x86_64-unknown-linux-gnu
```

An older `git-wt` failing with ``version `GLIBC_2.xx' not found`` is fixed by
re-running the installer, which replaces it with the static build.

Every release is smoke-tested before publishing: a real bash and a real zsh open
the picker in a pty on several Ubuntu releases and press `Enter`, and the
release is blocked unless the shell actually changed directory — including from
a shell that already aliases `gwt`, `git`, `cat`, `mktemp` and `cd`.

## After installing

The rc block only takes effect in a **new** shell. In the one you ran the
installer from, open a new terminal or source your rc:

```sh
source ~/.zshrc      # or ~/.bashrc, ~/.bash_profile, ~/.config/fish/config.fish
```

## Manual setup

With `--no-setup`, add this by hand. The `command -v` guard matters: without it
every new shell prints `git-wt: command not found` when the binary is missing.

```sh
export PATH="$HOME/.local/bin:$PATH"
command -v git-wt >/dev/null 2>&1 && eval "$(git-wt shellinit zsh)"   # or: bash
```

fish:

```fish
set -gx PATH $HOME/.local/bin $PATH
type -q git-wt; and git-wt shellinit fish | source
```

- On macOS, bash reads `~/.bash_profile` (login shell), not `~/.bashrc`.
- `PREFIX` / `GWT_PREFIX` override the install prefix. The installer prints
  which one it used.
- Existing aliases are fine: the snippet is written so a `gwt`, `git`, `cat`,
  `mktemp`, `rm` or `cd` alias can neither shadow it nor be pulled into it.

## If `Enter` doesn't change directory

A subprocess cannot change its parent shell's directory, so the `cd` is done by
the shell function from `shellinit`. If that function isn't active, `git wt`
says so rather than appearing to ignore `Enter`:

```
git wt: shell integration is not active, so the directory was not changed.
        picked: /repo/feature-a
        add to ~/.zshrc:  eval "$(git-wt shellinit zsh)"
        then open a new shell, and use `gwt` or `git wt`.
```

Check with `type gwt` — it must print *shell function*, not *alias*. If your rc
defines a `gwt` alias **after** the git-wt block, move the block below it.

## Uninstall

```sh
curl -fsSL https://raw.githubusercontent.com/KikuchiTomo/gwt/main/install.sh | sh -s -- --uninstall
```

It lists the binary and the rc block it is about to delete, then asks. `--yes`
skips the question, `--prefix DIR` points at a custom install. Your worktrees,
branches and secret files are never touched, and neither is any repo's `.gwt/`.

By hand it is three things: `rm -f ~/.local/bin/git-wt`, delete the
`# >>> git-wt setup ... <<<` block from your rc, and optionally
`rm -rf ~/.config/gwt`. The `gwt` and `git` shell functions stay defined in
shells that are already open; start a new shell, or
`unset -f gwt git __gwt_run`.
