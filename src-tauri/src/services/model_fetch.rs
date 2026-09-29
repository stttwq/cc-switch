//! 模型列表获取服务
//!
//! 通过 OpenAI 兼容的 GET /v1/models 端点获取供应商可用模型列表。
//! 主要面向第三方聚合站（硅基流动、OpenRouter 等），以及把 Anthropic
//! 协议挂在兼容子路径上的官方供应商（DeepSeek、Kimi、智谱 GLM 等）。

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

/// 获取到的模型信息
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchedModel {
    pub id: String,
    pub owned_by: Option<String>,
}

/// 传给前端的错误载荷（SEC-C）
///
/// 只含稳定 code、重试标志与可选 HTTP 状态码；禁止把 reqwest 错误的
/// 原始 Display 送往前端——其 Display 会附带完整请求 URL（可能含
/// 查询参数中的 token 等敏感内容）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelFetchError {
    pub code: String,
    pub retryable: bool,
    pub status: Option<u16>,
}

impl ModelFetchError {
    fn new(code: &str, retryable: bool) -> Self {
        Self {
            code: code.to_string(),
            retryable,
            status: None,
        }
    }

    fn with_status(code: &str, retryable: bool, status: StatusCode) -> Self {
        Self {
            code: code.to_string(),
            retryable,
            status: Some(status.as_u16()),
        }
    }
}

/// OpenAI 兼容的 /v1/models 响应格式
#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Option<Vec<ModelEntry>>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
    owned_by: Option<String>,
}

const FETCH_TIMEOUT_SECS: u64 = 15;
const MAX_REQUEST_HEADERS: usize = 64;
const MAX_HEADER_NAME_BYTES: usize = 256;
const MAX_HEADER_VALUE_BYTES: usize = 16 * 1024;

/// 404/405 响应体截断长度：避免把几十 KB HTML 404 页整页保留到错误串里。
const ERROR_BODY_MAX_CHARS: usize = 512;

/// 已知的「Anthropic 协议兼容子路径」后缀；按长度降序，最长前缀优先匹配。
/// baseURL 命中这些后缀时，候选列表会追加「剥离后缀再拼 /v1/models / /models」的版本。
const KNOWN_COMPAT_SUFFIXES: &[&str] = &[
    "/api/claudecode",
    "/api/anthropic",
    "/apps/anthropic",
    "/api/coding",
    "/claudecode",
    "/anthropic",
    "/step_plan",
    "/coding",
    "/claude",
];

/// 获取供应商的可用模型列表
///
/// 使用 OpenAI 兼容的 GET /v1/models 端点，按候选列表顺序尝试。
pub async fn fetch_models(
    base_url: &str,
    api_key: &str,
    is_full_url: bool,
    models_url_override: Option<&str>,
    api_format: Option<&str>,
    request_headers: Option<&BTreeMap<String, String>>,
) -> Result<Vec<FetchedModel>, ModelFetchError> {
    let client = crate::services::http_client::get_no_redirect_client();
    fetch_models_with_client(
        client,
        base_url,
        api_key,
        is_full_url,
        models_url_override,
        api_format,
        request_headers,
    )
    .await
}

