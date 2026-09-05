//! A pseudo-terminal to run a `run` step's shell on.
//!
//! [`crate::shell`] explains *which* shell a step gets and why it has to be the
//! user's own, started the way their terminal starts it. This module is the
//! other half of that answer: the terminal itself.
//!
//! A shell only sets up a version manager when it believes it is interactive,
//! and "interactive" is not a flag we can simply assert. `bash -i` with its
//! output on a pipe opens by printing `bash: no job control in this shell`;
//! `zsh -i` prints `can't change option: zle`, twice — straight into the middle
//! of the output the step was there to produce. And the rc files themselves ask
//! the same question and believe the answer: the stock Debian `~/.bashrc`
//! begins
//!
//! ```text
//! case $- in *i*) ;; *) return;; esac
//! ```
//!
//! so sourcing it from a non-interactive shell returns before a single line of
//! nvm, rbenv or asdf setup has run. That is the whole bug, and no combination
//! of flags fixes it, because the shell is telling the truth: there is no
//! terminal.
//!
//! So we give it one. With a pty on the other end the shell is interactive
//! because it *is* interactive — job control works, `zle` loads, `$-` contains
//! `i`, `[[ -o interactive ]]` is true, and `/dev/tty` resolves — and every rc
//! file runs the way it does when a person opens a terminal. Nothing has to be
//! guessed at or worked around.
//!
//! What that costs is a single stream: a terminal has one, so the child's
//! stdout and stderr arrive interleaved, exactly as they would on screen. It
//! also means the command sees a terminal and prints for one, so [`Lines`]
//! undoes the two things that are only meant for a screen — ANSI escapes and
//! carriage-return redraws — and [`crate::shell::Fence`] separates what the
//! step was asked to run from what the shell says on its way in and out.
//!
//! Windows has no equivalent worth the surface area (ConPTY is a different
//! shape of API for a platform where none of the version managers above live),
//! so this module is Unix-only and the caller falls back to pipes.

#![cfg(unix)]

use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use crate::shell::Fence;

/// How big the terminal claims to be.
///
/// A pty starts at 0×0, and a build tool that asks and believes the answer will
/// either wrap every line at once or divide by zero. Nobody is watching this
/// one, so the number only has to be sane: wide enough that a progress bar or a
/// table is not folded into noise.
const COLS: u16 = 120;
const ROWS: u16 = 40;

/// An open pty pair: our end, and the end the child gets for its stdio.
pub struct Pty {
    master: OwnedFd,
    slave: OwnedFd,
}

