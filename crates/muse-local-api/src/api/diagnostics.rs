//! 本地诊断接口，包含路由注册和 HTTP 适配实现。

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{Json, Router, extract::State, routing::get};

use super::ApiRouter;
use crate::dto::{DiagnosticsConnectivityItem, DiagnosticsConnectivityResponse};
use crate::state::AppState;

pub(super) fn routes() -> ApiRouter {
    Router::new().route(
        "/diagnostics/connectivity",
        get(handle_diagnostics_connectivity),
    )
}

/// 执行设置中心连通性诊断。
async fn handle_diagnostics_connectivity(
    State(state): State<Arc<AppState>>,
) -> Json<DiagnosticsConnectivityResponse> {
    let (chat_base, tts_base, asr_base) = {
        let store = state.model_config.lock().await;
        (
            store.chat().api_base.clone(),
            store.tts().api_base.clone(),
            store.speech_recognition().api_base.clone(),
        )
    };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build();

    let mut checks = Vec::new();
    match client {
        Ok(client) => {
            checks.push(probe_http_connectivity(&client, "chat", "对话模型 API", chat_base).await);
            checks.push(probe_http_connectivity(&client, "tts", "TTS API", tts_base).await);
            checks.push(probe_http_connectivity(&client, "asr", "语音识别 API", asr_base).await);
        }
        Err(err) => {
            checks.push(diagnostics_failed_item(
                "chat",
                "对话模型 API",
                chat_base,
                format!("创建诊断客户端失败：{err}"),
            ));
            checks.push(diagnostics_failed_item(
                "tts",
                "TTS API",
                tts_base,
                format!("创建诊断客户端失败：{err}"),
            ));
            checks.push(diagnostics_failed_item(
                "asr",
                "语音识别 API",
                asr_base,
                format!("创建诊断客户端失败：{err}"),
            ));
        }
    }

    Json(DiagnosticsConnectivityResponse { checks })
}

async fn probe_http_connectivity(
    client: &reqwest::Client,
    id: &str,
    label: &str,
    target: String,
) -> DiagnosticsConnectivityItem {
    let target = target.trim().trim_end_matches('/').to_string();
    if target.is_empty() {
        return DiagnosticsConnectivityItem {
            id: id.to_string(),
            label: label.to_string(),
            target: "未配置".to_string(),
            status: "skipped".to_string(),
            latency_ms: None,
            message: "未配置 API Base，跳过检测。".to_string(),
        };
    }

    let started_at = Instant::now();
    match client.get(&target).send().await {
        Ok(response) => DiagnosticsConnectivityItem {
            id: id.to_string(),
            label: label.to_string(),
            target,
            status: "reachable".to_string(),
            latency_ms: Some(started_at.elapsed().as_millis() as u64),
            message: format!("服务已响应：HTTP {}。", response.status()),
        },
        Err(err) => DiagnosticsConnectivityItem {
            id: id.to_string(),
            label: label.to_string(),
            target,
            status: "failed".to_string(),
            latency_ms: Some(started_at.elapsed().as_millis() as u64),
            message: format!("连接失败：{err}"),
        },
    }
}

fn diagnostics_failed_item(
    id: &str,
    label: &str,
    target: String,
    message: String,
) -> DiagnosticsConnectivityItem {
    DiagnosticsConnectivityItem {
        id: id.to_string(),
        label: label.to_string(),
        target: if target.trim().is_empty() {
            "未配置".to_string()
        } else {
            target
        },
        status: "failed".to_string(),
        latency_ms: None,
        message,
    }
}