/// 用指定客户端获取模型列表（测试可注入直连/无重定向客户端）
pub async fn fetch_models_with_client(
    client: Client,
    base_url: &str,
    api_key: &str,
    is_full_url: bool,
    models_url_override: Option<&str>,
    api_format: Option<&str>,
    request_headers: Option<&BTreeMap<String, String>>,
) -> Result<Vec<FetchedModel>, ModelFetchError> {
    // SEC-B：override 与 Base URL 跨源时，拒绝携带凭据向另一目标发请求
    if let Some(raw) = models_url_override {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            ensure_override_same_origin(base_url, trimmed).map_err(|e| {
                log::debug!("[ModelFetch] {e}");
                ModelFetchError::new("cross_origin_override", false)
            })?;
        }
    }
    let candidates = build_models_url_candidates(base_url, is_full_url, models_url_override)
        .map_err(|e| {
            log::debug!("[ModelFetch] {e}");
            ModelFetchError::new("invalid_url", false)
        })?;
    for url in &candidates {
        validate_candidate_url(url).map_err(|e| {
            log::debug!("[ModelFetch] Rejected candidate URL: {e}");
            ModelFetchError::new("invalid_url", false)
        })?;
    }
    let headers = build_model_fetch_headers(api_key, api_format, request_headers).map_err(|e| {
        log::debug!("[ModelFetch] Rejected request headers: {e}");
        ModelFetchError::new("invalid_request", false)
    })?;
    let mut last_err: Option<ModelFetchError> = None;
    let mut known_secrets = vec![api_key.to_string()];
    if let Some(request_headers) = request_headers {
        known_secrets.extend(request_headers.values().cloned());
    }

    for url in &candidates {
        log::debug!(
            "[ModelFetch] Trying endpoint: {}",
            crate::url_for_log_with_secrets(url, &known_secrets)
        );
        let request = client
            .get(url)
            .headers(headers.clone())
            .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS));
        let response = match request.send().await {
            Ok(r) => r,
            // SEC-C：按错误类别映射为稳定 code，不把原始 Display（含 URL）送出
            Err(e) => {
                let err = if e.is_timeout() {
                    ModelFetchError::new("timeout", true)
                } else if e.is_connect() {
                    ModelFetchError::new("connect_failed", true)
                } else if e.is_request() {
                    ModelFetchError::new("request_failed", true)
                } else {
                    ModelFetchError::new("network_failed", true)
                };
                log::debug!(
                    "[ModelFetch] Request error ({}): {}",
                    err.code,
                    crate::url_for_log_with_secrets(url, &known_secrets)
                );
                return Err(err);
            }
        };

        let status = response.status();

        // SEC-B：重定向策略为 none，3xx 会原样返回；一律不跟随，
        // 也不把 Location（可能含目标地址上的 token）带回前端
        if status.is_redirection() {
            log::debug!(
                "[ModelFetch] Redirect blocked at {}",
                crate::url_for_log_with_secrets(url, &known_secrets)
            );
            return Err(ModelFetchError::new("redirect_blocked", false));
        }

        if status.is_success() {
            let resp: ModelsResponse = response.json().await.map_err(|e| {
                log::debug!("[ModelFetch] Failed to parse response: {e}");
                ModelFetchError::new("parse_failed", false)
            })?;

            let mut models: Vec<FetchedModel> = resp
                .data
                .unwrap_or_default()
                .into_iter()
                .map(|m| FetchedModel {
                    id: m.id,
                    owned_by: m.owned_by,
                })
                .collect();

            models.sort_by(|a, b| a.id.cmp(&b.id));
            return Ok(models);
        }

        if status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED {
            log_error_body(response, status, &known_secrets).await;
            last_err = Some(ModelFetchError::with_status(
                "endpoint_not_found",
                false,
                status,
            ));
            continue;
        }

        log_error_body(response, status, &known_secrets).await;
        return Err(ModelFetchError::with_status("http_error", false, status));
    }

    Err(last_err.unwrap_or_else(|| ModelFetchError::new("all_candidates_failed", false)))
}

/// 读取并按 debug 级别记录（脱敏+截断后的）错误响应体
///
/// 正文只进本机 debug 日志，不进入 IPC 错误载荷（SEC-C）。
async fn log_error_body(response: reqwest::Response, status: StatusCode, known_secrets: &[String]) {
    let body = response.text().await.unwrap_or_default();
    log::debug!(
        "[ModelFetch] HTTP {status} body: {}",
        redact_model_fetch_error_body(body, known_secrets)
    );
}

/// 校验模型端点候选 URL（SEC-B §5.2-5）
///
/// 仅允许 http/https；禁止 userinfo（`user:pass@host`）——凭据只能
/// 走请求头，不允许藏在 URL 里被代理/日志/重定向二次扩散。
fn validate_candidate_url(raw: &str) -> Result<(), String> {
    let parsed = url::Url::parse(raw).map_err(|e| format!("Invalid URL: {e}"))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => return Err(format!("Unsupported scheme: {other}")),
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("URL must not contain userinfo credentials".to_string());
    }
    Ok(())
}

/// 比较 URL 的源（scheme + host + 有效端口）
fn url_origin(u: &url::Url) -> Option<(String, String, Option<u16>)> {
    Some((
        u.scheme().to_string(),
        u.host_str()?.to_string(),
        u.port_or_known_default(),
    ))
}