/// `ptsname` returns a pointer into static storage, so two threads asking at
/// once get one answer between them. `ptsname_r` would avoid it but does not
/// exist everywhere we build, and the lock costs nothing here.
fn ptsname_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Open a pty pair.
///
/// Built out of `posix_openpt`/`grantpt`/`unlockpt`/`ptsname` rather than
/// `openpty`, which lives in `libutil` on older glibc and would put a linker
/// flag between gwt and a working build on distributions we never hear about.
pub fn open() -> io::Result<Pty> {
    // SAFETY: each call below is the documented POSIX sequence for opening a
    // pty pair, and every raw fd is wrapped in an `OwnedFd` before anything
    // that could return early.
    unsafe {
        let fd = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let master = OwnedFd::from_raw_fd(fd);
        if libc::grantpt(fd) < 0 || libc::unlockpt(fd) < 0 {
            return Err(io::Error::last_os_error());
        }
        let name = {
            let _guard = ptsname_lock().lock().unwrap_or_else(|e| e.into_inner());
            let p = libc::ptsname(fd);
            if p.is_null() {
                return Err(io::Error::last_os_error());
            }
            std::ffi::CStr::from_ptr(p).to_owned()
        };
        let sfd = libc::open(name.as_ptr(), libc::O_RDWR | libc::O_NOCTTY);
        if sfd < 0 {
            return Err(io::Error::last_os_error());
        }
        let slave = OwnedFd::from_raw_fd(sfd);
        let ws = libc::winsize {
            ws_row: ROWS,
            ws_col: COLS,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // A failure here is cosmetic — the pty works, it just does not know how
        // wide it is — so it is not worth failing the step over.
        libc::ioctl(fd, libc::TIOCSWINSZ as _, &ws);
        Ok(Pty { master, slave })
    }
}

impl Pty {
    /// Point `cmd`'s output at the pty and arrange for the child to adopt it as
    /// its *controlling* terminal.
    ///
    /// The controlling terminal is the part that is easy to leave out and hard
    /// to diagnose without: fds that happen to be a terminal make `isatty`
    /// true, but a shell also wants a session to put its jobs in and a
    /// `/dev/tty` to open, and without `setsid` + `TIOCSCTTY` it has neither —
    /// which is exactly the "cannot set terminal process group" this module
    /// exists to avoid.
    ///
    /// **stdin stays on `/dev/null`**, and deliberately. Everything the shell
    /// decides about being interactive it decides from `-i` and from the
    /// terminal it now controls, so the rc files all run — verified on both zsh
    /// and bash, `$-` and `[[ -o interactive ]]` included. What a pty on stdin
    /// would add is the one thing nobody wants here: a prompt that nobody is
    /// there to answer, blocking until the step's timeout runs out. On
    /// `/dev/null` the same prompt reads end-of-file and the command gets on
    /// with its default, which is what it would do in a script.
    pub fn attach(&self, cmd: &mut Command) -> io::Result<()> {
        use std::os::unix::process::CommandExt;
        use std::process::Stdio;
        cmd.stdin(Stdio::null())
            .stdout(self.slave.try_clone()?)
            .stderr(self.slave.try_clone()?);
        // The child inherits every open fd, so the slave is still at this
        // number over there — and unlike fd 1, it is a number nothing else is
        // about to be dup2'd onto.
        let slave = self.slave.as_raw_fd();
        // SAFETY: `setsid` and `ioctl` are async-signal-safe, which is the bar
        // for anything running between fork and exec.
        unsafe {
            cmd.pre_exec(move || {
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(slave, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Ok(())
    }

    /// Split the spawned pty into the stream to read and the handle that can
    /// still end what is writing to it.
    ///
    /// Consuming `self` is what closes our copy of the slave, and that matters:
    /// the read only ever ends because the last slave fd went with the child
    /// that closed it. Hold one here and a finished command still leaves the
    /// reader waiting forever.
    ///
    /// The two halves are separated because they are used from two threads: the
    /// stream is read on one, while the other is still counting down to the
    /// timeout that may have to kill the command.
    pub fn split(self) -> io::Result<(File, Control)> {
        let control = Control {
            master: self.master.try_clone()?,
        };
        drop(self.slave);
        Ok((File::from(self.master), control))
    }
}

/// What is left of a pty once its output has been handed to a reader: enough of
/// the terminal to ask who is running on it, and to stop them.
pub struct Control {
    master: OwnedFd,
}

/// Read from a pty, treating the hangup as the end of the stream.
///
/// A pipe reports the writer going away as end-of-file. A pty reports it as
/// `EIO` on Linux — a genuine error everywhere else, and simply "the command
/// exited" here.
pub fn read(f: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    use std::io::Read;
    match f.read(buf) {
        Err(e) if e.raw_os_error() == Some(libc::EIO) => Ok(0),
        other => other,
    }
}

impl Control {
    /// Kill a timed-out step and everything it is still running.
    ///
    /// Two groups, because a shell puts work in more than one. The shell got
    /// its own session when it took the pty, so its pid is also its process
    /// group id, and signalling that group reaches everything it ran without
    /// job control — which is most things, since the prologue turns job control
    /// off. But the rc files run *before* that prologue, while the shell is
    /// still a fresh interactive one, and a job started under job control is
    /// given a process group of its own. So the terminal is asked which group
    /// is in the foreground, and that one is signalled too.
    ///
    /// Only ever right for a child that was given a pty: without the `setsid`
    /// that comes with one, the group being signalled is gwt's own.
    pub fn kill_group(&self, pid: u32) {
        // SAFETY: `tcgetpgrp` reads a number out of the terminal we own, and
        // the signals go to groups led by a child we spawned and have not yet
        // reaped — so neither pid can have been recycled onto anyone else.
        unsafe {
            let fg = libc::tcgetpgrp(self.master.as_raw_fd());
            if fg > 0 && fg != pid as libc::pid_t {
                libc::killpg(fg, libc::SIGKILL);
            }
            libc::killpg(pid as libc::pid_t, libc::SIGKILL);
        }
    }
}

/// Terminal bytes, turned back into lines worth logging.
///
/// A command that can see a terminal writes for one, and three of those habits
/// have to be undone before its output can be handed to a reporter that expects
/// lines:
///
/// * **Escape sequences.** Colour is the harmless kind; the rest moves the
///   cursor, and a TUI drawing the same screen does not survive being told to.
/// * **Carriage returns.** A progress bar redraws one line by returning to the
///   start of it. Read as a stream that is one enormous line, so `\r` ends a
///   line here just as `\n` does — each redraw becomes its own, and the last
///   one is the final state.
/// * **The fence.** See [`crate::shell::Fence`]: everything before the opening
///   marker is the shell starting up, everything after the closing one is it
///   shutting down, and neither is what the step was asked to produce.
pub struct Lines {
    fence: Fence,
    /// The line being built, escapes already gone.
    line: Vec<u8>,
    /// Held back until the opening marker proves the shell got that far.
    held: Vec<u8>,
    started: bool,
    ended: bool,
    /// A `\r\n` is one line ending, not two.
    after_cr: bool,
    esc: Esc,
}

/// Where we are inside an escape sequence.
enum Esc {
    None,
    /// Just saw ESC; the next byte says what kind.
    Start,
    /// `ESC [ … ` — ends at the first byte in `@`..`~`.
    Csi,
    /// `ESC ] … ` — ends at BEL, or at the ST that follows an ESC.
    Osc,
}

/// How much shell startup to hold before deciding the opening marker is never
/// coming. Past this the fence is abandoned and everything is reported: an
/// enormous preamble is a broken rc file, and the text of it is the only thing
/// that explains the step's failure.
const HELD_MAX: usize = 256 * 1024;

impl Lines {
    pub fn new(fence: Fence) -> Lines {
        Lines {
            fence,
            line: Vec::new(),
            held: Vec::new(),
            started: false,
            ended: false,
            after_cr: false,
            esc: Esc::None,
        }
    }

    /// Feed raw bytes from the pty, calling `out` once per completed line.
    pub fn feed(&mut self, bytes: &[u8], out: &mut dyn FnMut(&str)) {
        for &b in bytes {
            if self.ended {
                return;
            }
            match self.esc {
                Esc::Start => {
                    self.esc = match b {
                        b'[' => Esc::Csi,
                        b']' => Esc::Osc,
                        // `ESC ( B` and friends: one more byte, already eaten.
                        _ => Esc::None,
                    };
                    continue;
                }
                Esc::Csi => {
                    if (0x40..=0x7e).contains(&b) {
                        self.esc = Esc::None;
                    }
                    continue;
                }
                Esc::Osc => {
                    // BEL ends it, and so does the ST written as ESC \ — whose
                    // ESC we can simply treat as the end, since what follows is
                    // a lone backslash we would rather not print either.
                    if b == 0x07 || b == 0x1b {
                        self.esc = Esc::None;
                    }
                    continue;
                }
                Esc::None => {}
            }
            match b {
                0x1b => self.esc = Esc::Start,
                b'\n' => {
                    // The `\r` of a `\r\n` already ended this line.
                    if !std::mem::take(&mut self.after_cr) {
                        self.flush(out);
                    }
                }
                b'\r' => {
                    self.flush(out);
                    self.after_cr = true;
                }
                // Backspace, bell, and the rest of C0 are for a screen, not a
                // log. Tab is real text.
                b if b < 0x20 && b != b'\t' => {
                    self.after_cr = false;
                    self.line.push(b);
                    self.check_markers(out);
                }
                b => {
                    self.after_cr = false;
                    self.line.push(b);
                    self.check_markers(out);
                }
            }
        }
    }

    /// The stream ended. Anything left is a line the command never terminated —
    /// and if the fence never opened, the startup we were holding is all the
    /// explanation there is.
    pub fn finish(&mut self, out: &mut dyn FnMut(&str)) {
        if self.ended {
            return;
        }
        if !self.started {
            self.abandon_fence();
        }
        self.flush(out);
    }

    /// Watch the line being built for either marker.
    ///
    /// Markers are looked for in the accumulated line rather than the incoming
    /// chunk because a read can split one down the middle.
    fn check_markers(&mut self, out: &mut dyn FnMut(&str)) {
        if !self.started {
            if let Some(at) = find(&self.line, self.fence.begin.as_bytes()) {
                self.line.drain(..at + self.fence.begin.len());
                self.held.clear();
                self.started = true;
            } else if self.line.len() + self.held.len() > HELD_MAX {
                self.abandon_fence();
            }
            return;
        }
        if let Some(at) = find(&self.line, self.fence.end.as_bytes()) {
            self.line.truncate(at);
            self.flush(out);
            self.ended = true;
        }
    }

    /// Report everything after all: the shell never reached the command, so the
    /// noise we were holding back is the error message.
    fn abandon_fence(&mut self) {
        let mut all = std::mem::take(&mut self.held);
        all.append(&mut self.line);
        self.line = all;
        self.started = true;
    }

    fn flush(&mut self, out: &mut dyn FnMut(&str)) {
        let line = std::mem::take(&mut self.line);
        if !self.started {
            // Still waiting on the opening marker, so this belongs to the
            // shell's startup — keep it only in case it turns out to be the
            // last thing the shell ever said.
            self.held.extend_from_slice(&line);
            self.held.push(b'\n');
            return;
        }
        // Trailing whitespace is a redraw padding its line out, not content.
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end();
        if !text.is_empty() {
            out(text);
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(fence: &Fence, chunks: &[&[u8]]) -> Vec<String> {
        let mut got = Vec::new();
        let mut lines = Lines::new(fence.clone());
        for c in chunks {
            lines.feed(c, &mut |l| got.push(l.to_string()));
        }
        lines.finish(&mut |l| got.push(l.to_string()));
        got
    }

    #[test]
    fn the_fence_keeps_the_command_and_drops_the_shell_around_it() {
        let f = Fence::new();
        let out = collect(
            &f,
            &[format!(
                "architecture: arm64\n{}built ok\n{}So long and thanks for all the fish.\n",
                f.begin, f.end
            )
            .as_bytes()],
        );
        assert_eq!(out, vec!["built ok"], "rc noise on both sides must go");
    }

    #[test]
    fn a_marker_split_across_two_reads_is_still_a_marker() {
        let f = Fence::new();
        let whole = format!("noise\n{}kept\n{}tail\n", f.begin, f.end);
        let bytes = whole.as_bytes();
        // Split inside the opening marker.
        let at = 5 + 1 + f.begin.len() / 2;
        let out = collect(&f, &[&bytes[..at], &bytes[at..]]);
        assert_eq!(out, vec!["kept"]);
    }

    #[test]
    fn a_shell_that_never_reached_the_command_still_gets_to_explain_itself() {
        let f = Fence::new();
        let out = collect(&f, &[b".zshrc:12: command not found: rbenv\n"]);
        assert_eq!(
            out,
            vec![".zshrc:12: command not found: rbenv"],
            "swallowing this is how a broken rc becomes a silent failure"
        );
    }

    #[test]
    fn a_progress_bar_becomes_one_line_per_redraw() {
        let f = Fence::new();
        let out = collect(
            &f,
            &[format!("{}\rFetching  10%\rFetching 100%\r\ndone\n", f.begin).as_bytes()],
        );
        assert_eq!(
            out,
            vec!["Fetching  10%", "Fetching 100%", "done"],
            "\\r ends a line, and \\r\\n is still only one ending"
        );
    }

    #[test]
    fn escape_sequences_never_reach_the_log() {
        let f = Fence::new();
        let out = collect(
            &f,
            &[format!(
                "{}\x1b[32mgreen\x1b[0m\x1b]0;a title\x07 and text\n",
                f.begin
            )
            .as_bytes()],
        );
        assert_eq!(out, vec!["green and text"]);
    }

    #[test]
    fn a_blank_line_inside_the_output_is_not_a_line_ending_twice() {
        let f = Fence::new();
        let out = collect(&f, &[format!("{}a\r\n\r\nb\n", f.begin).as_bytes()]);
        assert_eq!(out, vec!["a", "b"]);
    }
}
