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

const BASH: &str = r#"# `gwt` and `git` are ordinary words, so either may already be an alias. In zsh
# an existing alias turns `name() {` into "defining function based on alias" and
# a parse error, which lands in the middle of shell startup. The `function`
# keyword suppresses that alias expansion in both bash and zsh, and the unalias
# stops a leftover `gwt` alias from shadowing the function once it is defined.
# `git` is left alone: overriding someone else's git alias is not ours to do,
# and `gwt` keeps working either way.
unalias gwt 2>/dev/null || true

function gwt {
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

function git {
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

#[cfg(test)]
mod tests {
    use super::{BASH, FISH};

    /// A `gwt` or `git` alias in the user's rc turns `name() {` into a parse
    /// error in zsh ("defining function based on alias"), which lands in the
    /// middle of shell startup. The `function` keyword is immune in both bash
    /// and zsh, so the POSIX form must never come back.
    #[test]
    fn defines_functions_with_the_function_keyword() {
        for name in ["gwt", "git"] {
            assert!(
                BASH.contains(&format!("function {name} {{")),
                "{name} must be defined with the `function` keyword"
            );
            assert!(
                !BASH.contains(&format!("\n{name}() {{")),
                "{name}() {{ ... }} breaks when `{name}` is already an alias"
            );
        }
    }

    /// Defining the function is not enough: an alias still shadows it at call
    /// time, so `gwt` would silently keep running the old alias.
    #[test]
    fn clears_a_stale_gwt_alias() {
        assert!(BASH.contains("unalias gwt"));
    }

    #[test]
    fn every_snippet_defines_gwt() {
        assert!(FISH.contains("function gwt"));
        assert!(BASH.contains("GWT_CD_FILE"));
        assert!(FISH.contains("GWT_CD_FILE"));
    }
}
