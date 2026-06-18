use anyhow::Result;
use async_openai::config::OpenAIConfig;
use async_openai::types::chat::{
    ChatCompletionRequestAssistantMessage, ChatCompletionRequestMessage,
    ChatCompletionRequestSystemMessage, ChatCompletionRequestUserMessage,
    CreateChatCompletionRequestArgs,
};
use futures::StreamExt;
use tokio::sync::mpsc::UnboundedSender;

use crate::state::AppState;
use crate::types::{ChatMessage, Role};

/// Events emitted while an assistant response streams in. Consumed by the TUI
/// event loop; the LLM task never panics, it reports failures as `Error`.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// A chunk of generated text to append to the in-progress reply.
    Token(String),
    /// The stream finished successfully.
    Done,
    /// The request failed; carries a human-readable message.
    Error(String),
}

/// Convert a stored chat message into an `async-openai` request message.
fn to_request_message(msg: &ChatMessage) -> ChatCompletionRequestMessage {
    match msg.role {
        Role::System => {
            ChatCompletionRequestSystemMessage::from(msg.content.as_str()).into()
        }
        Role::User => ChatCompletionRequestUserMessage::from(msg.content.as_str()).into(),
        Role::Assistant => {
            ChatCompletionRequestAssistantMessage::from(msg.content.as_str()).into()
        }
    }
}

/// Stream a chat completion for the given conversation `history`, forwarding each
/// token to `tx`. Sends `StreamEvent::Done` on success or `StreamEvent::Error`
/// on any failure. Designed to be run in a spawned task.
pub async fn prompt_stream(
    client: async_openai::Client<OpenAIConfig>,
    history: Vec<ChatMessage>,
    model: String,
    tx: UnboundedSender<StreamEvent>,
) {
    let messages: Vec<ChatCompletionRequestMessage> =
        history.iter().map(to_request_message).collect();

    let request = match CreateChatCompletionRequestArgs::default()
        .model(model)
        .messages(messages)
        .build()
    {
        Ok(req) => req,
        Err(e) => {
            let _ = tx.send(StreamEvent::Error(format!("failed to build request: {e}")));
            return;
        }
    };

    let mut stream = match client.chat().create_stream(request).await {
        Ok(s) => s,
        Err(e) => {
            let _ = tx.send(StreamEvent::Error(format!("request failed: {e}")));
            return;
        }
    };

    while let Some(item) = stream.next().await {
        match item {
            Ok(response) => {
                if let Some(content) = response
                    .choices
                    .first()
                    .and_then(|choice| choice.delta.content.clone())
                    && !content.is_empty()
                    && tx.send(StreamEvent::Token(content)).is_err()
                {
                    // Receiver dropped (UI closed); stop streaming.
                    return;
                }
            }
            Err(e) => {
                let _ = tx.send(StreamEvent::Error(format!("stream error: {e}")));
                return;
            }
        }
    }

    let _ = tx.send(StreamEvent::Done);
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
