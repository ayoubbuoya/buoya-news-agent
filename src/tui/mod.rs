//! Terminal chat UI: an interactive, persisted conversation with the agent.

mod app;
mod markdown;
mod ui;

use std::time::Duration;

use anyhow::Result;
use ratatui::crossterm::event;
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio::time;

use crate::llm::StreamEvent;
use crate::state::AppState;
use app::App;

/// Set up the terminal, run the chat event loop, and restore the terminal on exit.
pub async fn run(state: AppState) -> Result<()> {
    let mut terminal = ratatui::try_init()?;
    let result = run_loop(&mut terminal, state).await;
    ratatui::try_restore()?;
    result
}

async fn run_loop(terminal: &mut ratatui::DefaultTerminal, state: AppState) -> Result<()> {
    let mut app = App::new(state);
    app.load_sessions().await?;

    // Blocking terminal reads happen on a dedicated thread and arrive as messages,
    // keeping the async event loop free to also poll streamed LLM tokens.
    let (input_tx, mut input_rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        while let Ok(ev) = event::read() {
            if input_tx.send(ev).is_err() {
                break;
            }
        }
    });

    let mut llm_rx: Option<UnboundedReceiver<StreamEvent>> = None;
    let mut ticker = time::interval(Duration::from_millis(100));

    while !app.should_quit {
        terminal.draw(|frame| ui::draw(frame, &app))?;

        tokio::select! {
            maybe_event = input_rx.recv() => {
                match maybe_event {
                    Some(event) => app.handle_event(event).await?,
                    None => break, // input thread ended
                }
            }
            maybe_stream = recv_opt(&mut llm_rx) => {
                if let Some(stream_event) = maybe_stream
                    && app.handle_stream_event(stream_event).await?
                {
                    llm_rx = None;
                }
            }
            _ = ticker.tick() => app.on_tick(),
        }

        // A submit may have started a new stream; install its receiver.
        if let Some(rx) = app.take_pending_stream() {
            llm_rx = Some(rx);
        }
    }

    Ok(())
}

/// Await the next stream event, or never resolve when no stream is active.
async fn recv_opt(rx: &mut Option<UnboundedReceiver<StreamEvent>>) -> Option<StreamEvent> {
    match rx {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}
