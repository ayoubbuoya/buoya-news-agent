mod tools;

use anyhow::Result;
use async_openai::config::OpenAIConfig;
use async_openai::types::chat::{
    ChatCompletionMessageToolCall, ChatCompletionMessageToolCalls,
    ChatCompletionRequestAssistantMessage, ChatCompletionRequestAssistantMessageArgs,
    ChatCompletionRequestMessage, ChatCompletionRequestSystemMessage,
    ChatCompletionRequestToolMessageArgs, ChatCompletionRequestUserMessage,
    CreateChatCompletionRequestArgs, FunctionCall,
};
use futures::StreamExt;
use sqlx::SqlitePool;
use tokio::sync::mpsc::UnboundedSender;

use crate::state::AppState;
use crate::types::{ChatMessage, Role};

/// System prompt prepended to every conversation. It steers the model toward the
/// article-reading tools when a question concerns stored news.
const SYSTEM_PROMPT: &str = "You are Buoya, a news assistant. You have tools to \
read a local database covering crypto, DeFi, AI, security, and markets. It holds \
two kinds of records: (1) news articles from RSS feeds, and (2) daily market \
snapshots — a crypto Fear & Greed sentiment index, a top-coins market overview \
with 24h price moves, and total DeFi TVL by chain. \
When the user asks about a topic or theme, use semantic_search (meaning-based) to \
find relevant articles; for an exact ticker or proper name use search_articles; \
for what's new in an area use list_recent_articles. Then use get_article to read \
a promising result in depth. \
For questions about market sentiment or mood, current prices or movers, or DeFi \
TVL, call get_market_snapshot instead of searching the articles. \
Ground your reply in what the tools return and cite article titles and sources. \
If the database has nothing relevant, say so plainly instead of inventing \
details. For general questions unrelated to stored data, answer directly without \
using the tools.";

/// Build the system prompt for a conversation, appending the current UTC date and
/// time so the model can answer time-relative questions ("latest", "this week",
/// "yesterday") without guessing. Resolved per request from the clock.
fn system_prompt() -> String {
    let now = chrono::Utc::now().format("%A, %B %-d, %Y at %H:%M UTC");
    format!("{SYSTEM_PROMPT}\n\nThe current date and time is {now}.")
}

/// Upper bound on tool-call rounds in a single turn, guarding against a model
/// that loops on tool calls without ever producing a final answer.
const MAX_TOOL_ROUNDS: usize = 8;

/// Events emitted while an assistant response streams in. Consumed by the TUI
/// event loop; the LLM task never panics, it reports failures as `Error`.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// A chunk of generated text to append to the in-progress reply.
    Token(String),
    /// The assistant invoked a tool; carries a human-readable label for display.
    ToolCall(String),
    /// The stream finished successfully.
    Done,
    /// The request failed; carries a human-readable message.
    Error(String),
}

/// Convert a stored chat message into an `async-openai` request message.
fn to_request_message(msg: &ChatMessage) -> ChatCompletionRequestMessage {
    match msg.role {
        Role::System => ChatCompletionRequestSystemMessage::from(msg.content.as_str()).into(),
        Role::User => ChatCompletionRequestUserMessage::from(msg.content.as_str()).into(),
        Role::Assistant => ChatCompletionRequestAssistantMessage::from(msg.content.as_str()).into(),
    }
}

/// A tool call assembled from streamed deltas. The model sends a call's id,
/// name, and arguments in fragments across many chunks, keyed by `index`.
#[derive(Default)]
struct ToolCallAccum {
    id: String,
    name: String,
    arguments: String,
}

/// Outcome of streaming one assistant turn: either it finished a text answer, or
/// it requested tool calls that must be run before continuing.
enum TurnOutcome {
    /// The model produced its final answer; tokens were already forwarded.
    Finished,
    /// The model asked to call these tools; run them and loop.
    ToolCalls(Vec<ToolCallAccum>),
    /// The receiver was dropped (UI closed); stop silently.
    Aborted,
}

/// Stream a chat completion for the given conversation `history`, forwarding each
/// token to `tx`. The model may call article-reading tools (against `pool`)
/// before producing its answer; tool rounds are resolved transparently and only
/// the final answer's tokens reach `tx`. Sends `StreamEvent::Done` on success or
/// `StreamEvent::Error` on any failure. Designed to be run in a spawned task.
pub async fn prompt_stream(
    client: async_openai::Client<OpenAIConfig>,
    history: Vec<ChatMessage>,
    model: String,
    pool: SqlitePool,
    embedder: std::sync::Arc<crate::embeddings::Embedder>,
    tx: UnboundedSender<StreamEvent>,
) {
    // Prepend the system prompt; it is not persisted in `history`.
    let mut messages: Vec<ChatCompletionRequestMessage> = Vec::with_capacity(history.len() + 1);
    messages.push(ChatCompletionRequestSystemMessage::from(system_prompt()).into());
    messages.extend(history.iter().map(to_request_message));

    for _ in 0..MAX_TOOL_ROUNDS {
        match stream_turn(&client, &model, &messages, &tx).await {
            Ok(TurnOutcome::Finished) => {
                let _ = tx.send(StreamEvent::Done);
                return;
            }
            Ok(TurnOutcome::Aborted) => return,
            Ok(TurnOutcome::ToolCalls(calls)) => {
                if let Err(e) = run_tool_round(&pool, &embedder, &mut messages, calls, &tx).await {
                    let _ = tx.send(StreamEvent::Error(e.to_string()));
                    return;
                }
            }
            Err(message) => {
                let _ = tx.send(StreamEvent::Error(message));
                return;
            }
        }
    }

    let _ = tx.send(StreamEvent::Error(
        "the model kept requesting tools without answering".to_string(),
    ));
}

