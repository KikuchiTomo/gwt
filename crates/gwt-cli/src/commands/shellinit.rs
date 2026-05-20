// `git wt` is a subprocess and cannot change the parent shell's cwd; the
// wrapper below captures stdout and cd's on the caller's behalf.

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
"#;

const FISH: &str = r#"function gwt
  set -l out (command git wt $argv)
  or return $status
  if test -n "$out" -a -d "$out"
    cd $out
  end
end
"#;