/// override 与 Base URL 跨源时拒绝（SEC-B §5.2-5）
///
/// 默认不携当前凭据向另一源发请求；要求用户把 Base URL 改到目标源。
/// 双方都能解析为合法 URL 时才做比较，比较按 URL 语义而非前缀。
fn ensure_override_same_origin(base_url: &str, override_url: &str) -> Result<(), String> {
    let base = url::Url::parse(base_url.trim())
        .ok()
        .and_then(|u| url_origin(&u));
    let over = url::Url::parse(override_url)
        .ok()
        .and_then(|u| url_origin(&u));
    if let (Some(b), Some(o)) = (base, over) {
        if b != o {
            return Err(
                "Models URL override points to a different origin than the Base URL; \
                 credentials are not sent cross-origin. Update the Base URL to the \
                 target origin instead."
                    .to_string(),
            );
        }
    }
    Ok(())
}

fn redact_model_fetch_error_body(body: String, known_secrets: &[String]) -> String {
    truncate_body(crate::redact_known_secrets_strict(&body, known_secrets))
}

fn build_model_fetch_headers(
    api_key: &str,
    api_format: Option<&str>,
    request_headers: Option<&BTreeMap<String, String>>,
) -> Result<HeaderMap, String> {
    let custom_count = request_headers.map_or(0, BTreeMap::len);
    if api_key.is_empty() && custom_count == 0 {
        return Err("API Key or request headers are required to fetch models".to_string());
    }
    if custom_count > MAX_REQUEST_HEADERS {
        return Err(format!(
            "Too many model-fetch request headers (maximum {MAX_REQUEST_HEADERS})"
        ));
    }

    let mut headers = HeaderMap::new();
    if !api_key.is_empty() {
        let (name, value) = match api_format {
            Some("anthropic-messages") => (
                HeaderName::from_static("x-api-key"),
                HeaderValue::from_str(api_key)
                    .map_err(|error| format!("Invalid API Key header value: {error}"))?,
            ),
            Some("google-generative-ai") => (
                HeaderName::from_static("x-goog-api-key"),
                HeaderValue::from_str(api_key)
                    .map_err(|error| format!("Invalid API Key header value: {error}"))?,
            ),
            _ => (
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {api_key}"))
                    .map_err(|error| format!("Invalid API Key header value: {error}"))?,
            ),
        };
        headers.insert(name, value);
    }

    if let Some(request_headers) = request_headers {
        for (raw_name, raw_value) in request_headers {
            let name = raw_name.trim();
            if name.is_empty() || name.len() > MAX_HEADER_NAME_BYTES {
                return Err(format!("Invalid model-fetch header name: {raw_name}"));
            }
            if raw_value.len() > MAX_HEADER_VALUE_BYTES {
                return Err(format!("Model-fetch header value is too large: {name}"));
            }
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|error| format!("Invalid model-fetch header name {name}: {error}"))?;
            let value = HeaderValue::from_str(raw_value)
                .map_err(|error| format!("Invalid model-fetch header value for {name}: {error}"))?;
            headers.insert(name, value);
        }
    }

    Ok(headers)
}

/// 构造「模型列表端点」的候选 URL 列表
///
/// 候选顺序：
/// 1. `models_url_override` 非空 → 只返回它
/// 2. baseURL 拼 `/v1/models`；若已以版本段 `/v{N}` 结尾（`/v1`、智谱
///    `/api/coding/paas/v4` 等），版本号已在路径里，改拼 `/models`
/// 3. 版本段非 `/v1`（如 `/v4`）时再追加 `/v1/models` 作为兜底次候选
/// 4. 若 baseURL 命中 [`KNOWN_COMPAT_SUFFIXES`]，剥离后缀再拼 `/v1/models`、`/models`
///
/// 结果已去重且保持首次出现顺序。
pub fn build_models_url_candidates(
    base_url: &str,
    is_full_url: bool,
    models_url_override: Option<&str>,
) -> Result<Vec<String>, String> {
    if let Some(raw) = models_url_override {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Ok(vec![trimmed.to_string()]);
        }
    }

    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("Base URL is empty".to_string());
    }

    let mut candidates: Vec<String> = Vec::new();

    if is_full_url {
        if let Some(idx) = trimmed.find("/v1/") {
            candidates.push(format!("{}/v1/models", &trimmed[..idx]));
        } else if let Some(idx) = trimmed.rfind('/') {
            let root = &trimmed[..idx];
            if root.contains("://") && root.len() > root.find("://").unwrap() + 3 {
                candidates.push(format!("{root}/v1/models"));
            }
        }
        if candidates.is_empty() {
            return Err("Cannot derive models endpoint from full URL".to_string());
        }
        return Ok(candidates);
    }

    // baseURL 已以版本段 /v{N} 结尾时（如 `/v1`、智谱 `/api/coding/paas/v4`），
    // OpenAI 惯例的模型端点是 `{base}/models`，不能再补 `/v1`
    // （否则 .../coding/paas/v4/v1/models → 404）。
    if ends_with_version_segment(trimmed) {
        candidates.push(format!("{trimmed}/models"));
        // 版本段非 /v1 时，保留旧的 /v1/models 作为兜底次候选（正确路径已在前）。
        if !trimmed.ends_with("/v1") {
            candidates.push(format!("{trimmed}/v1/models"));
        }
    } else {
        candidates.push(format!("{trimmed}/v1/models"));
    }

    if let Some(stripped) = strip_compat_suffix(trimmed) {
        let root = stripped.trim_end_matches('/');
        if !root.is_empty() && root.contains("://") {
            candidates.push(format!("{root}/v1/models"));
            candidates.push(format!("{root}/models"));
        }
    }

    // 候选最多 3 条，线性去重即可，不值得上 HashSet。
    let mut unique: Vec<String> = Vec::with_capacity(candidates.len());
    for url in candidates {
        if !unique.iter().any(|u| u == &url) {
            unique.push(url);
        }
    }

    Ok(unique)
}