/// Stream a single assistant turn, forwarding text tokens to `tx` and gathering
/// any tool-call deltas. Returns the turn's outcome, or an error string.
async fn stream_turn(
    client: &async_openai::Client<OpenAIConfig>,
    model: &str,
    messages: &[ChatCompletionRequestMessage],
    tx: &UnboundedSender<StreamEvent>,
) -> std::result::Result<TurnOutcome, String> {
    let request = CreateChatCompletionRequestArgs::default()
        .model(model)
        .messages(messages.to_vec())
        .tools(tools::tool_definitions())
        .build()
        .map_err(|e| format!("failed to build request: {e}"))?;

    let mut stream = client
        .chat()
        .create_stream(request)
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    let mut tool_calls: Vec<ToolCallAccum> = Vec::new();

    while let Some(item) = stream.next().await {
        let response = item.map_err(|e| format!("stream error: {e}"))?;
        let Some(choice) = response.choices.first() else {
            continue;
        };

        if let Some(content) = &choice.delta.content
            && !content.is_empty()
            && tx.send(StreamEvent::Token(content.clone())).is_err()
        {
            // Receiver dropped (UI closed); stop streaming.
            return Ok(TurnOutcome::Aborted);
        }

        if let Some(deltas) = &choice.delta.tool_calls {
            accumulate_tool_calls(&mut tool_calls, deltas);
        }
    }

    // The stream has fully drained, so any accumulated calls are complete. Run
    // them rather than relying on `finish_reason`, which some OpenAI-compatible
    // providers omit. A call with no name is noise from a stray delta; drop it.
    tool_calls.retain(|call| !call.name.is_empty());
    if tool_calls.is_empty() {
        Ok(TurnOutcome::Finished)
    } else {
        Ok(TurnOutcome::ToolCalls(tool_calls))
    }
}

/// Merge streamed tool-call fragments into `acc`, keyed by their `index`.
fn accumulate_tool_calls(
    acc: &mut Vec<ToolCallAccum>,
    deltas: &[async_openai::types::chat::ChatCompletionMessageToolCallChunk],
) {
    for delta in deltas {
        let idx = delta.index as usize;
        if idx >= acc.len() {
            acc.resize_with(idx + 1, ToolCallAccum::default);
        }
        let entry = &mut acc[idx];
        if let Some(id) = &delta.id {
            entry.id.push_str(id);
        }
        if let Some(function) = &delta.function {
            if let Some(name) = &function.name {
                entry.name.push_str(name);
            }
            if let Some(arguments) = &function.arguments {
                entry.arguments.push_str(arguments);
            }
        }
    }
}

/// Append the assistant's tool-call message followed by each tool's result, so
/// the next turn sees what was requested and what came back.
async fn run_tool_round(
    pool: &SqlitePool,
    embedder: &std::sync::Arc<crate::embeddings::Embedder>,
    messages: &mut Vec<ChatCompletionRequestMessage>,
    calls: Vec<ToolCallAccum>,
    tx: &UnboundedSender<StreamEvent>,
) -> Result<()> {
    let tool_calls: Vec<ChatCompletionMessageToolCalls> = calls
        .iter()
        .map(|call| {
            ChatCompletionMessageToolCalls::Function(ChatCompletionMessageToolCall {
                id: call.id.clone(),
                function: FunctionCall {
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                },
            })
        })
        .collect();

    let assistant_msg = ChatCompletionRequestAssistantMessageArgs::default()
        .tool_calls(tool_calls)
        .build()?;
    messages.push(assistant_msg.into());

    for call in calls {
        tracing::debug!("running tool {} with args {}", call.name, call.arguments);
        let _ = tx.send(StreamEvent::ToolCall(describe_call(
            &call.name,
            &call.arguments,
        )));
        let result = tools::execute(pool, embedder, &call.name, &call.arguments).await;
        let tool_msg = ChatCompletionRequestToolMessageArgs::default()
            .tool_call_id(call.id)
            .content(result)
            .build()?;
        messages.push(tool_msg.into());
    }

    Ok(())
}

/// Build a short, human-readable label for a tool call to show in the UI, e.g.
/// `search_articles · "ethereum etf"`. Falls back to the bare tool name when the
/// arguments cannot be parsed.
fn describe_call(name: &str, arguments: &str) -> String {
    let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or_default();
    let detail = match name {
        "semantic_search" | "search_articles" => args
            .get("query")
            .and_then(|v| v.as_str())
            .map(|q| format!("\"{q}\"")),
        "list_recent_articles" => args
            .get("category")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or(Some("recent".to_string())),
        "get_article" => args
            .get("id")
            .and_then(|v| v.as_i64())
            .map(|id| format!("#{id}")),
        "get_market_snapshot" => Some("sentiment, prices, TVL".to_string()),
        _ => None,
    };
    match detail {
        Some(detail) => format!("{name} · {detail}"),
        None => name.to_string(),
    }
}

pub async fn prompt(app_state: &AppState, prompt: &str, model: &str) -> Result<String> {
    let client = &app_state.llm_client;

    let request = CreateChatCompletionRequestArgs::default()
        .model(model)
        .messages([ChatCompletionRequestUserMessage::from(prompt).into()])
        .build()?;

    let response = client.chat().create(request).await?;

    let content = response
        .choices
        .first()
        .and_then(|choice| choice.message.content.clone())
        .ok_or_else(|| anyhow::anyhow!("No content in LLM response"))?;

    Ok(content)
}
