//! 诊断接口的响应 DTO。

use serde::Serialize;

/// 单项连通性诊断结果。
#[derive(Serialize)]
pub struct DiagnosticsConnectivityItem {
    pub id: String,
    pub label: String,
    pub target: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    pub message: String,
}

/// 连通性诊断响应体。
#[derive(Serialize)]
pub struct DiagnosticsConnectivityResponse {
    pub checks: Vec<DiagnosticsConnectivityItem>,
}