/// 截断响应体到 [`ERROR_BODY_MAX_CHARS`] 字符，避免 HTML 404 页占用错误串。
fn truncate_body(body: String) -> String {
    if body.chars().count() <= ERROR_BODY_MAX_CHARS {
        body
    } else {
        let mut s: String = body.chars().take(ERROR_BODY_MAX_CHARS).collect();
        s.push('…');
        s
    }
}

/// 若 baseURL 以任一已知兼容子路径结尾，返回剥离后的剩余部分；否则 `None`。
///
/// 依赖 [`KNOWN_COMPAT_SUFFIXES`] 按长度降序排列，确保最长前缀优先命中
/// （否则 `/anthropic` 会提前匹配掉 `/api/anthropic` 的场景）。
fn strip_compat_suffix(base_url: &str) -> Option<&str> {
    for suffix in KNOWN_COMPAT_SUFFIXES {
        if base_url.ends_with(*suffix) {
            return Some(&base_url[..base_url.len() - suffix.len()]);
        }
    }
    None
}

/// 判断 baseURL 是否以 OpenAI 风格的版本段 `/v{N}` 结尾（`N` 为一个或多个数字），
/// 例如 `/v1`、`.../paas/v4`。这类 URL 版本号已在路径中，模型端点应为
/// `{base}/models`，不能再补 `/v1`（智谱 Coding Plan 即 `.../coding/paas/v4`）。
fn ends_with_version_segment(url: &str) -> bool {
    let last = url.rsplit('/').next().unwrap_or("");
    last.strip_prefix('v')
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_fetch_headers_follow_pi_api_format() {
        let anthropic =
            build_model_fetch_headers("anthropic-key", Some("anthropic-messages"), None).unwrap();
        assert_eq!(anthropic["x-api-key"], "anthropic-key");
        assert!(!anthropic.contains_key(AUTHORIZATION));

        let google =
            build_model_fetch_headers("google-key", Some("google-generative-ai"), None).unwrap();
        assert_eq!(google["x-goog-api-key"], "google-key");
        assert!(!google.contains_key(AUTHORIZATION));

        let openai =
            build_model_fetch_headers("openai-key", Some("openai-responses"), None).unwrap();
        assert_eq!(openai[AUTHORIZATION], "Bearer openai-key");
    }

    #[test]
    fn model_fetch_headers_allow_validated_header_only_auth_and_overrides() {
        let custom = BTreeMap::from([
            ("Authorization".to_string(), "Token literal".to_string()),
            ("X-Tenant".to_string(), "tenant-a".to_string()),
        ]);
        let headers =
            build_model_fetch_headers("", Some("openai-completions"), Some(&custom)).unwrap();
        assert_eq!(headers[AUTHORIZATION], "Token literal");
        assert_eq!(headers["x-tenant"], "tenant-a");

        let override_default =
            BTreeMap::from([("x-api-key".to_string(), "header-managed-key".to_string())]);
        let headers = build_model_fetch_headers(
            "provider-key",
            Some("anthropic-messages"),
            Some(&override_default),
        )
        .unwrap();
        assert_eq!(headers["x-api-key"], "header-managed-key");
    }

    /// 供应商自配的 User-Agent 走 requestHeaders 这一条路投递——曾经的
    /// `custom_user_agent` 独立入参只是它的重复通路（§7.1 custom_user_agent 面已删）。
    #[test]
    fn model_fetch_headers_pass_through_configured_user_agent() {
        let custom = BTreeMap::from([("User-Agent".to_string(), "pi-test-agent/1.0".to_string())]);
        let headers =
            build_model_fetch_headers("key", Some("openai-completions"), Some(&custom)).unwrap();
        assert_eq!(headers["user-agent"], "pi-test-agent/1.0");
    }

    #[test]
    fn model_fetch_headers_reject_invalid_or_missing_credentials() {
        assert!(build_model_fetch_headers("", None, None).is_err());
        let invalid = BTreeMap::from([("bad header".to_string(), "literal-value".to_string())]);
        assert!(build_model_fetch_headers("", None, Some(&invalid)).is_err());
    }

    #[test]
    fn model_fetch_error_body_redacts_known_header_credentials() {
        let secrets = vec![
            "short".to_string(),
            "Bearer literal-header-secret".to_string(),
        ];
        let body = redact_model_fetch_error_body(
            "invalid short / Bearer literal-header-secret".to_string(),
            &secrets,
        );
        assert_eq!(body, "invalid [REDACTED] / [REDACTED]");
    }

    /// SEC-B §5.2-5：仅允许 http/https
    #[test]
    fn candidate_url_validation_rejects_non_http_schemes() {
        assert!(validate_candidate_url("https://api.example.com/v1/models").is_ok());
        assert!(validate_candidate_url("http://127.0.0.1:8080/v1/models").is_ok());
        assert!(validate_candidate_url("ftp://api.example.com/v1/models").is_err());
        assert!(validate_candidate_url("file:///etc/passwd").is_err());
        assert!(validate_candidate_url("not a url").is_err());
    }

    /// SEC-B §5.2-5：禁止 userinfo，凭据只允许走请求头
    #[test]
    fn candidate_url_validation_rejects_userinfo() {
        assert!(validate_candidate_url("http://user:pass@127.0.0.1:8080/v1/models").is_err());
        assert!(validate_candidate_url("http://user@127.0.0.1:8080/v1/models").is_err());
        assert!(validate_candidate_url("https://:pass@api.example.com/v1/models").is_err());
    }

    /// SEC-B §5.2-5：跨源 override 拒绝；同源（含默认端口归一化）放行
    #[test]
    fn override_origin_check_blocks_cross_origin_and_allows_same_origin() {
        // 同 host 不同端口 = 不同源
        assert!(ensure_override_same_origin(
            "http://127.0.0.1:8000",
            "http://127.0.0.1:8001/v1/models"
        )
        .is_err());
        // HTTPS → HTTP 是跨源（scheme 不同）
        assert!(ensure_override_same_origin(
            "https://api.example.com",
            "http://api.example.com/v1/models"
        )
        .is_err());
        // 同源：默认端口归一化后一致
        assert!(ensure_override_same_origin(
            "https://api.example.com/anthropic",
            "https://api.example.com/v1/models"
        )
        .is_ok());
        assert!(ensure_override_same_origin(
            "https://api.example.com:443",
            "https://api.example.com/v1/models"
        )
        .is_ok());
        // 路径不同不影响同源判定
        assert!(ensure_override_same_origin(
            "https://api.deepseek.com/anthropic",
            "https://api.deepseek.com/models"
        )
        .is_ok());
        // Base URL 无法解析时不做比较（由候选构造/请求阶段自行失败）
        assert!(
            ensure_override_same_origin("not a url", "https://api.example.com/v1/models").is_ok()
        );
    }

    /// SEC-C：错误载荷序列化后只含 code/retryable/status
    #[test]
    fn model_fetch_error_payload_has_no_url_material() {
        let err = ModelFetchError::with_status("http_error", false, StatusCode::UNAUTHORIZED);
        let json = serde_json::to_string(&err).unwrap();
        assert_eq!(
            json,
            r#"{"code":"http_error","retryable":false,"status":401}"#
        );
    }

    // —— SEC-B/SEC-C 集成测试：本机回环假 HTTP 服务，不访问公网 ——

    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Clone)]
    struct RecordedRequest {
        method: String,
        target: String,
        headers: Vec<(String, String)>,
    }

    impl RecordedRequest {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str())
        }
    }

    /// 启动一个极简回环 HTTP 服务：每个连接读取一个请求、按 responder
    /// 生成响应并以 Connection: close 关闭。请求记录进共享 Vec。
    fn spawn_fake_server(
        responder: Arc<dyn Fn(&RecordedRequest) -> String + Send + Sync>,
    ) -> (String, Arc<Mutex<Vec<RecordedRequest>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let requests_clone = Arc::clone(&requests);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream: TcpStream = stream;
                let Some(req) = read_request(&mut stream) else {
                    continue;
                };
                requests_clone.lock().unwrap().push(req.clone());
                let response = responder(&req);
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        (format!("http://127.0.0.1:{port}"), requests)
    }

    fn read_request(stream: &mut TcpStream) -> Option<RecordedRequest> {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                Err(_) => return None,
            }
        }
        let head_end = buf
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|p| p + 4)?;
        let head = String::from_utf8_lossy(&buf[..head_end]);
        let mut lines = head.lines();
        let request_line = lines.next()?;
        let mut segs = request_line.split_whitespace();
        let method = segs.next()?.to_string();
        let target = segs.next()?.to_string();
        let headers = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(n, v)| (n.trim().to_string(), v.trim().to_string()))
            .collect();
        Some(RecordedRequest {
            method,
            target,
            headers,
        })
    }

    fn ok_models_response() -> String {
        let body = r#"{"data":[{"id":"m1","owned_by":"t"}]}"#;
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
    }

    fn redirect_response(status: u16, location: &str) -> String {
        let reason = match status {
            302 => "Found",
            307 => "Temporary Redirect",
            _ => "Permanent Redirect",
        };
        format!(
            "HTTP/1.1 {status} {reason}\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
    }

    fn no_redirect_client() -> Client {
        Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
    }

    /// SEC-B 验收：A 对 B 的 302/307/308 跳转，B 必须收到 0 个携凭据请求
    #[tokio::test]
    async fn redirect_is_not_followed_and_second_target_receives_no_credentials() {
        for status in [302u16, 307, 308] {
            let (b_base, b_requests) = spawn_fake_server(Arc::new(|_req| ok_models_response()));
            let b_models = format!("{b_base}/v1/models");
            let (a_base, a_requests) =
                spawn_fake_server(Arc::new(move |_req| redirect_response(status, &b_models)));

            let err = fetch_models_with_client(
                no_redirect_client(),
                &a_base,
                "synthetic-key-abc",
                false,
                None,
                None,
                None,
            )
            .await
            .unwrap_err();

            assert_eq!(err.code, "redirect_blocked", "status {status}");
            assert!(
                b_requests.lock().unwrap().is_empty(),
                "第二目标收到了请求 (status {status})"
            );
            let a_reqs = a_requests.lock().unwrap();
            assert_eq!(a_reqs.len(), 1, "status {status}");
            assert_eq!(a_reqs[0].method, "GET", "status {status}");
            assert_eq!(a_reqs[0].target, "/v1/models", "status {status}");
            assert_eq!(
                a_reqs[0].header("authorization"),
                Some("Bearer synthetic-key-abc"),
                "首目标应正常携带凭据 (status {status})"
            );
        }
    }

    /// SEC-B 验收：override 跨源（同 host 不同端口）默认不携 key 发请求
    #[tokio::test]
    async fn cross_origin_override_is_rejected_without_any_request() {
        let (b_base, b_requests) = spawn_fake_server(Arc::new(|_req| ok_models_response()));
        let (a_base, a_requests) = spawn_fake_server(Arc::new(|_req| ok_models_response()));
        let override_url = format!("{b_base}/v1/models");

        let err = fetch_models_with_client(
            no_redirect_client(),
            &a_base,
            "synthetic-key-abc",
            false,
            Some(&override_url),
            None,
            None,
        )
        .await
        .unwrap_err();

        assert_eq!(err.code, "cross_origin_override");
        assert!(b_requests.lock().unwrap().is_empty(), "跨源目标收到了请求");
        assert!(a_requests.lock().unwrap().is_empty(), "首目标不应被请求");
    }

    /// 同源 override 正常工作（不破坏合法用法）
    #[tokio::test]
    async fn same_origin_override_round_trips() {
        let (base, requests) = spawn_fake_server(Arc::new(|_req| ok_models_response()));
        let override_url = format!("{base}/models");

        let models = fetch_models_with_client(
            no_redirect_client(),
            &base,
            "synthetic-key-abc",
            false,
            Some(&override_url),
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "m1");
        assert_eq!(requests.lock().unwrap().len(), 1);
    }

    /// SEC-C 验收：网络失败时错误载荷不含 URL/合成秘密
    #[tokio::test]
    async fn network_error_payload_leaks_no_url_material() {
        // 拿一个空闲端口后释放 → 连接拒绝（connect_failed 分类）
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let base = format!("http://127.0.0.1:{port}/v1?token=supersecret-token");
        let err = fetch_models_with_client(
            no_redirect_client(),
            &base,
            "synthetic-key-abc",
            false,
            None,
            None,
            None,
        )
        .await
        .unwrap_err();

        assert_eq!(err.code, "connect_failed");
        let json = serde_json::to_string(&err).unwrap();
        assert!(!json.contains("supersecret"), "载荷含 URL 材料: {json}");
        assert!(!json.contains("127.0.0.1"), "载荷含主机: {json}");
    }

    #[test]
    fn test_candidates_plain_root() {
        let c = build_models_url_candidates("https://api.siliconflow.cn", false, None).unwrap();
        assert_eq!(c, vec!["https://api.siliconflow.cn/v1/models"]);
    }

    #[test]
    fn test_candidates_trailing_slash() {
        let c = build_models_url_candidates("https://api.example.com/", false, None).unwrap();
        assert_eq!(c, vec!["https://api.example.com/v1/models"]);
    }

    #[test]
    fn test_candidates_with_v1() {
        let c = build_models_url_candidates("https://api.example.com/v1", false, None).unwrap();
        assert_eq!(c, vec!["https://api.example.com/v1/models"]);
    }

    #[test]
    fn test_candidates_zhipu_coding_paas_v4() {
        // 智谱 Coding Plan 端点以 /v4 版本段结尾：模型端点是 {base}/models，
        // 正确路径必须排在 .../v4/v1/models（404）之前。
        let c =
            build_models_url_candidates("https://open.bigmodel.cn/api/coding/paas/v4", false, None)
                .unwrap();
        assert_eq!(
            c,
            vec![
                "https://open.bigmodel.cn/api/coding/paas/v4/models",
                "https://open.bigmodel.cn/api/coding/paas/v4/v1/models",
            ]
        );
    }

    #[test]
    fn test_candidates_zai_coding_paas_v4() {
        let c = build_models_url_candidates("https://api.z.ai/api/coding/paas/v4", false, None)
            .unwrap();
        assert_eq!(
            c,
            vec![
                "https://api.z.ai/api/coding/paas/v4/models",
                "https://api.z.ai/api/coding/paas/v4/v1/models",
            ]
        );
    }

    #[test]
    fn test_ends_with_version_segment() {
        assert!(ends_with_version_segment("https://x.com/v1"));
        assert!(ends_with_version_segment(
            "https://open.bigmodel.cn/api/coding/paas/v4"
        ));
        assert!(ends_with_version_segment("https://x.com/v10"));
        assert!(!ends_with_version_segment("https://x.com/api"));
        assert!(!ends_with_version_segment("https://x.com/vX"));
        assert!(!ends_with_version_segment("https://x.com/models"));
        assert!(!ends_with_version_segment("https://api.siliconflow.cn"));
    }

    #[test]
    fn test_candidates_full_url() {
        let c = build_models_url_candidates(
            "https://proxy.example.com/v1/chat/completions",
            true,
            None,
        )
        .unwrap();
        assert_eq!(c, vec!["https://proxy.example.com/v1/models"]);
    }

    #[test]
    fn test_candidates_empty() {
        assert!(build_models_url_candidates("", false, None).is_err());
    }

    #[test]
    fn test_candidates_override_returns_single() {
        let c = build_models_url_candidates(
            "https://api.deepseek.com/anthropic",
            false,
            Some("https://api.deepseek.com/models"),
        )
        .unwrap();
        assert_eq!(c, vec!["https://api.deepseek.com/models"]);
    }

    #[test]
    fn test_candidates_override_empty_falls_through() {
        let c =
            build_models_url_candidates("https://api.siliconflow.cn", false, Some("   ")).unwrap();
        assert_eq!(c, vec!["https://api.siliconflow.cn/v1/models"]);
    }

    #[test]
    fn test_candidates_deepseek_strip_anthropic() {
        let c =
            build_models_url_candidates("https://api.deepseek.com/anthropic", false, None).unwrap();
        assert_eq!(
            c,
            vec![
                "https://api.deepseek.com/anthropic/v1/models",
                "https://api.deepseek.com/v1/models",
                "https://api.deepseek.com/models",
            ]
        );
    }

    #[test]
    fn test_candidates_zhipu_strip_api_anthropic() {
        let c = build_models_url_candidates("https://open.bigmodel.cn/api/anthropic", false, None)
            .unwrap();
        assert_eq!(
            c,
            vec![
                "https://open.bigmodel.cn/api/anthropic/v1/models",
                "https://open.bigmodel.cn/v1/models",
                "https://open.bigmodel.cn/models",
            ]
        );
    }

    #[test]
    fn test_candidates_bailian_strip_apps_anthropic() {
        let c = build_models_url_candidates(
            "https://dashscope.aliyuncs.com/apps/anthropic",
            false,
            None,
        )
        .unwrap();
        assert_eq!(
            c,
            vec![
                "https://dashscope.aliyuncs.com/apps/anthropic/v1/models",
                "https://dashscope.aliyuncs.com/v1/models",
                "https://dashscope.aliyuncs.com/models",
            ]
        );
    }

    #[test]
    fn test_candidates_stepfun_strip_step_plan() {
        let c =
            build_models_url_candidates("https://api.stepfun.com/step_plan", false, None).unwrap();
        assert_eq!(
            c,
            vec![
                "https://api.stepfun.com/step_plan/v1/models",
                "https://api.stepfun.com/v1/models",
                "https://api.stepfun.com/models",
            ]
        );
    }

    #[test]
    fn test_candidates_doubao_strip_api_coding() {
        let c = build_models_url_candidates(
            "https://ark.cn-beijing.volces.com/api/coding",
            false,
            None,
        )
        .unwrap();
        assert_eq!(
            c,
            vec![
                "https://ark.cn-beijing.volces.com/api/coding/v1/models",
                "https://ark.cn-beijing.volces.com/v1/models",
                "https://ark.cn-beijing.volces.com/models",
            ]
        );
    }

    #[test]
    fn test_candidates_rightcode_strip_claude() {
        let c = build_models_url_candidates("https://www.right.codes/claude", false, None).unwrap();
        assert_eq!(
            c,
            vec![
                "https://www.right.codes/claude/v1/models",
                "https://www.right.codes/v1/models",
                "https://www.right.codes/models",
            ]
        );
    }

    #[test]
    fn test_candidates_longer_suffix_wins() {
        // baseURL 以 /api/anthropic 结尾时，应剥离整个 /api/anthropic，
        // 而不是只剥离 /anthropic（那样会得到残缺的 https://.../api 根）。
        let c = build_models_url_candidates("https://api.z.ai/api/anthropic", false, None).unwrap();
        assert_eq!(
            c,
            vec![
                "https://api.z.ai/api/anthropic/v1/models",
                "https://api.z.ai/v1/models",
                "https://api.z.ai/models",
            ]
        );
    }

    #[test]
    fn test_candidates_no_suffix_no_strip() {
        let c = build_models_url_candidates("https://openrouter.ai/api", false, None).unwrap();
        assert_eq!(c, vec!["https://openrouter.ai/api/v1/models"]);
    }

    #[test]
    fn test_candidates_deduplicate() {
        // 虚构 case：baseURL 就是 "scheme://host"，剥不出子路径，应只有一个候选。
        let c = build_models_url_candidates("https://host.example.com", false, None).unwrap();
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn test_parse_response() {
        let json = r#"{"object":"list","data":[{"id":"gpt-4","object":"model","owned_by":"openai"},{"id":"claude-3-sonnet","object":"model","owned_by":"anthropic"}]}"#;
        let resp: ModelsResponse = serde_json::from_str(json).unwrap();
        let data = resp.data.unwrap();
        assert_eq!(data.len(), 2);
        assert_eq!(data[0].id, "gpt-4");
        assert_eq!(data[0].owned_by.as_deref(), Some("openai"));
        assert_eq!(data[1].id, "claude-3-sonnet");
    }

    #[test]
    fn test_parse_response_no_owned_by() {
        let json = r#"{"object":"list","data":[{"id":"my-model","object":"model"}]}"#;
        let resp: ModelsResponse = serde_json::from_str(json).unwrap();
        let data = resp.data.unwrap();
        assert_eq!(data[0].id, "my-model");
        assert!(data[0].owned_by.is_none());
    }

    #[test]
    fn test_parse_response_empty_data() {
        let json = r#"{"object":"list","data":[]}"#;
        let resp: ModelsResponse = serde_json::from_str(json).unwrap();
        assert!(resp.data.unwrap().is_empty());
    }
}
