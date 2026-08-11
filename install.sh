#!/usr/bin/env sh
# install.sh — fetch the latest git-wt release, verify sha256, install it.
# Re-running detects an existing install and prompts to update.

set -eu

REPO="KikuchiTomo/gwt"
BIN="git-wt"
DEFAULT_PREFIX="${HOME}/.local/bin"

VERSION=""
TARGET_OVERRIDE=""
# GWT_PREFIX is the specific knob; a bare PREFIX is honored for compatibility but
# it's a common ambient variable, so we say out loud when it's what we picked up.
PREFIX_SRC="default"
if [ -n "${GWT_PREFIX:-}" ]; then
    PREFIX="$GWT_PREFIX"; PREFIX_SRC="\$GWT_PREFIX"
elif [ -n "${PREFIX:-}" ]; then
    PREFIX="$PREFIX"; PREFIX_SRC="\$PREFIX (inherited from your environment)"
else
    PREFIX="$DEFAULT_PREFIX"
fi
ASSUME_YES=0
FORCE=0
NO_SETUP=0
UNINSTALL=0
MARKER="# >>> git-wt setup (managed by install.sh) >>>"
MARKER_END="# <<< git-wt setup <<<"

usage() {
    cat <<EOF
Usage: install.sh [options]

Options:
  --version vX.Y.Z   install a specific release (default: latest)
  --prefix DIR       install destination (default: \$PREFIX or $DEFAULT_PREFIX)
  --target TRIPLE    force a release target (default: auto-detected;
                     Linux defaults to the static musl build)
  --yes              don't prompt; auto-update and auto-setup shell rc
  --force            reinstall even if the same version is already installed
  --no-setup         skip writing shell init / PATH lines into your rc file
  --uninstall        remove the binary and the managed rc block, then exit
  -h, --help         show this help
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --version) VERSION="$2"; shift 2 ;;
        --prefix)  PREFIX="$2";  PREFIX_SRC="--prefix"; shift 2 ;;
        --target)  TARGET_OVERRIDE="$2"; shift 2 ;;
        --yes|-y)  ASSUME_YES=1; shift ;;
        --force)   FORCE=1;      shift ;;
        --no-setup) NO_SETUP=1;  shift ;;
        --uninstall) UNINSTALL=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

err() { printf 'install: %s\n' "$*" >&2; exit 1; }
info() { printf '  %s\n' "$*"; }

need() { command -v "$1" >/dev/null 2>&1 || err "missing required command: $1"; }
need uname
need tar
need mkdir
need rm
# curl or wget — at least one
if command -v curl >/dev/null 2>&1; then DL="curl -fL"
elif command -v wget >/dev/null 2>&1; then DL="wget -qO-"
else err "need curl or wget"
fi

# sha256 verification helper — different tool names per OS.
sha256_check() {
    file="$1"; expected="$2"
    if command -v shasum >/dev/null 2>&1; then
        actual=$(shasum -a 256 "$file" | awk '{print $1}')
    elif command -v sha256sum >/dev/null 2>&1; then
        actual=$(sha256sum "$file" | awk '{print $1}')
    else
        err "need shasum or sha256sum"
    fi
    [ "$actual" = "$expected" ] || err "sha256 mismatch for $file"
}

detect_target() {
    os=$(uname -s); arch=$(uname -m)
    case "$os" in
        Darwin)
            [ "$arch" = "arm64" ] || err "macOS x86_64 is not published; use arm64"
            echo "aarch64-apple-darwin"
            ;;
        Linux)
            [ "$arch" = "x86_64" ] || [ "$arch" = "amd64" ] \
                || err "unsupported Linux arch: $arch (only x86_64)"
            # Default to musl: it is statically linked, so it runs on any Linux.
            # The gnu build carries the glibc version of whatever built it, and a
            # newer glibc than the host's fails at exec time with
            # "version `GLIBC_x.yz' not found" — a confusing failure to hand to
            # someone who just ran an installer. Opt into gnu with --target.
            echo "x86_64-unknown-linux-musl"
            ;;
        MINGW*|MSYS*|CYGWIN*)
            echo "x86_64-pc-windows-msvc"
            ;;
        *) err "unsupported OS: $os" ;;
    esac
}

