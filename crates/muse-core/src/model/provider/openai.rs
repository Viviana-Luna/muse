//! OpenAI 兼容聊天模型 provider，负责把内部会话和工具定义转换为上游聊天协议。

use super::{
    ChatModelError, ChatModelProvider, ChatModelResult, ChatStreamEvent, ChatStreamResult,
};
use crate::domain::conversation::{Conversation, Role};
use crate::domain::tool::{ToolCall, ToolCallSource, ToolDef};
use crate::domain::usage::{ProviderTokenUsage, TokenUsageSource};
use crate::model::config::LlmConfig;
use crate::model::profile::chat_completions_request_options;
use async_trait::async_trait;
use std::collections::BTreeMap;

/// Chat Completions 协议适配器，只承载 DeepSeek 与火山方舟 Agent Plan 的公共传输逻辑。
pub struct OpenAiProvider {
    config: LlmConfig,
    client: reqwest::Client,
}

#[derive(Default)]
struct StreamingToolCallState {
    name: Option<String>,
    arguments: String,
    call_id: Option<String>,
    active: bool,
}

impl OpenAiProvider {
    /// 创建 OpenAI 兼容 provider 实例。
    pub fn new(config: LlmConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .expect("创建聊天模型 HTTP 客户端失败");
        Self { config, client }
    }

    fn api_key(&self) -> ChatModelResult {
        self.config
            .api_key
            .clone()
            .or_else(|| std::env::var("LLM_API_KEY").ok())
            .ok_or_else(|| {
                ChatModelError::ConfigError(
                    "未配置 LLM_API_KEY，请在模型配置文件或 LLM_API_KEY 环境变量中提供。".into(),
                )
            })
    }

    fn apply_chat_completions_stream_options(&self, body: &mut serde_json::Value) {
        let options =
            chat_completions_request_options(&self.config.provider, &self.config.api_base);
        if options.include_stream_usage {
            body["stream_options"] = serde_json::json!({ "include_usage": true });
        }
    }

