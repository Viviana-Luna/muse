//! DeepSeek 专属供应商能力。

use serde::Deserialize;

use crate::model::profile::DEEPSEEK_PROVIDER_PROFILE;

use super::{ProviderBalance, ProviderBalanceInfo, ProviderSupportError};

#[derive(Deserialize)]
struct DeepSeekBalancePayload {
    is_available: bool,
    #[serde(default)]
    balance_infos: Vec<ProviderBalanceInfo>,
}

pub(crate) async fn fetch_balance(
    api_base: &str,
    api_key: &str,
) -> Result<ProviderBalance, ProviderSupportError> {
    fetch_balance_with_timeout(api_base, api_key, std::time::Duration::from_secs(30)).await
}

async fn fetch_balance_with_timeout(
    api_base: &str,
    api_key: &str,
    timeout: std::time::Duration,
) -> Result<ProviderBalance, ProviderSupportError> {
    let api_key = api_key.trim();
    if api_key.is_empty() {
        return Err(ProviderSupportError::Config(
            "DeepSeek 余额检测需要 API Key。".to_string(),
        ));
    }

    let endpoint = balance_url(api_base);
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|_| ProviderSupportError::Network("无法创建供应商诊断请求。".to_string()))?;
    let response = client
        .get(&endpoint)
        .bearer_auth(api_key)
        .send()
        .await
        .map_err(|error| {
            ProviderSupportError::Network(if error.is_timeout() {
                "供应商诊断请求超时，请稍后重试。".to_string()
            } else {
                "无法连接供应商诊断接口，请检查网络与 API 地址。".to_string()
            })
        })?;
    let status = response.status();
    if !status.is_success() {
        return Err(ProviderSupportError::Api {
            status: status.as_u16(),
            message: format!("DeepSeek 余额接口返回 HTTP {status}。"),
        });
    }
    let body = response
        .text()
        .await
        .map_err(|_| ProviderSupportError::Network("读取供应商诊断响应失败。".to_string()))?;

    parse_balance_body(&body)
}

fn parse_balance_body(body: &str) -> Result<ProviderBalance, ProviderSupportError> {
    let payload: DeepSeekBalancePayload =
        serde_json::from_str(body).map_err(|err| ProviderSupportError::Parse(err.to_string()))?;
    Ok(ProviderBalance {
        provider_id: DEEPSEEK_PROVIDER_PROFILE.id.to_string(),
        is_available: payload.is_available,
        balance_infos: payload.balance_infos,
    })
}

fn balance_url(api_base: &str) -> String {
    format!("{}/user/balance", deepseek_api_root(api_base))
}

fn deepseek_api_root(api_base: &str) -> String {
    let mut base = api_base.trim().trim_end_matches('/').to_string();
    if base.is_empty() {
        return DEEPSEEK_PROVIDER_PROFILE.default_api_base.to_string();
    }
    for suffix in ["/v1", "/beta"] {
        if base.ends_with(suffix) {
            base.truncate(base.len() - suffix.len());
            break;
        }
    }
    base
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "live-tests")]
    use super::fetch_balance_with_timeout;
    use super::{balance_url, parse_balance_body};
    #[cfg(feature = "live-tests")]
    use crate::model::vendor::ProviderSupportError;
    #[cfg(feature = "live-tests")]
    use std::time::Duration;

    #[cfg(feature = "live-tests")]
    async fn spawn_balance_server(status_line: &str, body: &str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("应能绑定测试监听地址");
        let addr = listener.local_addr().expect("应能读取测试监听地址");
        let response = format!(
            "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("应能接受测试连接");
            let mut request = [0u8; 1024];
            let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut request).await;
            tokio::io::AsyncWriteExt::write_all(&mut socket, response.as_bytes())
                .await
                .expect("应能写入测试响应");
        });
        format!("http://{addr}")
    }

    #[test]
    fn balance_url_uses_deepseek_root_endpoint() {
        assert_eq!(
            balance_url("https://api.deepseek.com/v1/"),
            "https://api.deepseek.com/user/balance"
        );
        assert_eq!(
            balance_url("https://api.deepseek.com/beta"),
            "https://api.deepseek.com/user/balance"
        );
        assert_eq!(
            balance_url("https://gateway.example/deepseek"),
            "https://gateway.example/deepseek/user/balance"
        );
    }

    #[test]
    fn parses_deepseek_balance_payload() {
        let balance = parse_balance_body(
            r#"{
                "is_available": true,
                "balance_infos": [{
                    "currency": "CNY",
                    "total_balance": "110.00",
                    "granted_balance": "10.00",
                    "topped_up_balance": "100.00"
                }]
            }"#,
        )
        .expect("应解析 DeepSeek 余额响应");

        assert_eq!(balance.provider_id, "deepseek");
        assert!(balance.is_available);
        assert_eq!(balance.balance_infos.len(), 1);
        assert_eq!(balance.balance_infos[0].currency, "CNY");
        assert_eq!(balance.balance_infos[0].total_balance, "110.00");
    }

    #[test]
    fn parses_unavailable_balance_without_inventing_amounts() {
        let balance = parse_balance_body(r#"{"is_available":false,"balance_infos":[]}"#)
            .expect("余额不足响应仍应是合法供应商结果");

        assert!(!balance.is_available);
        assert!(balance.balance_infos.is_empty());
    }

    #[tokio::test]
    #[cfg(feature = "live-tests")]
    async fn maps_rate_limit_without_exposing_response_body() {
        let origin = spawn_balance_server(
            "429 Too Many Requests",
            r#"{"error":{"message":"secret=should-not-leak"}}"#,
        )
        .await;

        let error = fetch_balance_with_timeout(&origin, "test-key", Duration::from_secs(1))
            .await
            .expect_err("429 应返回稳定错误");

        assert!(matches!(
            error,
            ProviderSupportError::Api { status: 429, .. }
        ));
        assert!(!error.to_string().contains("should-not-leak"));
    }

    #[tokio::test]
    #[cfg(feature = "live-tests")]
    async fn rejects_non_json_balance_response() {
        let origin = spawn_balance_server("200 OK", "not-json").await;

        let error = fetch_balance_with_timeout(&origin, "test-key", Duration::from_secs(1))
            .await
            .expect_err("非 JSON 余额响应必须失败");

        assert!(matches!(error, ProviderSupportError::Parse(_)));
    }

    #[tokio::test]
    #[cfg(feature = "live-tests")]
    async fn maps_stalled_balance_request_to_timeout() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("应能绑定测试监听地址");
        let addr = listener.local_addr().expect("应能读取测试监听地址");
        tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.expect("应能接受测试连接");
            tokio::time::sleep(Duration::from_secs(1)).await;
        });

        let error = fetch_balance_with_timeout(
            &format!("http://{addr}"),
            "test-key",
            Duration::from_millis(20),
        )
        .await
        .expect_err("超时请求必须失败");

        assert!(
            matches!(error, ProviderSupportError::Network(message) if message.contains("超时"))
        );
    }
}
