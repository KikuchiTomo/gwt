//! What the TUI prints while it is not drawing.
//!
//! A `run` step in the recipe can take minutes and print megabytes, and none of
//! that fits on the one status line a spinner has. So the TUI tears its
//! viewport down, hands the terminal to the command (`sync::lease_screen`) and
//! prints here instead: plain lines on stderr, no frame, no colors of ours
//! competing with the command's own, and nothing that assumes it knows where
//! the cursor is.
//!
//! The vocabulary is the one `git wt sync apply` already uses on the command
//! line, so the same recipe reads the same either way.

use std::io::Write;

use gwt_core::sync::{self, Event, Outcome, Step};
use gwt_core::t;

/// A reporter for while the screen belongs to the recipe, plus whatever it has
/// to say afterwards.
#[derive(Default)]
pub struct Screen {
    failures: Vec<String>,
}

impl Screen {
    pub fn new() -> Self {
        Self::default()
    }

    /// Announce what is about to take the screen.
    pub fn banner(label: &str) {
        eprintln!();
        eprintln!("{label}");
    }

    /// Name each command before it runs, and say how it went afterwards. What
    /// comes between the two lines is the command's own output, printed by the
    /// command itself.
    pub fn on(&mut self, ev: Event) {
        match ev {
            Event::StepStart(Step::Run(r)) => eprintln!("· {}", sync::one_line(&r.cmd)),
            Event::StepStart(_) => {}
            // Only reachable if the lease was not taken; relay it rather than
            // silently dropping a command's output.
            Event::Output(line) => eprintln!("  {line}"),
            Event::StepDone(step, outcome) => match outcome {
                Outcome::Ran { code: 0, secs } => {
                    eprintln!("  ✓ {} ({secs}s)", step.subject_line())
                }
                Outcome::Ran { code, secs } => self.fail(format!(
                    "{} exited {code} after {secs}s",
                    step.subject_line()
                )),
                Outcome::Failed { detail } => {
                    self.fail(format!("{}: {detail}", step.subject_line()))
                }
                Outcome::Blocked { reason } => {
                    self.fail(format!("{}: {reason}", step.dst().unwrap_or("")))
                }
                _ => {}
            },
        }
    }

    fn fail(&mut self, text: String) {
        eprintln!("  ✗ {text}");
        self.failures.push(text);
    }

    /// The first thing that went wrong, for the message the TUI shows once it
    /// is back — the screen scrolls, a status line does not.
    pub fn first_failure(&self) -> Option<&str> {
        self.failures.first().map(String::as_str)
    }

    /// Hold the screen until the user has read it.
    ///
    /// Out of an inline viewport the output stays where it is, right above the
    /// redrawn TUI, so on a clean run there is nothing to wait for — stopping
    /// to ask would be a keystroke of ceremony per worktree. A failure is worth
    /// reading before anything paints over it, and on the alt-screen fallback
    /// *everything* is: the screen the command wrote on is about to be taken
    /// back whole.
    pub fn finish(&self, output_survives: bool) {
        if self.failures.is_empty() && output_survives {
            return;
        }
        eprint!("{}", t::screen_return());
        let _ = std::io::stderr().flush();
        let mut sink = String::new();
        let _ = std::io::stdin().read_line(&mut sink);
    }
}
