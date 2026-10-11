//! Interactive terminal viewer (`genome-viewer -i`).

mod app;
mod tracks;
mod ui;

use anyhow::Result;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyEventKind};

pub use app::App;

/// Take over the terminal and run the viewer until the user quits.
pub fn run(mut app: App) -> Result<()> {
    // ratatui::init also restores the terminal if we panic.
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    while !app.quit {
        terminal.draw(|frame| ui::draw(frame, app))?;
        // Resizes and other events just fall through to a redraw.
        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Press {
                app.handle_key(key);
            }
        }
    }
    Ok(())
}
