use anyhow::Result;
use async_openai::types::chat::{
    ChatCompletionRequestUserMessage, CreateChatCompletionRequestArgs,
};

use crate::state::AppState;

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