    fn configured_provider_name(&self) -> &'static str {
        match self.config.provider.trim().to_ascii_lowercase().as_str() {
            "deepseek" => "deepseek",
            "volcengine_agent_plan" => "volcengine_agent_plan",
            _ => "unsupported",
        }
    }

    fn openai_tools(tools: &[ToolDef]) -> Vec<serde_json::Value> {
        tools
            .iter()
            .map(|tool| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters,
                    }
                })
            })
            .collect()
    }

    fn attach_chat_completions_tools(&self, body: &mut serde_json::Value, tools: &[ToolDef]) {
        let native_tools = Self::openai_tools(tools);
        if native_tools.is_empty() {
            return;
        }
        body["tools"] = serde_json::Value::Array(native_tools);
        body["tool_choice"] = serde_json::json!("auto");
        // 两个正式供应商都未声明 `parallel_tool_calls` 扩展参数，不能发送该字段。
        // 响应侧仍会拒绝单响应内的多个工具调用，保持串行工具续轮边界。
    }

    fn chat_completions_messages(conversation: &Conversation) -> Vec<serde_json::Value> {
        let mut messages = Vec::<serde_json::Value>::new();
        for message in conversation.api_messages() {
            let value = match message.role {
                Role::Assistant if message.tool_call_id.is_some() => {
                    let mut value = serde_json::json!({
                        "role": "assistant",
                        "content": serde_json::Value::Null,
                        "tool_calls": [{
                                "id": message.tool_call_id.clone().unwrap_or_default(),
                                "type": "function",
                                "function": {
                                    "name": message.tool_name.clone().unwrap_or_default(),
                                "arguments": Self::tool_arguments_text(message.tool_arguments.as_ref()),
                            }
                        }]
                    });
                    Self::attach_reasoning_content(
                        &mut value,
                        message.reasoning_content.as_deref(),
                    );
                    value
                }
                Role::Tool => serde_json::json!({
                    "role": "tool",
                    "tool_call_id": message.tool_call_id.clone().unwrap_or_default(),
                    "content": message.content.clone(),
                }),
                _ => {
                    serde_json::json!({
                        "role": message.role.to_string(),
                        "content": message.content.clone(),
                    })
                }
            };

            // Chat Completions 的同一助手响应可以同时包含可见文本、推理和一个
            // 工具调用。运行时按事件保存成相邻消息；续轮时合并回原生形态，
            // 避免把同一响应伪造成两个助手回合。
            if message.role == Role::Assistant
                && message.tool_call_id.is_some()
                && let Some(previous) = messages.last_mut()
                && previous.get("role").and_then(serde_json::Value::as_str) == Some("assistant")
                && previous.get("tool_calls").is_none()
            {
                previous["tool_calls"] = value["tool_calls"].clone();
                if let Some(reasoning) = value.get("reasoning_content") {
                    previous["reasoning_content"] = reasoning.clone();
                }
                continue;
            }
            messages.push(value);
        }
        messages
    }

    fn attach_reasoning_content(value: &mut serde_json::Value, reasoning_content: Option<&str>) {
        let Some(reasoning_content) = reasoning_content else {
            return;
        };
        if reasoning_content.trim().is_empty() {
            return;
        }
        value["reasoning_content"] = serde_json::json!(reasoning_content);
    }

    fn tool_arguments_text(arguments: Option<&serde_json::Value>) -> String {
        match arguments {
            Some(value) => serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string()),
            None => "{}".to_string(),
        }
    }

    async fn upstream_http_error(response: reqwest::Response) -> ChatModelError {
        let status = response.status();
        match response.json::<serde_json::Value>().await {
            Ok(payload) if ChatModelError::from_payload_if_error(&payload).is_some() => {
                ChatModelError::from_upstream_error_payload(&payload)
            }
            _ => ChatModelError::from_upstream_status(status),
        }
    }

    fn parse_chat_completions_event(data: &str) -> Result<serde_json::Value, ChatModelError> {
        serde_json::from_str(data)
            .map_err(|_| ChatModelError::protocol_error("模型 API 返回了无效的 SSE JSON 事件。"))
    }

    fn usage_number(value: &serde_json::Value, path: &[&str]) -> u64 {
        let mut cursor = value;
        for key in path {
            let Some(next) = cursor.get(*key) else {
                return 0;
            };
            cursor = next;
        }
        cursor.as_u64().unwrap_or(0)
    }

    fn sum_number_fields(value: Option<&serde_json::Value>) -> u64 {
        match value {
            Some(serde_json::Value::Number(number)) => number.as_u64().unwrap_or(0),
            Some(serde_json::Value::Array(items)) => items
                .iter()
                .map(|item| Self::sum_number_fields(Some(item)))
                .sum(),
            Some(serde_json::Value::Object(map)) => map
                .values()
                .map(|item| Self::sum_number_fields(Some(item)))
                .sum(),
            _ => 0,
        }
    }

    fn parse_usage(value: &serde_json::Value) -> Option<ProviderTokenUsage> {
        let usage = value.get("usage").unwrap_or(value);
        if usage.is_null() || !usage.is_object() {
            return None;
        }

        let reported_input_tokens = Self::usage_number(usage, &["input_tokens"])
            .max(Self::usage_number(usage, &["prompt_tokens"]));
        let output_tokens = Self::usage_number(usage, &["output_tokens"])
            .max(Self::usage_number(usage, &["completion_tokens"]));
        let detail_cache_creation_input_tokens = Self::usage_number(
            usage,
            &["input_tokens_details", "cache_creation_input_tokens"],
        )
        .max(Self::usage_number(
            usage,
            &["prompt_tokens_details", "cache_creation_input_tokens"],
        ));
        let detail_cached_tokens =
            Self::usage_number(usage, &["input_tokens_details", "cached_tokens"]).max(
                Self::usage_number(usage, &["prompt_tokens_details", "cached_tokens"]),
            );
        let cache_creation_input_tokens =
            Self::usage_number(usage, &["cache_creation_input_tokens"])
                .max(Self::usage_number(usage, &["cache_creation_tokens"]))
                .max(detail_cache_creation_input_tokens);
        let prompt_cache_hit_tokens = Self::usage_number(usage, &["prompt_cache_hit_tokens"]);
        let prompt_cache_miss_tokens = Self::usage_number(usage, &["prompt_cache_miss_tokens"]);
        let cache_read_input_tokens = Self::usage_number(usage, &["cache_read_input_tokens"])
            .max(detail_cached_tokens)
            .max(prompt_cache_hit_tokens);
        // OpenAI 的 cached_tokens 是 prompt/input_tokens 的明细子集，不是额外 token。
        // 只有来自 *_tokens_details 的缓存字段需要从 input_tokens 中扣除；
        // top-level cache_read_input_tokens 兼容 Anthropic 风格，保持原样。
        // DeepSeek 的 prompt_cache_hit_tokens / prompt_cache_miss_tokens 是 prompt_tokens
        // 的命中/未命中拆分：命中部分展示为 cache_read，未命中部分展示为 input。
        let has_prompt_cache_breakdown =
            prompt_cache_hit_tokens > 0 || prompt_cache_miss_tokens > 0;
        let input_tokens = if has_prompt_cache_breakdown {
            if prompt_cache_miss_tokens > 0 {
                prompt_cache_miss_tokens
            } else {
                reported_input_tokens
                    .saturating_sub(cache_read_input_tokens)
                    .saturating_sub(cache_creation_input_tokens)
            }
        } else {
            reported_input_tokens
                .saturating_sub(detail_cached_tokens)
                .saturating_sub(detail_cache_creation_input_tokens)
        };
        let reasoning_tokens = Self::usage_number(usage, &["reasoning_tokens"])
            .max(Self::usage_number(
                usage,
                &["output_tokens_details", "reasoning_tokens"],
            ))
            .max(Self::usage_number(
                usage,
                &["completion_tokens_details", "reasoning_tokens"],
            ));
        let server_tool_tokens = Self::usage_number(usage, &["server_tool_tokens"])
            .max(Self::sum_number_fields(usage.get("server_tool_use")));
        let reported_total_tokens = Self::usage_number(usage, &["total_tokens"]);
        let computed_total_tokens = input_tokens
            .saturating_add(output_tokens)
            .saturating_add(cache_creation_input_tokens)
            .saturating_add(cache_read_input_tokens)
            .saturating_add(server_tool_tokens);
        let total_tokens = if reported_total_tokens > 0 {
            reported_total_tokens
        } else {
            computed_total_tokens
        };

        if total_tokens == 0
            && input_tokens == 0
            && output_tokens == 0
            && cache_creation_input_tokens == 0
            && cache_read_input_tokens == 0
        {
            return None;
        }

        Some(ProviderTokenUsage {
            input_tokens,
            output_tokens,
            cache_creation_input_tokens,
            cache_read_input_tokens,
            reasoning_tokens,
            server_tool_tokens,
            total_tokens,
            source: TokenUsageSource::ProviderReported,
            raw_usage: Some(usage.clone()),
        })
    }

    fn collect_chat_completions_tool_deltas(
        value: &serde_json::Value,
        states: &mut BTreeMap<u64, StreamingToolCallState>,
    ) -> Result<(), ChatModelError> {
        let Some(tool_calls) = value
            .pointer("/choices/0/delta/tool_calls")
            .and_then(serde_json::Value::as_array)
        else {
            return Ok(());
        };
        for tool_call_delta in tool_calls {
            let index = tool_call_delta
                .get("index")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let state = states.entry(index).or_default();
            state.active = true;
            if let Some(id) = tool_call_delta
                .get("id")
                .and_then(serde_json::Value::as_str)
            {
                if state
                    .call_id
                    .as_deref()
                    .is_some_and(|current| current != id)
                {
                    return Err(ChatModelError::protocol_error(
                        "模型 API 在同一工具调用位置返回了冲突的 call_id。",
                    ));
                }
                state.call_id.get_or_insert_with(|| id.to_string());
            }
            if let Some(function) = tool_call_delta.get("function") {
                if let Some(name) = function.get("name").and_then(serde_json::Value::as_str) {
                    if state.name.as_deref().is_some_and(|current| current != name) {
                        return Err(ChatModelError::protocol_error(
                            "模型 API 在同一工具调用位置返回了冲突的工具名称。",
                        ));
                    }
                    state.name.get_or_insert_with(|| name.to_string());
                }
                if let Some(arguments) = function
                    .get("arguments")
                    .and_then(serde_json::Value::as_str)
                {
                    state.arguments.push_str(arguments);
                }
            }
        }
        Ok(())
    }

    fn finalize_chat_completions_tool_call(
        states: &mut BTreeMap<u64, StreamingToolCallState>,
    ) -> Result<Option<ToolCall>, ChatModelError> {
        let mut active = std::mem::take(states)
            .into_values()
            .filter(|state| state.active)
            .collect::<Vec<_>>();
        if active.len() > 1 {
            return Err(ChatModelError::protocol_error(
                "模型 API 返回了多个并行工具调用，当前运行时拒绝执行。",
            ));
        }
        let Some(state) = active.pop() else {
            return Ok(None);
        };
        let call_id = state
            .call_id
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| ChatModelError::protocol_error("模型 API 工具调用缺少 call_id。"))?;
        let name = state
            .name
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| ChatModelError::protocol_error("模型 API 工具调用缺少 name。"))?;
        let arguments = serde_json::from_str::<serde_json::Value>(&state.arguments)
            .map_err(|_| ChatModelError::protocol_error("模型 API 工具调用参数不是合法 JSON。"))?;
        Ok(Some(ToolCall {
            call_id,
            name,
            arguments,
            source: ToolCallSource::Native,
        }))
    }
}

