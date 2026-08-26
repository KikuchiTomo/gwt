//! Work the UI thread must never wait for.
//!
//! Every git call costs a process, and a process costs milliseconds that land
//! squarely between a keypress and the frame that answers it. So the rule is
//! the one lazygit is built on: the loop draws what it has, and what it does
//! not have yet is on its way.
//!
//! There are two shapes of that here. A stream — the per-worktree counts —
//! arrives row by row and lives in `picker::state`. A single answer, which is
//! most things, is a [`Pending`]: started as early as we know we will want it,
//! collected whenever the frame that needs it comes around.

use std::sync::mpsc::{self, Receiver, TryRecvError};

/// One value being fetched off-thread.
///
/// Nothing here blocks: [`Pending::poll`] takes the answer if it has arrived
/// and says so if it just did, and [`Pending::get`] is what the frame draws
/// from — `None` meaning "not yet", never "wait here".
pub struct Pending<T> {
    rx: Option<Receiver<T>>,
    value: Option<T>,
}

impl<T: Send + 'static> Pending<T> {
    /// Start `job` on a worker thread.
    pub fn start(job: impl FnOnce() -> T + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            // A receiver that is gone means the screen it was for is gone too.
            let _ = tx.send(job());
        });
        Self {
            rx: Some(rx),
            value: None,
        }
    }

    /// Collect the answer if the worker has it. `true` on the one call that
    /// takes delivery — the frame after that is the one worth redrawing.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = self.rx.as_ref() else {
            return false;
        };
        match rx.try_recv() {
            Ok(v) => {
                self.value = Some(v);
                self.rx = None;
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                // The worker died without answering (a panic in a git call, say).
                // Nothing is coming, and pretending otherwise would spin the
                // loop at the pending poll rate forever.
                self.rx = None;
                false
            }
        }
    }

    /// What has landed, if anything.
    pub fn get(&self) -> Option<&T> {
        self.value.as_ref()
    }

    /// Whether an answer is still on its way.
    pub fn is_pending(&self) -> bool {
        self.rx.is_some()
    }
}

impl<T> Default for Pending<T> {
    fn default() -> Self {
        Self {
            rx: None,
            value: None,
        }
    }
}
