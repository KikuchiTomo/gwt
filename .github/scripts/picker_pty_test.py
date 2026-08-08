#!/usr/bin/env python3
"""End-to-end shell-integration test: does `git wt` open the picker, and does
Enter actually change the shell's directory?

Unit tests cannot see this. The failure mode that shipped in v0.6.3 was a shell
one: both bash and zsh expand aliases while *parsing*, and `eval "$(git-wt
shellinit zsh)"` parses the whole snippet before running any of it, so a user's
`alias gwt='git worktree'` was baked into the body of the `git` wrapper and
`git wt` silently ran the old alias. Only a real shell, in a real pty, with the
real binary catches that — so this drives one.

usage: picker_pty_test.py --bin /path/to/git-wt --shell zsh [--hostile]
"""
import argparse
import os
import pty
import re
import select
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
import fcntl
import time
import unicodedata

ROWS, COLS = 30, 110

# Aliases people really do have. Every one of them used to break something:
# `gwt` hijacked the `git wt` wrapper, `cat` made the chosen path unreadable,
# `mktemp` broke the channel file, `cd` swallowed the move.
HOSTILE_RC = """\
alias gwt='echo OLDALIAS'
alias git='git'
alias cat='echo NOT-A-PATH'
alias mktemp='echo /nonexistent'
alias rm='rm -i'
alias cd='echo NOPE'
"""


def run(cmd, **kw):
    subprocess.run(cmd, check=True, stdout=subprocess.DEVNULL, **kw)


def make_repo(binary, root):
    """A real origin plus a bare-style clone, the layout the picker expects."""
    env = dict(os.environ, GIT_CONFIG_GLOBAL=os.path.join(root, "gitconfig"))
    run(["git", "config", "--global", "user.email", "t@example.com"], env=env)
    run(["git", "config", "--global", "user.name", "T"], env=env)
    run(["git", "config", "--global", "init.defaultBranch", "main"], env=env)
    origin = os.path.join(root, "origin.git")
    seed = os.path.join(root, "seed")
    run(["git", "init", "-q", "--bare", origin], env=env)
    os.makedirs(seed)
    run(["git", "init", "-q"], cwd=seed, env=env)
    with open(os.path.join(seed, "README.md"), "w") as f:
        f.write("hi\n")
    run(["git", "add", "-A"], cwd=seed, env=env)
    run(["git", "commit", "-qm", "init"], cwd=seed, env=env)
    run(["git", "branch", "-M", "main"], cwd=seed, env=env)
    run(["git", "remote", "add", "origin", origin], cwd=seed, env=env)
    run(["git", "push", "-q", "-u", "origin", "main"], cwd=seed, env=env)
    work = os.path.join(root, "work")
    os.makedirs(work)
    run([binary, "clone", origin, "proj"], cwd=work, env=env)
    return os.path.join(work, "proj")


class Screen:
    """Just enough terminal to render the final frame and answer the picker's
    cursor probe — without a DSR reply the inline viewport falls back to the
    alt screen and we would be testing the wrong code path."""

    def __init__(self, fd):
        self.fd = fd
        self.cells = [[" "] * COLS for _ in range(ROWS)]
        self.y = self.x = 0
        self.raw = ""

    def feed(self, text):
        self.raw += text
        i = 0
        while i < len(text):
            c = text[i]
            if c == "\x1b":
                m = re.match(r"\x1b\[([0-9;?]*)([a-zA-Z])", text[i:])
                if not m:
                    i += 1
                    continue
                nums = [int(x) for x in m.group(1).split(";") if x.isdigit()]
                ch = m.group(2)
                if ch == "H":
                    self.y = (nums[0] - 1) if nums else 0
                    self.x = (nums[1] - 1) if len(nums) > 1 else 0
                elif ch == "J":
                    self.cells = [[" "] * COLS for _ in range(ROWS)]
                    self.y = self.x = 0
                elif ch == "K":
                    for x in range(self.x, COLS):
                        self.cells[self.y][x] = " "
                elif ch == "A":
                    self.y = max(0, self.y - (nums[0] if nums else 1))
                elif ch == "B":
                    self.y = min(ROWS - 1, self.y + (nums[0] if nums else 1))
                elif ch == "C":
                    self.x = min(COLS - 1, self.x + (nums[0] if nums else 1))
                elif ch == "D":
                    self.x = max(0, self.x - (nums[0] if nums else 1))
                elif ch == "n" and nums == [6]:
                    os.write(self.fd, ("\x1b[%d;%dR" % (self.y + 1, self.x + 1)).encode())
                i += m.end()
                continue
            if c == "\n":
                self.y = min(ROWS - 1, self.y + 1)
            elif c == "\r":
                self.x = 0
            elif 0 <= self.y < ROWS and 0 <= self.x < COLS and c.isprintable():
                self.cells[self.y][self.x] = c
                self.x += 2 if unicodedata.east_asian_width(c) in "WF" else 1
                if self.x >= COLS:
                    self.x, self.y = 0, min(ROWS - 1, self.y + 1)
            i += 1

    def render(self):
        return "\n".join("".join(r).rstrip() for r in self.cells).rstrip()

    def plain(self):
        """Everything the session ever wrote, minus the styling — the frame is
        drawn as styled spans, so raw bytes never contain a readable title."""
        text = re.sub(r"\x1b\[[0-9;?]*[a-zA-Z]", "", self.raw)
        text = re.sub(r"\x1b[()][A-Z0-9]|\x1b[=>]", "", text)
        return text.replace("\r", "\n")