#[async_trait]
impl ChatModelProvider for OpenAiProvider {
    fn name(&self) -> &'static str {
        self.configured_provider_name()
    }

    async fn chat(&self, conversation: &Conversation) -> ChatModelResult {
        let api_key = self.api_key()?;

        let messages = Self::chat_completions_messages(conversation);

        let body = serde_json::json!({
            "model": self.config.model,
            "messages": messages,
            "max_tokens": self.config.max_tokens,
            "temperature": self.config.temperature,
        });

        let response = self
            .client
            .post(format!(
                "{}/chat/completions",
                self.config.api_base.trim_end_matches('/')
            ))
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(ChatModelError::from_network_error)?;

        if !response.status().is_success() {
            return Err(Self::upstream_http_error(response).await);
        }

        let data: serde_json::Value = response
            .json()
            .await
            .map_err(|_| ChatModelError::protocol_error("解析模型响应失败。"))?;

        if let Some(error) = ChatModelError::from_payload_if_error(&data) {
            return Err(error);
        }

        let content = data["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| ChatModelError::ApiError("模型 API 返回格式不符合预期。".into()))?
            .to_string();

        Ok(content)
    }

    async fn chat_stream(&self, conversation: &Conversation) -> ChatStreamResult {
        self.chat_stream_with_tools(conversation, &[]).await
    }

    async fn chat_stream_with_tools(
        &self,
        conversation: &Conversation,
        tools: &[ToolDef],
    ) -> ChatStreamResult {
        use futures::StreamExt;
        use futures::stream;
        use tokio::sync::mpsc;

        let api_key = match self.api_key() {
            Ok(key) => key,
            Err(e) => return Box::pin(stream::once(async { Err(e) })),
        };

        let messages = Self::chat_completions_messages(conversation);

        let mut body = serde_json::json!({
            "model": self.config.model,
            "messages": messages,
            "max_tokens": self.config.max_tokens,
            "temperature": self.config.temperature,
            "stream": true,
        });
        self.apply_chat_completions_stream_options(&mut body);
        self.attach_chat_completions_tools(&mut body, tools);

        let endpoint = format!(
            "{}/chat/completions",
            self.config.api_base.trim_end_matches('/')
        );

        let response = match self
            .client
            .post(&endpoint)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return Box::pin(stream::once(async move {
                    Err(ChatModelError::from_network_error(e))
                }));
            }
        };

        if !response.status().is_success() {
            let error = Self::upstream_http_error(response).await;
            return Box::pin(stream::once(async move { Err(error) }));
        }

        let (tx, rx) = mpsc::channel::<Result<ChatStreamEvent, ChatModelError>>(64);
        let mut byte_stream = response.bytes_stream();

        tokio::spawn(async move {
            let mut buffer = String::new();
            let mut tool_call_states = BTreeMap::<u64, StreamingToolCallState>::new();
            loop {
                let chunk_result = tokio::select! {
                    _ = tx.closed() => return,
                    chunk_result = byte_stream.next() => chunk_result,
                };
                let Some(chunk_result) = chunk_result else {
                    break;
                };
                match chunk_result {
                    Ok(bytes) => {
                        buffer.push_str(&String::from_utf8_lossy(&bytes));
                        while let Some(pos) = buffer.find('\n') {
                            let line = buffer[..pos].trim().to_string();
                            buffer = buffer[pos + 1..].to_string();
                            if let Some(data) = line.strip_prefix("data: ") {
                                let data = data.trim().to_string();
                                if data == "[DONE]" {
                                    match Self::finalize_chat_completions_tool_call(
                                        &mut tool_call_states,
                                    ) {
                                        Ok(Some(call)) => {
                                            if tx
                                                .send(Ok(ChatStreamEvent::ToolCall(call)))
                                                .await
                                                .is_err()
                                            {
                                                return;
                                            }
                                        }
                                        Ok(None) => {}
                                        Err(error) => {
                                            let _ = tx.send(Err(error)).await;
                                            return;
                                        }
                                    }
                                    let _ = tx.send(Ok(ChatStreamEvent::Done)).await;
                                    return;
                                }
                                match Self::parse_chat_completions_event(&data) {
                                    Ok(val) => {
                                        if let Some(error) =
                                            ChatModelError::from_payload_if_error(&val)
                                        {
                                            let _ = tx.send(Err(error)).await;
                                            return;
                                        }
                                        if let Some(usage) = Self::parse_usage(&val)
                                            && tx
                                                .send(Ok(ChatStreamEvent::Usage(usage)))
                                                .await
                                                .is_err()
                                        {
                                            return;
                                        }
                                        let choice = &val["choices"][0];
                                        let delta = &choice["delta"];
                                        if let Some(reasoning) = delta["reasoning_content"]
                                            .as_str()
                                            .or_else(|| delta["reasoning"].as_str())
                                            && tx
                                                .send(Ok(ChatStreamEvent::Reasoning(
                                                    reasoning.to_string(),
                                                )))
                                                .await
                                                .is_err()
                                        {
                                            return;
                                        }
                                        if let Some(content) = delta["content"].as_str() {
                                            // 接收端（SSE 消费方）断开时 tx.send 返回 Err，
                                            // 立即停止读取上游 HTTP 流，避免前端停止后后端继续拉取。
                                            if tx
                                                .send(Ok(ChatStreamEvent::Text(
                                                    content.to_string(),
                                                )))
                                                .await
                                                .is_err()
                                            {
                                                return;
                                            }
                                        }
                                        if let Err(error) =
                                            Self::collect_chat_completions_tool_deltas(
                                                &val,
                                                &mut tool_call_states,
                                            )
                                        {
                                            let _ = tx.send(Err(error)).await;
                                            return;
                                        }
                                        if choice["finish_reason"].as_str() == Some("tool_calls") {
                                            match Self::finalize_chat_completions_tool_call(
                                                &mut tool_call_states,
                                            ) {
                                                Ok(Some(call)) => {
                                                    if tx
                                                        .send(Ok(ChatStreamEvent::ToolCall(call)))
                                                        .await
                                                        .is_err()
                                                    {
                                                        return;
                                                    }
                                                }
                                                Ok(None) => {
                                                    let _ = tx
                                                    .send(Err(ChatModelError::protocol_error(
                                                        "模型 API 以工具调用结束，但未返回工具调用。",
                                                    )))
                                                    .await;
                                                    return;
                                                }
                                                Err(error) => {
                                                    let _ = tx.send(Err(error)).await;
                                                    return;
                                                }
                                            }
                                            let _ = tx.send(Ok(ChatStreamEvent::Done)).await;
                                            return;
                                        }
                                    }
                                    Err(error) => {
                                        let _ = tx.send(Err(error)).await;
                                        return;
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(ChatModelError::from_network_error(e))).await;
                        return;
                    }
                }
            }
            match Self::finalize_chat_completions_tool_call(&mut tool_call_states) {
                Ok(Some(call)) => {
                    if tx.send(Ok(ChatStreamEvent::ToolCall(call))).await.is_err() {
                        return;
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    let _ = tx.send(Err(error)).await;
                    return;
                }
            }
            let _ = tx.send(Ok(ChatStreamEvent::Done)).await;
        });

        Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
    }
}

#[cfg(test)]
mod tests {
    use super::{OpenAiProvider, StreamingToolCallState};
    use crate::domain::conversation::Conversation;
    use crate::model::provider::ChatModelProvider;
    use std::collections::BTreeMap;

    #[test]
    fn parses_chat_completions_usage_fields() {
        let payload = serde_json::json!({
            "usage": {
                "prompt_tokens": 200,
                "completion_tokens": 50,
                "total_tokens": 250,
                "prompt_tokens_details": {
                    "cached_tokens": 120
                },
                "completion_tokens_details": {
                    "reasoning_tokens": 12
                }
            }
        });

        let usage = OpenAiProvider::parse_usage(&payload).expect("应解析 Chat Completions usage");

        assert_eq!(usage.input_tokens, 80);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cache_read_input_tokens, 120);
        assert_eq!(usage.reasoning_tokens, 12);
        assert_eq!(usage.total_tokens, 250);
        assert_eq!(usage.context_input_tokens(), 200);
    }

    #[test]
    fn parses_deepseek_cache_usage_fields() {
        let payload = serde_json::json!({
            "usage": {
                "prompt_tokens": 200,
                "prompt_cache_hit_tokens": 120,
                "prompt_cache_miss_tokens": 80,
                "completion_tokens": 50,
                "completion_tokens_details": {
                    "reasoning_tokens": 16
                },
                "total_tokens": 250
            }
        });

        let usage = OpenAiProvider::parse_usage(&payload).expect("应解析 DeepSeek usage");

        assert_eq!(usage.input_tokens, 80);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cache_read_input_tokens, 120);
        assert_eq!(usage.cache_creation_input_tokens, 0);
        assert_eq!(usage.reasoning_tokens, 16);
        assert_eq!(usage.total_tokens, 250);
        assert_eq!(usage.context_input_tokens(), 200);
    }

    #[test]
    fn deepseek_streaming_requests_include_usage_chunk() {
        let provider = OpenAiProvider::new(crate::model::config::LlmConfig {
            provider: "deepseek".to_string(),
            api_base: "https://api.deepseek.com".to_string(),
            ..Default::default()
        });
        let mut body = serde_json::json!({ "stream": true });

        provider.apply_chat_completions_stream_options(&mut body);

        assert_eq!(
            body.pointer("/stream_options/include_usage")
                .and_then(|value| value.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn ignores_missing_usage() {
        let payload = serde_json::json!({ "choices": [] });

        assert!(OpenAiProvider::parse_usage(&payload).is_none());
    }

    #[test]
    fn keeps_compatible_cache_creation_fields() {
        let payload = serde_json::json!({
            "usage": {
                "input_tokens": 80,
                "output_tokens": 10,
                "cache_creation_input_tokens": 30,
                "cache_read_input_tokens": 40
            }
        });

        let usage = OpenAiProvider::parse_usage(&payload).expect("应解析兼容 usage");

        assert_eq!(usage.cache_creation_input_tokens, 30);
        assert_eq!(usage.cache_read_input_tokens, 40);
        assert_eq!(usage.total_tokens, 160);
    }

    #[test]
    fn reports_the_two_supported_provider_names() {
        let deepseek = OpenAiProvider::new(crate::model::config::LlmConfig {
            provider: "deepseek".to_string(),
            ..Default::default()
        });
        let agent_plan = OpenAiProvider::new(crate::model::config::LlmConfig {
            provider: "volcengine_agent_plan".to_string(),
            ..Default::default()
        });

        assert_eq!(deepseek.name(), "deepseek");
        assert_eq!(agent_plan.name(), "volcengine_agent_plan");
    }

    #[test]
    fn compatible_tool_requests_disable_parallel_calls_without_breaking_deepseek() {
        let tools = [crate::domain::tool::ToolDef {
            name: "weather".to_string(),
            description: "查询天气".to_string(),
            parameters: serde_json::json!({ "type": "object" }),
            category: "read_only".to_string(),
            risk: crate::domain::tool::ToolRisk::ReadOnly,
            requires_approval: false,
            execution_owner: crate::domain::tool::ToolExecutionOwner::Core,
            available: true,
            disabled_reason: None,
        }];
        let agent_plan = OpenAiProvider::new(crate::model::config::LlmConfig {
            provider: "volcengine_agent_plan".to_string(),
            ..Default::default()
        });
        let deepseek = OpenAiProvider::new(crate::model::config::LlmConfig {
            provider: "deepseek".to_string(),
            ..Default::default()
        });
        let mut agent_plan_body = serde_json::json!({});
        let mut deepseek_body = serde_json::json!({});

        agent_plan.attach_chat_completions_tools(&mut agent_plan_body, &tools);
        deepseek.attach_chat_completions_tools(&mut deepseek_body, &tools);

        assert!(agent_plan_body.get("parallel_tool_calls").is_none());
        assert!(agent_plan_body.get("tools").is_some());
        assert!(deepseek_body.get("parallel_tool_calls").is_none());
        assert!(deepseek_body.get("tools").is_some());
    }

    #[test]
    fn deepseek_serial_tool_continuation_preserves_reasoning_and_result_order() {
        let mut conversation = Conversation::new("系统提示".to_string(), 20);
        conversation.add_user_message("上海天气如何？".to_string());
        conversation.add_assistant_message("我先查询实时天气。".to_string());
        conversation.add_assistant_tool_call_with_reasoning(
            "call-weather".to_string(),
            "weather".to_string(),
            serde_json::json!({ "city": "上海" }),
            Some("需要先查询天气。".to_string()),
        );
        conversation.add_tool_result(
            "call-weather".to_string(),
            "weather".to_string(),
            "晴，25℃".to_string(),
        );

        let messages = OpenAiProvider::chat_completions_messages(&conversation);

        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(messages[2]["content"], "我先查询实时天气。");
        assert_eq!(messages[2]["reasoning_content"], "需要先查询天气。");
        assert_eq!(messages[2]["tool_calls"].as_array().map(Vec::len), Some(1));
        assert_eq!(messages[3]["role"], "tool");
        assert_eq!(messages[3]["tool_call_id"], "call-weather");
    }

    #[test]
    fn ordinary_deepseek_history_omits_previous_reasoning_content() {
        let mut conversation = Conversation::new("系统提示".to_string(), 20);
        conversation.add_user_message("第一轮问题".to_string());
        conversation.add_assistant_message_with_reasoning(
            "第一轮回答".to_string(),
            Some("不应进入普通下一轮请求的推理内容".to_string()),
        );
        conversation.add_user_message("第二轮问题".to_string());

        let messages = OpenAiProvider::chat_completions_messages(&conversation);

        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(messages[2]["content"], "第一轮回答");
        assert!(messages[2].get("reasoning_content").is_none());
    }

    #[test]
    fn invalid_sse_json_is_a_protocol_error() {
        let error = OpenAiProvider::parse_chat_completions_event("{not-json")
            .expect_err("无效 SSE JSON 必须失败关闭");

        assert_eq!(error.code(), "provider_protocol_error");
        assert!(!error.retryable());
        assert!(!error.to_string().contains("not-json"));
    }

    #[test]
    fn single_streamed_tool_fixture_is_reassembled_once() {
        let events: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("fixtures/openai_chat_single_tool.json"))
                .expect("fixture 应为合法 JSON");
        let mut states = BTreeMap::<u64, StreamingToolCallState>::new();

        for event in &events {
            OpenAiProvider::collect_chat_completions_tool_deltas(event, &mut states)
                .expect("单工具分片应能合并");
        }
        let call = OpenAiProvider::finalize_chat_completions_tool_call(&mut states)
            .expect("单工具调用应通过")
            .expect("应生成工具调用");

        assert_eq!(call.call_id, "call-weather");
        assert_eq!(call.name, "weather");
        assert_eq!(call.arguments, serde_json::json!({ "city": "上海" }));
    }

    #[test]
    fn parallel_chat_tool_fixture_fails_closed_before_emitting_a_call() {
        let events: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("fixtures/openai_chat_parallel_tools.json"))
                .expect("fixture 应为合法 JSON");
        let mut states = BTreeMap::<u64, StreamingToolCallState>::new();
        for event in &events {
            OpenAiProvider::collect_chat_completions_tool_deltas(event, &mut states)
                .expect("并行分片本身应可收集");
        }

        let error = OpenAiProvider::finalize_chat_completions_tool_call(&mut states)
            .expect_err("多个调用必须拒绝");

        assert_eq!(error.code(), "provider_protocol_error");
        assert!(error.to_string().contains("拒绝执行"));
    }
}
