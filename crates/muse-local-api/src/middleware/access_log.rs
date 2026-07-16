use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::extract::{OriginalUri, Request};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use tracing::Instrument;

static REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, PartialEq, Eq)]
enum AccessLogLevel {
    Info,
    Warn,
    Error,
}

/// 记录 API 请求的统一访问日志，不采集查询参数、请求头或请求体。
pub(crate) async fn log_api_request(request: Request, next: Next) -> Response {
    let request_id = REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let method = request.method().clone();
    let path = request
        .extensions()
        .get::<OriginalUri>()
        .map(|uri| uri.0.path())
        .unwrap_or_else(|| request.uri().path())
        .to_string();
    let span = tracing::info_span!(
        target: "agent_vp::http",
        "http_request",
        request_id,
        method = %method,
        path = %path,
    );

    async move {
        let started_at = Instant::now();
        let response = next.run(request).await;
        log_response(
            request_id,
            &method,
            &path,
            response.status(),
            started_at.elapsed(),
        );
        response
    }
    .instrument(span)
    .await
}

fn log_response(
    request_id: u64,
    method: &Method,
    path: &str,
    status: StatusCode,
    latency: Duration,
) {
    let status = status.as_u16();
    let latency_ms = format_latency_ms(latency);

    match access_log_level(status) {
        AccessLogLevel::Info => tracing::info!(
            target: "agent_vp::http",
            request_id,
            method = %method,
            path,
            status,
            latency_ms = %latency_ms,
            "HTTP 请求完成"
        ),
        AccessLogLevel::Warn => tracing::warn!(
            target: "agent_vp::http",
            request_id,
            method = %method,
            path,
            status,
            latency_ms = %latency_ms,
            "HTTP 请求完成"
        ),
        AccessLogLevel::Error => tracing::error!(
            target: "agent_vp::http",
            request_id,
            method = %method,
            path,
            status,
            latency_ms = %latency_ms,
            "HTTP 请求完成"
        ),
    }
}

fn format_latency_ms(latency: Duration) -> String {
    format!("{:.3}", latency.as_secs_f64() * 1_000.0)
}

fn access_log_level(status: u16) -> AccessLogLevel {
    match status {
        500..=599 => AccessLogLevel::Error,
        400..=499 => AccessLogLevel::Warn,
        _ => AccessLogLevel::Info,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{AccessLogLevel, access_log_level, format_latency_ms};

    #[test]
    fn maps_http_status_to_expected_log_level() {
        assert_eq!(access_log_level(200), AccessLogLevel::Info);
        assert_eq!(access_log_level(302), AccessLogLevel::Info);
        assert_eq!(access_log_level(400), AccessLogLevel::Warn);
        assert_eq!(access_log_level(404), AccessLogLevel::Warn);
        assert_eq!(access_log_level(500), AccessLogLevel::Error);
        assert_eq!(access_log_level(503), AccessLogLevel::Error);
    }

    #[test]
    fn formats_latency_with_three_decimal_places() {
        assert_eq!(format_latency_ms(Duration::from_micros(90)), "0.090");
        assert_eq!(format_latency_ms(Duration::from_micros(8_316)), "8.316");
    }
}