latest_tag() {
    # Avoid the GitHub API (rate-limited) — peek at the redirect from /releases/latest.
    if command -v curl >/dev/null 2>&1; then
        url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' \
              "https://github.com/$REPO/releases/latest")
    else
        # wget follows redirects too; --max-redirect 0 -S exposes Location.
        url=$(wget --max-redirect=0 -S "https://github.com/$REPO/releases/latest" 2>&1 \
              | awk '/Location:/ {print $2}' | tail -1)
    fi
    [ -n "${url:-}" ] || err "failed to resolve latest release"
    tag="${url##*/}"
    # When no releases exist, GitHub redirects to /releases (no tag) instead.
    case "$tag" in
        v[0-9]*) printf '%s\n' "$tag" ;;
        *) err "no published releases found for $REPO" ;;
    esac
}

prompt_yes_no() {
    if [ "$ASSUME_YES" = 1 ]; then return 0; fi
    printf '%s [y/N] ' "$1"
    read -r ans </dev/tty || ans=""
    case "$ans" in y|Y|yes|YES) return 0 ;; *) return 1 ;; esac
}

detect_shell() {
    # Trust $SHELL if it points to something we know; fall back to bash.
    case "${SHELL:-}" in
        */zsh)  echo "zsh"  ;;
        */fish) echo "fish" ;;
        */bash) echo "bash" ;;
        *)      echo "bash" ;;
    esac
}

rc_path_for() {
    case "$1" in
        zsh)  echo "${ZDOTDIR:-$HOME}/.zshrc" ;;
        fish) echo "$HOME/.config/fish/config.fish" ;;
        bash)
            # On macOS every Terminal window is a LOGIN shell, and login bash reads
            # .bash_profile — never .bashrc. Writing to .bashrc there means the
            # block silently never runs, which looks exactly like "git-wt: command
            # not found". So on Darwin always target .bash_profile, creating it if
            # needed. Linux bash reads .bashrc for interactive shells.
            if [ "$(uname -s)" = "Darwin" ]; then
                echo "$HOME/.bash_profile"
            else
                echo "$HOME/.bashrc"
            fi
            ;;
    esac
}

# Everything between the markers is ours: drop it so a re-run (or an uninstall)
# never leaves a second copy or a stale eval behind.
strip_setup_block() {
    awk -v m="$MARKER" -v e="$MARKER_END" '
        $0 == m { skip = 1; next }
        $0 == e { skip = 0; next }
        !skip   { print }
    ' "$1"
}

# Idempotent: a marker block lets re-runs rewrite cleanly without dupes.
write_setup_block() {
    rc="$1"; shell="$2"
    mkdir -p "$(dirname "$rc")"
    [ -f "$rc" ] || : > "$rc"

    tmp="${rc}.gwt.tmp"
    strip_setup_block "$rc" > "$tmp"

    # The eval is guarded twice: `command -v` for a missing binary, and a
    # captured, stderr-silenced run for one that resolves but cannot exec (a
    # glibc mismatch, a truncated download). Either way shell startup stays
    # silent — anything printed here lands before the prompt and breaks tools
    # like powerlevel10k's instant prompt.
    {
        cat "$tmp"
        printf '\n%s\n' "$MARKER"
        case "$shell" in
            fish)
                printf 'if not contains %s $PATH\n' "$PREFIX"
                printf '    set -gx PATH %s $PATH\n' "$PREFIX"
                printf 'end\n'
                printf 'if type -q %s\n' "$BIN"
                printf '    %s shellinit fish 2>/dev/null | source\n' "$BIN"
                printf 'end\n'
                ;;
            *)
                printf 'case ":$PATH:" in\n'
                printf '    *":%s:"*) ;;\n' "$PREFIX"
                printf '    *) export PATH="%s:$PATH" ;;\n' "$PREFIX"
                printf 'esac\n'
                printf 'if command -v %s >/dev/null 2>&1; then\n' "$BIN"
                printf '    __gwt_init="$(%s shellinit %s 2>/dev/null)" && eval "$__gwt_init"\n' "$BIN" "$shell"
                printf '    unset __gwt_init\n'
                printf 'fi\n'
                ;;
        esac
        printf '%s\n' "$MARKER_END"
    } > "$rc.new"
    mv "$rc.new" "$rc"
    rm -f "$tmp"
}

