// TUIs draw to stderr so the picker can print the chosen path on stdout
// for shell `cd "$(git wt)"` integrations.

use std::io::{self, Stderr};

use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::{Terminal, TerminalOptions, Viewport};

pub type Tui = Terminal<CrosstermBackend<Stderr>>;

pub fn enter_fullscreen() -> io::Result<Tui> {
    enable_raw_mode()?;
    let mut stderr = io::stderr();
    execute!(stderr, EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(stderr))
}

pub fn leave_fullscreen(term: &mut Tui) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(term.backend_mut(), LeaveAlternateScreen)?;
    term.show_cursor()?;
    Ok(())
}

pub fn enter_inline(height: u16) -> io::Result<Tui> {
    enable_raw_mode()?;
    Terminal::with_options(
        CrosstermBackend::new(io::stderr()),
        TerminalOptions {
            viewport: Viewport::Inline(height),
        },
    )
}

pub fn leave_inline(term: &mut Tui) -> io::Result<()> {
    disable_raw_mode()?;
    term.clear()?;
    term.show_cursor()?;
    Ok(())
}
