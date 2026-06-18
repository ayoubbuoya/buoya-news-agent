//! TUI application state, input handling, and persistence wiring.

use anyhow::Result;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::sync::mpsc::{self, UnboundedReceiver};

use crate::db;
use crate::llm::{self, StreamEvent};
use crate::state::AppState;
use crate::types::{ChatMessage, ChatSession, Role};

/// Title given to a freshly created session until its first user message renames it.
const DEFAULT_SESSION_TITLE: &str = "New chat";

/// Which pane currently receives navigation keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    Input,
}

/// Whether the agent is currently producing a reply.
#[derive(Debug, Clone)]
pub enum Status {
    Idle,
    /// A reply is streaming in; `partial` is the text received so far and
    /// `tools` lists any tools the assistant has invoked this turn.
    Streaming {
        partial: String,
        spinner_idx: usize,
        tools: Vec<String>,
    },
}

pub struct App {
    pub state: AppState,
    pub sessions: Vec<ChatSession>,
    /// Index into `sessions` highlighted in the sidebar.
    pub selected: usize,
    pub current_session_id: Option<String>,
    pub messages: Vec<ChatMessage>,

    pub input: String,
    /// Cursor position within `input`, measured in characters.
    pub cursor: usize,

    pub focus: Focus,
    /// Scroll offset (in display lines) for the chat history.
    pub scroll: u16,
    /// When true the history sticks to the newest message.
    pub follow: bool,

    pub status: Status,
    pub error: Option<String>,
    pub should_quit: bool,

    /// Receiver for a stream that was just started and needs to be installed by
    /// the event loop. Taken with `take_pending_stream`.
    pub pending_stream_rx: Option<UnboundedReceiver<StreamEvent>>,
}

impl App {
    pub fn new(state: AppState) -> Self {
        Self {
            state,
            sessions: Vec::new(),
            selected: 0,
            current_session_id: None,
            messages: Vec::new(),
            input: String::new(),
            cursor: 0,
            focus: Focus::Input,
            scroll: 0,
            follow: true,
            status: Status::Idle,
            error: None,
            should_quit: false,
            pending_stream_rx: None,
        }
    }

    /// Load sessions on startup, creating a first one if the table is empty, and
    /// open the most recent session.
    pub async fn load_sessions(&mut self) -> Result<()> {
        self.sessions = db::list_sessions(&self.state.db_pool).await?;

        if self.sessions.is_empty() {
            let session =
                db::create_session(&self.state.db_pool, DEFAULT_SESSION_TITLE).await?;
            self.sessions.push(session);
        }

        self.selected = 0;
        self.open_selected().await
    }

    /// Open the session highlighted in the sidebar and load its messages.
    pub async fn open_selected(&mut self) -> Result<()> {
        let Some(session) = self.sessions.get(self.selected) else {
            return Ok(());
        };
        let id = session.id.clone();
        self.messages = db::load_messages(&self.state.db_pool, &id).await?;
        self.current_session_id = Some(id);
        self.follow = true;
        self.scroll = 0;
        Ok(())
    }

    /// Create a brand-new session and make it current.
    pub async fn new_session(&mut self) -> Result<()> {
        let session =
            db::create_session(&self.state.db_pool, DEFAULT_SESSION_TITLE).await?;
        self.sessions.insert(0, session);
        self.selected = 0;
        self.open_selected().await
    }

    /// Take ownership of a just-started stream receiver, if any.
    pub fn take_pending_stream(&mut self) -> Option<UnboundedReceiver<StreamEvent>> {
        self.pending_stream_rx.take()
    }

    /// Advance the spinner; called on every UI tick.
    pub fn on_tick(&mut self) {
        if let Status::Streaming { spinner_idx, .. } = &mut self.status {
            *spinner_idx = spinner_idx.wrapping_add(1);
        }
    }

    pub async fn handle_event(&mut self, event: Event) -> Result<()> {
        if let Event::Key(key) = event
            && key.kind == KeyEventKind::Press
        {
            self.handle_key(key).await?;
        }
        Ok(())
    }