def drive(shell, rc, keys):
    pid, fd = pty.fork()
    if pid == 0:
        os.environ.update(TERM="xterm-256color", PS1="PROMPT> ", GWT_LANG="en")
        os.environ.pop("TMUX", None)
        if shell == "zsh":
            os.execvp("zsh", ["zsh", "-i", "-f"])
        os.execvp("bash", ["bash", "--norc", "-i"])
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    os.set_blocking(fd, False)
    screen = Screen(fd)

    def pump(seconds):
        end = time.time() + seconds
        while time.time() < end:
            r, _, _ = select.select([fd], [], [], 0.05)
            if not r:
                continue
            try:
                data = os.read(fd, 65536)
            except OSError:
                return
            if not data:
                return
            screen.feed(data.decode("utf-8", "replace"))

    pump(1.5)
    os.write(fd, ("source %s\r" % rc).encode())
    pump(1.5)
    for k in keys:
        os.write(fd, k.encode())
        pump(1.5)
    pump(0.8)
    try:
        os.close(fd)
    except OSError:
        pass
    try:
        os.waitpid(pid, 0)
    except ChildProcessError:
        pass
    return screen


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    ap.add_argument("--shell", required=True, choices=["zsh", "bash"])
    ap.add_argument("--hostile", action="store_true", help="pre-set the aliases users really have")
    args = ap.parse_args()
    binary = os.path.abspath(args.bin)

    root = tempfile.mkdtemp(prefix="gwt-pty-")
    try:
        proj = make_repo(binary, root)
        snippet = subprocess.run(
            [binary, "shellinit", args.shell], check=True, capture_output=True, text=True
        ).stdout
        rc = os.path.join(root, "rc")
        with open(rc, "w") as f:
            f.write('export PATH="%s:$PATH"\n' % os.path.dirname(binary))
            if args.hostile:
                f.write(HOSTILE_RC)
            f.write(snippet)
        # `builtin cd` so the harness's own cd survives the hostile alias; what
        # is under test is the cd the snippet performs, not this one.
        screen = drive(
            args.shell,
            rc,
            ["builtin cd %s\r" % proj, "git wt\r", "\r", "pwd\r"],
        )
        want = os.path.join(proj, "default")
        # macOS hands out /var/folders/... but `pwd` reports /private/var/...
        wants = {want, os.path.realpath(want)}
        plain = screen.plain()
        problems = []
        if "OLDALIAS" in plain:
            problems.append("`git wt` ran the user's old gwt alias instead of the picker")
        if "git wt ·" not in plain:
            problems.append("the picker never drew its frame")
        # The prompt echoes the command, so only a line that IS the path counts.
        if not any(re.search(r"(?m)^%s\s*$" % re.escape(w), plain) for w in wants):
            problems.append("Enter did not cd into %s" % want)
        if problems:
            print("FAIL (%s%s)" % (args.shell, ", hostile aliases" if args.hostile else ""))
            for p in problems:
                print("  · " + p)
            print("--- screen ---")
            print(screen.render())
            print("--- raw ---")
            print(plain)
            return 1
        print("ok: %s%s — picker drew, Enter cd'd into %s"
              % (args.shell, " (hostile aliases)" if args.hostile else "", want))
        return 0
    finally:
        shutil.rmtree(root, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
