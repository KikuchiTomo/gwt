// A subprocess can't change the parent shell's cwd, so we ship shell wrappers
// that run `git wt`, read the chosen worktree path it leaves in $GWT_CD_FILE, and
// cd into it. We must NOT capture stdout via $(...): the inline picker's crossterm
// cursor probe (ESC[6n) is written to stdout, and capturing it would break the
// inline viewport and leak escape bytes into the path. The file channel keeps
// stdout attached to the terminal. `gwt` is the explicit form; `git` is overridden
// so bare `git wt` (the picker) also performs the cd.

pub fn print(shell: &str) {
    let snippet = match shell {
        "fish" => FISH,
        "zsh" | "bash" => BASH,
        _ => BASH,
    };
    print!("{snippet}");
}

const BASH: &str = r#"gwt() {
  # Do NOT capture stdout: the picker keeps it attached to the terminal for its
  # cursor probe and writes the chosen path to $GWT_CD_FILE instead.
  local __gwt_cd
  __gwt_cd="$(mktemp "${TMPDIR:-/tmp}/gwt-cd.XXXXXX")" || return
  GWT_CD_FILE="$__gwt_cd" command git wt "$@"
  local __gwt_rc=$?
  local __gwt_dir
  __gwt_dir="$(cat "$__gwt_cd" 2>/dev/null)"
  rm -f "$__gwt_cd"
  if [ "$__gwt_rc" -eq 0 ] && [ -n "$__gwt_dir" ] && [ -d "$__gwt_dir" ]; then
    cd "$__gwt_dir" || return
  fi
  return "$__gwt_rc"
}

git() {
  # Only the bare `git wt` form (the picker) needs cd integration; everything
  # else — including `git wt list`, `git wt new …`, plain `git status`, etc. —
  # falls straight through to the real binary so we don't surprise users.
  if [ "$#" = "1" ] && [ "$1" = "wt" ]; then
    gwt
    return
  fi
  command git "$@"
}
"#;

const FISH: &str = r#"function gwt
  # Do NOT capture stdout: the picker keeps it attached to the terminal for its
  # cursor probe and writes the chosen path to $GWT_CD_FILE instead.
  set -l __gwt_cd (mktemp (test -n "$TMPDIR"; and echo $TMPDIR; or echo /tmp)/gwt-cd.XXXXXX)
  env GWT_CD_FILE=$__gwt_cd git wt $argv
  set -l __gwt_rc $status
  set -l __gwt_dir (cat $__gwt_cd 2>/dev/null)
  rm -f $__gwt_cd
  if test $__gwt_rc -eq 0 -a -n "$__gwt_dir" -a -d "$__gwt_dir"
    cd $__gwt_dir
  end
  return $__gwt_rc
end

function git
  if test (count $argv) -eq 1 -a "$argv[1]" = "wt"
    gwt
    return
  end
  command git $argv
end
"#;
