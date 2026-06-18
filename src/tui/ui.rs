//! Rendering for the chat TUI.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::tui::app::{App, Focus, Status};
use crate::tui::markdown;
use crate::types::Role;

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn draw(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(28), Constraint::Min(20)])
        .split(frame.area());

    draw_sidebar(frame, app, chunks[0]);
    draw_main(frame, app, chunks[1]);
}

fn draw_sidebar(frame: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Sidebar;
    let items: Vec<ListItem> = app
        .sessions
        .iter()
        .map(|s| ListItem::new(Line::from(s.title.clone())))
        .collect();

    let highlight = Style::default()
        .fg(Color::Black)
        .bg(Color::Cyan)
        .add_modifier(Modifier::BOLD);

    let list = List::new(items)
        .block(block("Sessions  (Ctrl+N new)", focused))
        .highlight_style(highlight)
        .highlight_symbol("▌ ");

    let mut state = ListState::default();
    state.select(Some(app.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_main(frame: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),    // history
            Constraint::Length(5), // input box
            Constraint::Length(1), // status / help bar
        ])
        .split(area);

    draw_history(frame, app, rows[0]);
    draw_input(frame, app, rows[1]);
    draw_status(frame, app, rows[2]);
}

fn draw_history(frame: &mut Frame, app: &App, area: Rect) {
    let block = block("Conversation", false);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let width = inner.width.max(1) as usize;
    let mut lines: Vec<Line> = Vec::new();

    for msg in &app.messages {
        push_message(&mut lines, msg.role, &msg.content, width);
    }
    if let Status::Streaming { partial, .. } = &app.status {
        let shown = if partial.is_empty() { "…" } else { partial };
        push_message(&mut lines, Role::Assistant, shown, width);
    }

    let height = inner.height as usize;
    let max_scroll = lines.len().saturating_sub(height);
    let scroll = if app.follow {
        max_scroll
    } else {
        (app.scroll as usize).min(max_scroll)
    };

    let visible: Vec<Line> = lines.into_iter().skip(scroll).take(height).collect();
    frame.render_widget(Paragraph::new(visible), inner);
}

/// Append a styled header line plus wrapped body lines for one message.
fn push_message(lines: &mut Vec<Line<'static>>, role: Role, content: &str, width: usize) {
    let (label, color) = match role {
        Role::User => ("You", Color::Green),
        Role::Assistant => ("Assistant", Color::Cyan),
        Role::System => ("System", Color::DarkGray),
    };
    lines.push(Line::from(Span::styled(
        label.to_string(),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )));

    match role {
        // The user typed plain text; render it verbatim (just wrapped).
        Role::User => {
            for raw_line in content.split('\n') {
                if raw_line.is_empty() {
                    lines.push(Line::from(String::new()));
                    continue;
                }
                for wrapped in wrap(raw_line, width) {
                    lines.push(Line::from(wrapped));
                }
            }
        }
        // The agent replies in Markdown; render it richly.
        Role::Assistant | Role::System => {
            lines.extend(markdown::render(content, width));
        }
    }
    lines.push(Line::from(String::new()));
}

fn draw_input(frame: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Input;
    let block = block("Message  (Enter send · Alt+Enter newline)", focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    frame.render_widget(Paragraph::new(app.input.as_str()), inner);

    // Place the cursor when the input is focused. The cursor index is in chars;
    // map it onto the wrapped layout of the (possibly multi-line) input.
    if focused {
        let width = inner.width.max(1) as usize;
        let (col, row) = cursor_position(&app.input, app.cursor, width);
        let x = inner.x + col.min(inner.width.saturating_sub(1) as usize) as u16;
        let y = inner.y + row.min(inner.height.saturating_sub(1) as usize) as u16;
        frame.set_cursor_position((x, y));
    }
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let line = if let Some(err) = &app.error {
        Line::from(Span::styled(
            format!(" error: {err}"),
            Style::default().fg(Color::Red),
        ))
    } else if let Status::Streaming { spinner_idx, .. } = &app.status {
        let frame_char = SPINNER[spinner_idx % SPINNER.len()];
        Line::from(Span::styled(
            format!(" {frame_char} thinking…"),
            Style::default().fg(Color::Yellow),
        ))
    } else {
        Line::from(Span::styled(
            " Tab switch pane · PgUp/PgDn scroll · Ctrl+Q quit",
            Style::default().fg(Color::DarkGray),
        ))
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn block(title: &str, focused: bool) -> Block<'static> {
    let border_color = if focused { Color::Cyan } else { Color::DarkGray };
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(Color::White),
        ))
}

/// Word-wrap `text` to `width` columns, hard-splitting words longer than the width.
fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_len = 0;

    for word in text.split(' ') {
        let word_len = word.chars().count();

        if word_len > width {
            if current_len > 0 {
                lines.push(std::mem::take(&mut current));
            }
            let mut chunk = String::new();
            for ch in word.chars() {
                if chunk.chars().count() == width {
                    lines.push(std::mem::take(&mut chunk));
                }
                chunk.push(ch);
            }
            current = chunk;
            current_len = current.chars().count();
            continue;
        }

        let extra = if current_len == 0 { word_len } else { word_len + 1 };
        if current_len + extra > width {
            lines.push(std::mem::take(&mut current));
            current = word.to_string();
            current_len = word_len;
        } else {
            if current_len > 0 {
                current.push(' ');
            }
            current.push_str(word);
            current_len += extra;
        }
    }
    lines.push(current);
    lines
}

/// Map a char-index cursor onto (column, row) within a wrapped input box.
fn cursor_position(input: &str, cursor: usize, width: usize) -> (usize, usize) {
    let mut col = 0;
    let mut row = 0;
    for ch in input.chars().take(cursor) {
        if ch == '\n' || col + 1 >= width {
            row += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (col, row)
}
