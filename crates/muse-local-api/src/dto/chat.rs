//! 聊天接口的请求与响应 DTO。

use serde::{Deserialize, Serialize};

/// 普通聊天请求体。
#[derive(Deserialize)]
pub struct ChatRequest {
    pub message: String,
    #[serde(default)]
    pub selected_skill: Option<String>,
}

/// 流式聊天请求体。
#[derive(Deserialize)]
pub struct ChatStreamRequest {
    pub message: String,
    pub conversation_id: String,
    pub client_request_id: String,
    #[serde(default)]
    pub voice_enabled: Option<bool>,
    #[serde(default)]
    pub selected_skill: Option<String>,
}

/// 当前角色在新回合中可选择的有效 Skill 目录。
#[derive(Serialize)]
pub struct RuntimeSkillCatalogResponse {
    pub skills: Vec<muse_core::domain::turn::RuntimeSkillCatalogEntry>,
    pub omitted_skill_count: usize,
}

/// WebSocket 握手短票据查询参数。
#[derive(Deserialize)]
pub struct WsTicketQuery {
    #[serde(default)]
    pub ticket: Option<String>,
}

/// 普通聊天响应体。
#[derive(Serialize)]
pub struct ChatResponse {
    pub reply: String,
}