manual_lines() {
    case "$1" in
        fish) info "  set -gx PATH $PREFIX \$PATH" ;
              info "  type -q $BIN; and $BIN shellinit fish | source" ;;
        *)    info "  export PATH=\"$PREFIX:\$PATH\"" ;
              info "  command -v $BIN >/dev/null 2>&1 && eval \"\$($BIN shellinit $1)\"" ;;
    esac
}

run_setup() {
    [ "$NO_SETUP" = 1 ] && {
        info "skipped shell setup (--no-setup) — add this to your shell rc:"
        manual_lines "$(detect_shell)"
        return
    }
    shell=$(detect_shell)
    rc=$(rc_path_for "$shell")
    if ! prompt_yes_no "Set up $shell integration in $rc?"; then
        info "skipped shell setup — to enable later, add to $rc:"
        manual_lines "$shell"
        return
    fi
    write_setup_block "$rc" "$shell"
    info "wrote setup block to $rc"
    SETUP_RC="$rc"
}

# Prove the thing we just installed is actually reachable, and say exactly what
# to do next. Guessing "it probably works" is how a broken PATH goes unnoticed.
verify_install() {
    if ! run_err=$("$install_dst" --version 2>&1 >/dev/null); then
        printf 'install: the binary at %s does not run:\n' "$install_dst" >&2
        printf '  %s\n' "$run_err" >&2
        case "$run_err" in
            *GLIBC*|*libc*)
                printf 'install: that is a glibc mismatch. Re-run with the static build:\n' >&2
                printf '  ... | sh -s -- --target x86_64-unknown-linux-musl\n' >&2
                ;;
        esac
        exit 1
    fi
    printf '\n'
    info "$BIN $VERSION is installed at $install_dst"

    case ":$PATH:" in
        *":$PREFIX:"*)
            info "$PREFIX is already on your PATH."
            ;;
        *)
            info "NOTE: $PREFIX is not on this shell's PATH yet."
            ;;
    esac

    if [ -n "${SETUP_RC:-}" ]; then
        info "Run this now (or just open a new terminal):"
        printf '\n      source %s\n\n' "$SETUP_RC"
    else
        info "Then verify with:  $BIN --version  &&  git wt --help"
    fi
}

# Uninstall is the whole run when asked for: no network, no version resolution.
# It only ever touches what this script created — the binary it installed and
# the marker block it wrote. Worktrees, secrets, .gwt/ and git state are never touched.
run_uninstall() {
    bin_path=""
    [ -f "$PREFIX/$BIN" ] && bin_path="$PREFIX/$BIN"
    rcs=""
    for shell in zsh bash fish; do
        rc=$(rc_path_for "$shell")
        [ -f "$rc" ] || continue
        grep -qF "$MARKER" "$rc" 2>/dev/null || continue
        case " $rcs " in *" $rc "*) ;; *) rcs="$rcs $rc" ;; esac
    done

    if [ -z "$bin_path" ] && [ -z "$rcs" ]; then
        info "nothing to uninstall: no $BIN at $PREFIX and no managed rc block."
        other=$(command -v "$BIN" 2>/dev/null || true)
        [ -n "$other" ] && info "note: a different $BIN is on your PATH at $other"
        exit 0
    fi

    printf '\n'
    info "about to remove:"
    [ -n "$bin_path" ] && info "  binary    $bin_path"
    for rc in $rcs; do info "  rc block  $rc"; done
    printf '\n'
    prompt_yes_no "Remove these?" || { info "cancelled."; exit 0; }

    [ -n "$bin_path" ] && rm -f "$bin_path" && info "removed $bin_path"
    for rc in $rcs; do
        strip_setup_block "$rc" > "$rc.new" && mv "$rc.new" "$rc"
        info "removed the setup block from $rc"
    done

    cfg="${XDG_CONFIG_HOME:-$HOME/.config}/gwt/config"
    printf '\n'
    if [ -f "$cfg" ]; then
        info "left in place: $cfg (your language setting)"
        info "  delete it with:  rm -rf $(dirname "$cfg")"
    fi
    other=$(command -v "$BIN" 2>/dev/null || true)
    [ -n "$other" ] && info "note: another $BIN is still on your PATH at $other"
    info "The gwt/git shell functions live in this shell until you open a new one"
    info "(or run: unset -f gwt git __gwt_run)."
    exit 0
}

