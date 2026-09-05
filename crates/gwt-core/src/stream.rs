//! Reading back what a `run` step printed.
//!
//! [`crate::shell`] explains which shell a step gets and why it has to be the
//! user's own, started the way their terminal starts it. This module is the
//! other half: how the step is watched while it runs, and how the shell is kept
//! from being able to stop and ask something.
//!
//! Two decisions, and they are the same decision twice.
//!
//! **One stream, not two.** An interactive shell says things on its way in and
//! out — a version manager announcing itself, an rc that echoes what it just
//! detected, a login shell's parting word — and it says them on stderr while
//! the command's own output goes to stdout. Read as two pipes those interleave
//! by luck, and no marker on one can say anything about the other. Joined into
//! one, the order is the order they happened in, and [`crate::shell::Fence`]
//! can put a marker either side of the command and mean it. That is what makes
//! a step's log the command's output and nothing else.
//!
//! **No terminal.** It is tempting to give the step a pty: the shell would then
//! be interactive because it *is* interactive, with job control and every rc
//! file running exactly as it does in a new tab. But an interactive shell with
//! a terminal is also a shell that can *ask* — and nobody is here to answer. On
//! a fresh Ubuntu, `/etc/zsh/zshrc` runs `compinit`, `compinit` finds a
//! completion directory it does not like, and it stops on
//!
//! ```text
//! Ignore insecure directories and continue [y] or abort compinit [n]?
//! ```
//!
//! reading the answer from the terminal, which never comes. The step then does
//! nothing at all until its timeout kills it, and `/dev/tty` means pointing
//! stdin at `/dev/null` does not help. With no terminal anywhere the same
//! prompt cannot be asked: zsh says `not interactive and can't open terminal`,
//! abandons `compinit`, and gets on with the command — while `-i` has already
//! done the part that mattered, which is to make `$-` contain `i` so the rc
//! files set the toolchain up. Interactive enough to be set up correctly,
//! never interactive enough to wait for somebody.
//!
//! What that costs is the two complaints a shell makes when it looks for a
//! terminal and finds none — bash's `cannot set terminal process group` and
//! `no job control in this shell`, zsh's note about `compinit`. All of it is
//! said before the opening marker, so none of it reaches the step's log.

#![cfg(unix)]

use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::Command;

use crate::shell::Fence;

/// Put the step in a session of its own.
///
/// Two things follow, and both matter. Everything the command starts shares one
/// process group, so a timed-out step can be ended whole rather than one shell
/// at a time — see [`kill_group`]. And a session with no controlling terminal
/// cannot acquire one from stdio that is a pipe and a `/dev/null`, so the
/// prompt described above has nowhere to be asked from, however interactive the
/// shell believes it is.
pub fn detach(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: `setsid` is async-signal-safe, which is the bar for anything
    // running between fork and exec, and cannot fail for a freshly forked child
    // — it is not a process group leader.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
}

/// Kill a timed-out step and everything it started.
///
/// [`detach`] made the shell a session leader, so its pid is also its process
/// group id and one signal reaches the compiler it was waiting on rather than
/// only the shell waiting for it. Only ever right for a child that was
/// detached: without that, the group being signalled is gwt's own.
pub fn kill_group(pid: u32) {
    // SAFETY: a signal to a group led by a child we spawned and have not yet
    // reaped, so the number cannot have been recycled onto anyone else.
    unsafe {
        libc::killpg(pid as libc::pid_t, libc::SIGKILL);
    }
}

/// One pipe for a step to write both its streams into.
///
/// Built by hand rather than with `std::io::pipe`, which is newer than the Rust
/// this crate promises to build on. The read end is close-on-exec so the child
/// does not inherit a copy: a pipe ends when its last writer goes, and a reader
/// that is also holding it open would wait for itself.
pub fn pipe() -> io::Result<(File, OwnedFd)> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `pipe` fills two fds or returns -1, and each is wrapped in an
    // `OwnedFd` before anything that could return early.
    unsafe {
        if libc::pipe(fds.as_mut_ptr()) < 0 {
            return Err(io::Error::last_os_error());
        }
        let read = OwnedFd::from_raw_fd(fds[0]);
        let write = OwnedFd::from_raw_fd(fds[1]);
        // std dups the write end onto the child's 1 and 2, and a dup drops the
        // flag — so marking both here costs the child nothing and keeps every
        // other fd of ours out of it.
        for fd in [&read, &write] {
            libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
        }
        Ok((File::from(read), write))
    }
}

/// Read a step's output, treating a signal as nothing at all.
///
/// `EINTR` is not the end of anything, but taken for one it stops the reader
/// while the command is still writing — and then the command blocks on a pipe
/// nobody is draining, in the middle of its own output, until the timeout.
pub fn read(f: &mut impl io::Read, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        return match f.read(buf) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            other => other,
        };
    }
}

/// A step's bytes, turned into lines worth logging.
///
/// Three things have to be undone before the stream can be handed to a reporter
/// that expects lines:
///
/// * **The fence.** See [`crate::shell::Fence`]: everything before the opening
///   marker is the shell starting up — including the complaints it makes about
///   the terminal it could not find — and everything after the closing one is
///   it shutting down. Neither is what the step was asked to produce.
/// * **Escape sequences.** A command told to colour its output anyway (`FORCE_COLOR`,
///   `--color=always`, a CI variable it recognises) writes escapes that a
///   status line cannot show, and the ones that move the cursor are worse than
///   unreadable: a TUI drawing the same screen does not survive being told to.
/// * **Carriage returns.** A progress bar redraws one line by returning to the
///   start of it. Read as a stream that is one enormous line, so `\r` ends a
///   line here just as `\n` does — each redraw becomes its own, and the last
///   one is the final state.
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
