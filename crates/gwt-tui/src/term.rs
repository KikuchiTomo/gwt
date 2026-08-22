// TUIs draw to stderr so the picker can print the chosen path on stdout
// for shell `cd "$(git wt)"` integrations.

use std::io::{self, Stderr};

use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::{Terminal, TerminalOptions, Viewport};

pub type Backend = CrosstermBackend<Stderr>;

pub struct Tui {
    pub term: Terminal<Backend>,
    alt_screen: bool,
}

impl std::ops::Deref for Tui {
    type Target = Terminal<Backend>;
    fn deref(&self) -> &Self::Target {
        &self.term
    }
}
impl std::ops::DerefMut for Tui {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.term
    }
}

pub fn enter_fullscreen() -> io::Result<Tui> {
    enable_raw_mode()?;
    let mut stderr = io::stderr();
    execute!(stderr, EnterAlternateScreen)?;
    Ok(Tui {
        term: Terminal::new(CrosstermBackend::new(stderr))?,
        alt_screen: true,
    })
}

pub fn leave_fullscreen(tui: &mut Tui) -> io::Result<()> {
    if tui.alt_screen {
        execute!(tui.term.backend_mut(), LeaveAlternateScreen)?;
        tui.alt_screen = false;
    }
    disable_raw_mode()?;
    tui.term.show_cursor()?;
    Ok(())
}

/// Inline (fzf-style) viewport. Inside tmux — and any terminal where the
/// initial DSR cursor probe fails — fall back to the alt-screen so the user
/// doesn't get bitten by "cursor position could not be read".
pub fn enter_inline(height: u16) -> io::Result<Tui> {
    enable_raw_mode()?;
    if std::env::var_os("TMUX").is_some() {
        return alt_screen_fallback();
    }
    match Terminal::with_options(
        CrosstermBackend::new(io::stderr()),
        TerminalOptions {
            viewport: Viewport::Inline(height),
        },
    ) {
        Ok(term) => Ok(Tui {
            term,
            alt_screen: false,
        }),
        Err(_) => alt_screen_fallback(),
    }
}

fn alt_screen_fallback() -> io::Result<Tui> {
    let mut stderr = io::stderr();
    execute!(stderr, EnterAlternateScreen)?;
    Ok(Tui {
        term: Terminal::new(CrosstermBackend::new(stderr))?,
        alt_screen: true,
    })
}

/// Give the real terminal back for as long as `f` runs, then build the TUI
/// again from scratch.
///
/// Built again rather than restored: `f` is a command printing whatever it
/// likes, so the cursor ends up wherever it left it, and a viewport that still
/// believed it owned four lines further up would paint over the very output the
/// user was meant to read.
/// `f` is told whether what it prints will still be there afterwards: an inline
/// viewport leaves it above the redrawn frame, but the alt-screen fallback
/// takes the whole screen back, so anything printed there has to be read before
/// `f` returns.
pub fn released<T>(tui: &mut Tui, height: u16, f: impl FnOnce(bool) -> T) -> io::Result<T> {
    let output_survives = !tui.alt_screen;
    leave_inline(tui)?;
    let out = f(output_survives);
    *tui = enter_inline(height)?;
    Ok(out)
}

pub fn leave_inline(tui: &mut Tui) -> io::Result<()> {
    if tui.alt_screen {
        execute!(tui.term.backend_mut(), LeaveAlternateScreen)?;
        tui.alt_screen = false;
    } else {
        tui.term.clear()?;
    }
    disable_raw_mode()?;
    tui.term.show_cursor()?;
    Ok(())
}
