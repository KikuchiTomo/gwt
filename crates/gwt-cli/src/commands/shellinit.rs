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

// Everything this snippet touches — `gwt`, `git`, `cat`, `rm`, `mktemp`, `cd` —
// is an ordinary word somebody has aliased (`gwt='git worktree'`, `cat=bat`,
// `cd=z`, `rm='rm -i'`). Both bash and zsh expand aliases while PARSING, and
// `eval "$(git-wt shellinit zsh)"` parses this whole snippet before running a
// single line of it, so every alias that exists at eval time gets baked into the
// function bodies below — `unalias` on line 1 runs far too late to stop it. Two
// rules keep the snippet ours:
//
//   * define with the `function` keyword, so the name being defined is not
//     alias-expanded (the POSIX `name() {` form dies with "defining function
//     based on alias" when `name` is taken);
//   * escape every command word we call (`\cat`), which suppresses alias
//     expansion in both shells while still finding functions and builtins.
//
// `git` itself is deliberately left alone at call time: overriding someone
// else's `git` alias is not ours to do, and `gwt` keeps working either way.
const BASH: &str = r#"unalias gwt 2>/dev/null || true

function __gwt_run {
  # Do NOT capture stdout: the picker keeps it attached to the terminal for its
  # cursor probe and writes the chosen path to $GWT_CD_FILE instead.
  local __gwt_cd __gwt_rc __gwt_dir
  __gwt_cd="$(\command mktemp "${TMPDIR:-/tmp}/gwt-cd.XXXXXX")" || return
  GWT_CD_FILE="$__gwt_cd" \command git wt "$@"
  __gwt_rc=$?
  __gwt_dir="$(\command cat "$__gwt_cd" 2>/dev/null)"
  \command rm -f "$__gwt_cd"
  if [ "$__gwt_rc" -eq 0 ] && [ -n "$__gwt_dir" ] && [ -d "$__gwt_dir" ]; then
    # \cd skips an alias but keeps a `cd` function (zoxide and friends) working.
    \cd "$__gwt_dir" || return
  fi
  return "$__gwt_rc"
}

function gwt {
  __gwt_run "$@"
}

function git {
  # Only the bare `git wt` form (the picker) needs cd integration; everything
  # else — including `git wt list`, `git wt new …`, plain `git status`, etc. —
  # falls straight through to the real binary so we don't surprise users.
  if [ "$#" = "1" ] && [ "$1" = "wt" ]; then
    __gwt_run
    return
  fi
  \command git "$@"
}
"#;

// fish expands nothing at parse time and its `alias` just defines a function,
// which our definitions replace — but a config that aliases `gwt` *after* this
// snippet would still capture the `git` wrapper, so route both through the
// private helper here too.
const FISH: &str = r#"function __gwt_run
  # Do NOT capture stdout: the picker keeps it attached to the terminal for its
  # cursor probe and writes the chosen path to $GWT_CD_FILE instead.
  set -l __gwt_cd (command mktemp (test -n "$TMPDIR"; and echo $TMPDIR; or echo /tmp)/gwt-cd.XXXXXX)
  env GWT_CD_FILE=$__gwt_cd command git wt $argv
  set -l __gwt_rc $status
  set -l __gwt_dir (command cat $__gwt_cd 2>/dev/null)
  command rm -f $__gwt_cd
  if test $__gwt_rc -eq 0 -a -n "$__gwt_dir" -a -d "$__gwt_dir"
    cd $__gwt_dir
  end
  return $__gwt_rc
end

function gwt
  __gwt_run $argv
end

function git
  if test (count $argv) -eq 1 -a "$argv[1]" = "wt"
    __gwt_run
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

    /// `eval` parses the whole snippet before `unalias` runs, so any command
    /// word left bare in a function body is replaced by the user's alias at
    /// definition time. `alias cat=bat` alone was enough to make the chosen
    /// path unreadable and the `cd` silently not happen.
    #[test]
    fn every_command_word_is_alias_proof() {
        for escaped in [
            "\\command mktemp",
            "\\command cat",
            "\\command rm",
            "\\command git wt",
            "\\cd \"$__gwt_dir\"",
        ] {
            assert!(BASH.contains(escaped), "must be called as `{escaped}`");
        }
        for bare in [
            "$(mktemp",
            "$(cat ",
            "\n  rm ",
            "\n    cd \"",
            "\n  command ",
        ] {
            assert!(!BASH.contains(bare), "`{bare}` is alias-expandable");
        }
    }

    /// The `git` wrapper used to call `gwt`. With `alias gwt='git worktree'`
    /// (a very common alias) present at eval time, that name was expanded into
    /// the wrapper's body, so `git wt` ran the old alias and the picker never
    /// appeared. Route through a name nobody aliases instead.
    #[test]
    fn the_git_wrapper_cannot_be_hijacked_by_a_gwt_alias() {
        for snippet in [BASH, FISH] {
            assert!(snippet.contains("__gwt_run"));
            assert!(
                !snippet.contains("\n    gwt\n"),
                "the git wrapper must not call the aliasable name `gwt`"
            );
        }
    }

    #[test]
    fn every_snippet_defines_gwt() {
        assert!(FISH.contains("function gwt"));
        assert!(BASH.contains("GWT_CD_FILE"));
        assert!(FISH.contains("GWT_CD_FILE"));
    }
}
