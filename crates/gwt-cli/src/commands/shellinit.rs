// A subprocess can't change the parent shell's cwd, so we ship shell wrappers
// that capture `git wt`'s stdout and cd into it. `gwt` is the explicit form;
// `git` is overridden so bare `git wt` (the picker) also performs the cd.

pub fn print(shell: &str) {
    let snippet = match shell {
        "fish" => FISH,
        "zsh" | "bash" => BASH,
        _ => BASH,
    };
    print!("{snippet}");
}

const BASH: &str = r#"gwt() {
  local out
  out="$(command git wt "$@")" || return $?
  if [ -n "$out" ] && [ -d "$out" ]; then
    cd "$out" || return
  fi
}

git() {
  # Only the bare `git wt` form (the picker) needs cd integration; everything
  # else — including `git wt list`, `git wt new …`, plain `git status`, etc. —
  # falls straight through to the real binary so we don't surprise users.
  if [ "$#" = "1" ] && [ "$1" = "wt" ]; then
    local out
    out="$(command git wt)" || return $?
    if [ -n "$out" ] && [ -d "$out" ]; then
      cd "$out" || return
    fi
    return
  fi
  command git "$@"
}
"#;

const FISH: &str = r#"function gwt
  set -l out (command git wt $argv)
  or return $status
  if test -n "$out" -a -d "$out"
    cd $out
  end
end

function git
  if test (count $argv) -eq 1 -a "$argv[1]" = "wt"
    set -l out (command git wt)
    or return $status
    if test -n "$out" -a -d "$out"
      cd $out
    end
    return
  end
  command git $argv
end
"#;
