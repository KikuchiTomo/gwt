#!/usr/bin/env python3
"""End-to-end test: does a `run` step get the toolchain your rc sets up, when
there is no terminal for the shell to be interactive on?

No Rust test can answer that. It takes the real shell, reading the real rc, with
no tty in sight — which is precisely the case that kept breaking:

  * `sh -c` (pre-0.8.1) read no rc at all, so rbenv/nvm/asdf shims never joined
    `PATH`;
  * the login-shell fallback that replaced it does not help either, because zsh
    reads `~/.zshrc` only when interactive and bash skips `~/.bashrc` for a
    login shell. On macOS it is actively worse: `/etc/zprofile` runs
    `path_helper`, which pushes the inherited shims *behind* `/usr/bin`, so
    `bundle` becomes `/usr/bin/bundle` on the system Ruby.

So this drives the real binary with its output on a pipe — a script, a hook, CI,
`| tee` — and checks which `toolchain` the step actually found: the one the rc
puts on `PATH`, or the decoy that was there already.

The other direction matters just as much: `shell = "login"` is the documented
way to skip an interactive rc that is too slow or too chatty, so it must still
skip it.

usage: sync_rc_test.py --bin target/debug/git-wt --shell zsh
"""
import argparse
import os
import shutil
import subprocess
import sys
import tempfile

# What a version manager's setup line amounts to, plus something on stdout: the
# rc runs before the step, so anything it prints would land in the step's own
# output if it were not dropped.
RC = """\
echo 'rc noise on stdout'
echo 'rc noise on stderr' >&2
export PATH="$HOME/shims:$PATH"
"""

RC_FILE = {"zsh": ".zshrc", "bash": ".bashrc"}


def run(cmd, **kw):
    """Run with the output on a pipe — never a tty. That is the whole point."""
    return subprocess.run(
        cmd, check=True, capture_output=True, text=True, **kw
    )


def tool(path, says):
    with open(path, "w") as f:
        f.write(f"#!/bin/sh\necho {says}\n")
    os.chmod(path, 0o755)


def make_repo(binary, root, env):
    """A real origin plus the bare-style clone `git wt` expects."""
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


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    ap.add_argument("--shell", required=True, choices=sorted(RC_FILE))
    args = ap.parse_args()

    shell = shutil.which(args.shell)
    if not shell:
        sys.exit(f"{args.shell} is not installed")
    binary = os.path.abspath(args.bin)

    root = tempfile.mkdtemp(prefix="gwt-sync-rc-")
    home = os.path.join(root, "home")
    shims = os.path.join(home, "shims")
    system = os.path.join(root, "system")
    for d in (home, shims, system):
        os.makedirs(d)
    with open(os.path.join(home, RC_FILE[args.shell]), "w") as f:
        f.write(RC)
    # The two answers the step can come back with: the rc's toolchain, or the
    # one that was on `PATH` before any shell started — `/usr/bin/bundle` in the
    # bug report, and the wrong one.
    tool(os.path.join(shims, "toolchain"), "shim")
    tool(os.path.join(system, "toolchain"), "system")

    env = dict(
        os.environ,
        HOME=home,
        SHELL=shell,
        PATH=os.pathsep.join([system, os.environ["PATH"]]),
        GIT_CONFIG_GLOBAL=os.path.join(home, "gitconfig"),
    )
    # A machine-wide preference would answer the question for us.
    env.pop("GWT_SYNC_SHELL", None)
    # Whatever is running this is not the project being set up.
    for pinned in ("BUNDLE_GEMFILE", "GEM_HOME", "RUBYOPT", "VIRTUAL_ENV"):
        env.pop(pinned, None)

    proj = make_repo(binary, root, env)
    wt = os.path.join(proj, "default")

    # `auto` is the default: whatever a terminal would have set up.
    run([binary, "sync", "run", "toolchain > probe-auto.txt"], cwd=wt, env=env)
    # `login` asked for the login rc and nothing more, and must keep meaning it.
    run(
        [binary, "sync", "run", "toolchain > probe-login.txt", "--shell", "login"],
        cwd=wt,
        env=env,
    )
    applied = run([binary, "sync", "apply", "--run"], cwd=wt, env=env)

    def probe(name):
        with open(os.path.join(wt, name)) as f:
            return f.read().strip()

    failures = []
    if probe("probe-auto.txt") != "shim":
        failures.append(
            f"the rc on {RC_FILE[args.shell]} never ran: a `run` step found the "
            f"{probe('probe-auto.txt')!r} toolchain, not the one the rc puts on PATH"
        )
    if probe("probe-login.txt") != "system":
        failures.append(
            "`shell = \"login\"` read the interactive rc anyway: it is the way "
            "out of a slow or chatty one, so it has to stay a way out"
        )
    noise = [
        line
        for line in (applied.stdout + applied.stderr).splitlines()
        if "rc noise" in line
    ]
    if noise:
        failures.append(f"the rc's own output leaked into the step's: {noise}")

    if failures:
        print(f"FAIL ({args.shell})", file=sys.stderr)
        for f in failures:
            print(f"  - {f}", file=sys.stderr)
        sys.exit(1)
    print(f"ok ({args.shell}): the rc ran, `login` still skipped it, no noise")
    shutil.rmtree(root, ignore_errors=True)


if __name__ == "__main__":
    main()