    async fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // Global shortcuts.
        match key.code {
            KeyCode::Char('q') if ctrl => {
                self.should_quit = true;
                return Ok(());
            }
            KeyCode::Char('n') if ctrl => {
                self.new_session().await?;
                self.focus = Focus::Input;
                return Ok(());
            }
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Sidebar => Focus::Input,
                    Focus::Input => Focus::Sidebar,
                };
                return Ok(());
            }
            KeyCode::PageUp => {
                self.follow = false;
                self.scroll = self.scroll.saturating_sub(5);
                return Ok(());
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_add(5);
                return Ok(());
            }
            _ => {}
        }

        match self.focus {
            Focus::Sidebar => self.handle_sidebar_key(key).await?,
            Focus::Input => self.handle_input_key(key).await?,
        }
        Ok(())
    }

    async fn handle_sidebar_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Down => {
                if self.selected + 1 < self.sessions.len() {
                    self.selected += 1;
                }
            }
            KeyCode::Enter => {
                self.open_selected().await?;
                self.focus = Focus::Input;
            }
            KeyCode::Esc | KeyCode::Char('q') => self.should_quit = true,
            _ => {}
        }
        Ok(())
    }

    async fn handle_input_key(&mut self, key: KeyEvent) -> Result<()> {
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => self.focus = Focus::Sidebar,
            KeyCode::Enter if alt => self.insert_char('\n'),
            KeyCode::Enter => self.submit().await?,
            KeyCode::Backspace => self.backspace(),
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => {
                if self.cursor < self.input.chars().count() {
                    self.cursor += 1;
                }
            }
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.input.chars().count(),
            KeyCode::Char(c) => self.insert_char(c),
            _ => {}
        }
        Ok(())
    }

    fn byte_offset(&self, char_idx: usize) -> usize {
        self.input
            .char_indices()
            .nth(char_idx)
            .map(|(i, _)| i)
            .unwrap_or(self.input.len())
    }

    fn insert_char(&mut self, c: char) {
        let offset = self.byte_offset(self.cursor);
        self.input.insert(offset, c);
        self.cursor += 1;
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let remove_at = self.byte_offset(self.cursor - 1);
        self.input.remove(remove_at);
        self.cursor -= 1;
    }

    /// Persist the typed message, then spawn a streaming LLM task for the reply.
    async fn submit(&mut self) -> Result<()> {
        if matches!(self.status, Status::Streaming { .. }) {
            return Ok(());
        }
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return Ok(());
        }
        let Some(session_id) = self.current_session_id.clone() else {
            return Ok(());
        };

        self.error = None;
        self.input.clear();
        self.cursor = 0;
        self.follow = true;

        let is_first = self.messages.is_empty();
        let user_msg =
            db::insert_message(&self.state.db_pool, &session_id, Role::User, &text, &[]).await?;
        self.messages.push(user_msg);

        // Auto-title a still-unnamed session from its opening message.
        if is_first {
            let title = title_from(&text);
            db::rename_session(&self.state.db_pool, &session_id, &title).await?;
            if let Some(s) = self.sessions.iter_mut().find(|s| s.id == session_id) {
                s.title = title;
            }
        }

        // Spawn the streaming completion; tokens come back over the channel.
        let (tx, rx) = mpsc::unbounded_channel();
        let client = self.state.llm_client.clone();
        let model = self.state.config.ai_model.clone();
        let history = self.messages.clone();
        let pool = self.state.db_pool.clone();
        let embedder = self.state.embedder.clone();
        tokio::spawn(llm::prompt_stream(
            client, history, model, pool, embedder, tx,
        ));

        self.pending_stream_rx = Some(rx);
        self.status = Status::Streaming {
            partial: String::new(),
            spinner_idx: 0,
            tools: Vec::new(),
        };
        Ok(())
    }

    /// Apply one streaming event. Returns true when the stream has ended (so the
    /// event loop drops its receiver).
    pub async fn handle_stream_event(&mut self, event: StreamEvent) -> Result<bool> {
        match event {
            StreamEvent::Token(token) => {
                if let Status::Streaming { partial, .. } = &mut self.status {
                    partial.push_str(&token);
                }
                Ok(false)
            }
            StreamEvent::ToolCall(label) => {
                if let Status::Streaming { tools, .. } = &mut self.status {
                    tools.push(label);
                }
                Ok(false)
            }
            StreamEvent::Done => {
                self.finish_stream(None).await?;
                Ok(true)
            }
            StreamEvent::Error(message) => {
                self.finish_stream(Some(message)).await?;
                Ok(true)
            }
        }
    }

    async fn finish_stream(&mut self, error: Option<String>) -> Result<()> {
        let (partial, tools) = match std::mem::replace(&mut self.status, Status::Idle) {
            Status::Streaming { partial, tools, .. } => (partial, tools),
            Status::Idle => (String::new(), Vec::new()),
        };

        if let Some(session_id) = self.current_session_id.clone()
            && !partial.trim().is_empty()
        {
            let msg = db::insert_message(
                &self.state.db_pool,
                &session_id,
                Role::Assistant,
                &partial,
                &tools,
            )
            .await?;
            self.messages.push(msg);
        }

        self.error = error;
        self.follow = true;
        Ok(())
    }
}

/// Build a short session title from the first user message.
fn title_from(text: &str) -> String {
    let first_line = text.lines().next().unwrap_or(text).trim();
    let truncated: String = first_line.chars().take(40).collect();
    if truncated.is_empty() {
        DEFAULT_SESSION_TITLE.to_string()
    } else if first_line.chars().count() > 40 {
        format!("{truncated}…")
    } else {
        truncated
    }
}