if [ "$UNINSTALL" = 1 ]; then
    run_uninstall
fi

TARGET="${TARGET_OVERRIDE:-$(detect_target)}"
info "install prefix: $PREFIX (from $PREFIX_SRC)"
if [ -z "$VERSION" ]; then
    info "resolving latest release..."
    VERSION=$(latest_tag)
fi
case "$VERSION" in v*) ;; *) VERSION="v$VERSION" ;; esac

# Existing install detection — probe ONLY the binary at our own install path,
# never whatever happens to be on $PATH. Other tools share the name `git-wt`
# and their --version output is noisy (and may even open /dev/tty).
EXISTING=""
if [ -x "$PREFIX/$BIN" ]; then
    ver_line=$("$PREFIX/$BIN" --version </dev/null 2>/dev/null | head -n1 || true)
    case "$ver_line" in
        "git-wt "[0-9]*.[0-9]*.[0-9]*)
            EXISTING=$(printf '%s\n' "$ver_line" | awk '{print $2}')
            ;;
    esac
fi
if [ -n "$EXISTING" ] && [ "$FORCE" -ne 1 ]; then
    target_ver="${VERSION#v}"
    if [ "$EXISTING" = "$target_ver" ]; then
        info "$BIN $EXISTING is already installed (latest)."
        prompt_yes_no "Reinstall anyway?" || { info "nothing to do."; exit 0; }
    else
        info "$BIN $EXISTING is installed; new version is $target_ver."
        prompt_yes_no "Update to $target_ver?" || { info "skipped."; exit 0; }
    fi
fi

case "$TARGET" in
    *windows*) EXT="zip" ;;
    *)         EXT="tar.gz" ;;
esac

ASSET="${BIN}-${VERSION}-${TARGET}.${EXT}"
BASE="https://github.com/${REPO}/releases/download/${VERSION}"
URL="${BASE}/${ASSET}"
SUM_URL="${BASE}/SHA256SUMS"

TMP=$(mktemp -d 2>/dev/null || mktemp -d -t gwt-install)
trap 'rm -rf "$TMP"' EXIT

info "downloading $ASSET"
( cd "$TMP" && $DL "$URL"     > "$ASSET" )
( cd "$TMP" && $DL "$SUM_URL" > "SHA256SUMS" )

# SHA256SUMS lines look like: `<sha>  <filename>` — match by filename.
expected=$(awk -v f="$ASSET" '$2 == f || $2 ~ ("/"f"$") {print $1; exit}' "$TMP/SHA256SUMS")
[ -n "$expected" ] || err "no checksum entry for $ASSET in SHA256SUMS"
sha256_check "$TMP/$ASSET" "$expected"
info "checksum ok"

# Windows assets are zips — we still expect the user runs this in a POSIX shell
# (Git Bash etc.) so we shell out to unzip for that case.
if [ "$EXT" = "zip" ]; then
    need unzip
    ( cd "$TMP" && unzip -q "$ASSET" )
else
    ( cd "$TMP" && tar -xzf "$ASSET" )
fi

# The extracted dir name matches the asset basename (without extension).
EXTRACTED="$TMP/${BIN}-${VERSION}-${TARGET}"
if [ ! -d "$EXTRACTED" ]; then
    err "unexpected archive layout: $EXTRACTED not found"
fi

bin_name="$BIN"
case "$TARGET" in *windows*) bin_name="${BIN}.exe" ;; esac
[ -f "$EXTRACTED/$bin_name" ] || err "$bin_name not in archive"

mkdir -p "$PREFIX"
install_dst="$PREFIX/$bin_name"
# install(1) sets sane perms on every platform we ship to.
if command -v install >/dev/null 2>&1; then
    install -m 0755 "$EXTRACTED/$bin_name" "$install_dst"
else
    cp "$EXTRACTED/$bin_name" "$install_dst"
    chmod 0755 "$install_dst"
fi

info "installed $install_dst ($VERSION)"
run_setup
verify_install
