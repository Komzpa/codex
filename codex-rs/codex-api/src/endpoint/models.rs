use crate::auth::SharedAuthProvider;
use crate::endpoint::session::EndpointSession;
use crate::error::ApiError;
use crate::provider::Provider;
use codex_client::HttpTransport;
use codex_client::RequestTelemetry;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelsResponse;
use http::HeaderMap;
use http::Method;
use http::header::ETAG;
use std::sync::Arc;

const MAX_MODELS_DECODE_ERROR_DETAIL_BYTES: usize = 256;

pub struct ModelsClient<T: HttpTransport> {
    session: EndpointSession<T>,
}

impl<T: HttpTransport> ModelsClient<T> {
    pub fn new(transport: T, provider: Provider, auth: SharedAuthProvider) -> Self {
        Self {
            session: EndpointSession::new(transport, provider, auth),
        }
    }

    pub fn with_telemetry(self, request: Option<Arc<dyn RequestTelemetry>>) -> Self {
        Self {
            session: self.session.with_request_telemetry(request),
        }
    }

    fn path() -> &'static str {
        "models"
    }

    fn append_client_version_query(req: &mut codex_client::Request, client_version: &str) {
        let separator = if req.url.contains('?') { '&' } else { '?' };
        req.url = format!("{}{}client_version={client_version}", req.url, separator);
    }

    pub fn request_url(provider: &Provider, client_version: &str) -> String {
        let mut request = provider.build_request(Method::GET, Self::path());
        Self::append_client_version_query(&mut request, client_version);
        request.url
    }

    pub async fn list_models(
        &self,
        request_url: String,
        extra_headers: HeaderMap,
    ) -> Result<(Vec<ModelInfo>, Option<String>), ApiError> {
        let resp = self
            .session
            .execute_with(
                Method::GET,
                Self::path(),
                extra_headers,
                /*body*/ None,
                move |req| {
                    req.url.clone_from(&request_url);
                },
            )
            .await?;

        let header_etag = resp
            .headers
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string);

        let body_len = resp.body.len();
        let ModelsResponse { models } = serde_json::from_slice::<ModelsResponse>(&resp.body)
            .map_err(|e| {
                let detail = e.to_string();
                let safe_detail = if detail.len() <= MAX_MODELS_DECODE_ERROR_DETAIL_BYTES {
                    detail
                } else {
                    format!(
                        "{:?} error at line {} column {}; diagnostic omitted ({} bytes)",
                        e.classify(),
                        e.line(),
                        e.column(),
                        detail.len()
                    )
                };
                ApiError::Stream(format!(
                    "failed to decode models response: {safe_detail}; body length: {body_len} bytes"
                ))
            })?;

        Ok((models, header_etag))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthProvider;
    use crate::provider::RetryConfig;
    use codex_client::Request;
    use codex_client::Response;
    use codex_client::StreamResponse;
    use codex_client::TransportError;
    use http::HeaderMap;
    use http::StatusCode;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::time::Duration;

    #[derive(Clone)]
    struct CapturingTransport {
        last_request: Arc<Mutex<Option<Request>>>,
        body: Arc<ModelsResponse>,
        etag: Option<String>,
    }

    impl Default for CapturingTransport {
        fn default() -> Self {
            Self {
                last_request: Arc::new(Mutex::new(None)),
                body: Arc::new(ModelsResponse { models: Vec::new() }),
                etag: None,
            }
        }
    }

    impl HttpTransport for CapturingTransport {
        async fn execute(&self, req: Request) -> Result<Response, TransportError> {
            *self.last_request.lock().unwrap() = Some(req);
            let body = serde_json::to_vec(&*self.body).unwrap();
            let mut headers = HeaderMap::new();
            if let Some(etag) = &self.etag {
                headers.insert(ETAG, etag.parse().unwrap());
            }
            Ok(Response {
                status: StatusCode::OK,
                headers,
                body: body.into(),
            })
        }

        async fn stream(&self, _req: Request) -> Result<StreamResponse, TransportError> {
            Err(TransportError::Build("stream should not run".to_string()))
        }
    }

    #[derive(Clone)]
    struct StaticBodyTransport {
        body: Arc<Vec<u8>>,
    }

    impl HttpTransport for StaticBodyTransport {
        async fn execute(&self, _req: Request) -> Result<Response, TransportError> {
            Ok(Response {
                status: StatusCode::OK,
                headers: HeaderMap::new(),
                body: self.body.as_ref().clone().into(),
            })
        }

        async fn stream(&self, _req: Request) -> Result<StreamResponse, TransportError> {
            Err(TransportError::Build("stream should not run".to_string()))
        }
    }

    #[derive(Clone, Default)]
    struct DummyAuth;

    impl AuthProvider for DummyAuth {
        fn add_auth_headers(&self, _headers: &mut HeaderMap) {}
    }

    fn provider(base_url: &str) -> Provider {
        Provider {
            name: "test".to_string(),
            base_url: base_url.to_string(),
            query_params: None,
            headers: HeaderMap::new(),
            retry: RetryConfig {
                max_attempts: 1,
                base_delay: Duration::from_millis(1),
                retry_429: false,
                retry_5xx: true,
                retry_transport: true,
            },
            stream_idle_timeout: Duration::from_secs(1),
        }
    }

    #[tokio::test]
    async fn appends_client_version_query() {
        let response = ModelsResponse { models: Vec::new() };

        let transport = CapturingTransport {
            last_request: Arc::new(Mutex::new(None)),
            body: Arc::new(response),
            etag: None,
        };

        let provider = provider("https://example.com/api/codex");
        let request_url = ModelsClient::<CapturingTransport>::request_url(&provider, "0.99.0");
        let client = ModelsClient::new(transport.clone(), provider, Arc::new(DummyAuth));

        let (models, _) = client
            .list_models(request_url, HeaderMap::new())
            .await
            .expect("request should succeed");

        assert_eq!(models.len(), 0);

        let url = transport
            .last_request
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .url
            .clone();
        assert_eq!(
            url,
            "https://example.com/api/codex/models?client_version=0.99.0"
        );
    }

    #[tokio::test]
    async fn parses_models_response() {
        let response = ModelsResponse {
            models: vec![
                serde_json::from_value(json!({
                    "slug": "gpt-test",
                    "display_name": "gpt-test",
                    "description": "desc",
                    "default_reasoning_level": "medium",
                    "supported_reasoning_levels": [{"effort": "low", "description": "low"}, {"effort": "medium", "description": "medium"}, {"effort": "high", "description": "high"}],
                    "shell_type": "shell_command",
                    "visibility": "list",
                    "minimal_client_version": [0, 99, 0],
                    "supported_in_api": true,
                    "priority": 1,
                    "upgrade": null,
                    "support_verbosity": false,
                    "default_verbosity": null,
                    "apply_patch_tool_type": null,
                    "truncation_policy": {"mode": "bytes", "limit": 10_000},
                    "supports_image_detail_original": false,
                    "context_window": 272_000,
                    "experimental_supported_tools": [],
                }))
                .unwrap(),
            ],
        };

        let transport = CapturingTransport {
            last_request: Arc::new(Mutex::new(None)),
            body: Arc::new(response),
            etag: None,
        };

        let provider = provider("https://example.com/api/codex");
        let request_url = ModelsClient::<CapturingTransport>::request_url(&provider, "0.99.0");
        let client = ModelsClient::new(transport, provider, Arc::new(DummyAuth));

        let (models, _) = client
            .list_models(request_url, HeaderMap::new())
            .await
            .expect("request should succeed");

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].slug, "gpt-test");
        assert_eq!(models[0].supported_in_api, true);
        assert_eq!(models[0].priority, 1);
    }

    #[tokio::test]
    async fn list_models_includes_etag() {
        let response = ModelsResponse { models: Vec::new() };

        let transport = CapturingTransport {
            last_request: Arc::new(Mutex::new(None)),
            body: Arc::new(response),
            etag: Some("\"abc\"".to_string()),
        };

        let provider = provider("https://example.com/api/codex");
        let request_url = ModelsClient::<CapturingTransport>::request_url(&provider, "0.1.0");
        let client = ModelsClient::new(transport, provider, Arc::new(DummyAuth));

        let (models, etag) = client
            .list_models(request_url, HeaderMap::new())
            .await
            .expect("request should succeed");

        assert_eq!(models.len(), 0);
        assert_eq!(etag, Some("\"abc\"".to_string()));
    }

    #[tokio::test]
    async fn malformed_models_response_error_excludes_large_body() {
        let marker = "UNIQUE_MODELS_RESPONSE_MARKER_MUST_NOT_ESCAPE";
        let body = format!(
            r#"{{"models":[{{"slug":"{marker}{}"}}]}}"#,
            "x".repeat(1_000_000)
        )
        .into_bytes();
        let body_len = body.len();
        let transport = StaticBodyTransport {
            body: Arc::new(body),
        };
        let provider = provider("https://example.com/api/codex");
        let request_url = ModelsClient::<StaticBodyTransport>::request_url(&provider, "0.1.0");
        let client = ModelsClient::new(transport, provider, Arc::new(DummyAuth));

        let error = client
            .list_models(request_url, HeaderMap::new())
            .await
            .expect_err("malformed models response should fail");
        let message = error.to_string();

        assert!(message.contains("missing field `display_name`"));
        assert!(message.contains(&format!("body length: {body_len} bytes")));
        assert!(!message.contains(marker));
        assert!(message.len() < 512);
    }

    #[tokio::test]
    async fn malformed_models_response_error_omits_large_serde_diagnostic() {
        let marker = "UNIQUE_MODELS_ENUM_MARKER_MUST_NOT_ESCAPE";
        let invalid_mode = format!("{marker}{}", "z".repeat(1_000_000));
        let body = serde_json::to_vec(&json!({
            "models": [{
                "slug": "gpt-test",
                "display_name": "gpt-test",
                "description": "desc",
                "default_reasoning_level": "medium",
                "supported_reasoning_levels": [{"effort": "medium", "description": "medium"}],
                "shell_type": "shell_command",
                "visibility": "list",
                "minimal_client_version": [0, 99, 0],
                "supported_in_api": true,
                "priority": 1,
                "upgrade": null,
                "base_instructions": "base instructions",
                "support_verbosity": false,
                "default_verbosity": null,
                "apply_patch_tool_type": null,
                "truncation_policy": {"mode": invalid_mode, "limit": 10_000},
                "supports_parallel_tool_calls": false,
                "supports_image_detail_original": false,
                "context_window": 272_000,
                "experimental_supported_tools": [],
            }],
        }))
        .expect("serialize malformed catalog");
        let body_len = body.len();
        let transport = StaticBodyTransport {
            body: Arc::new(body),
        };
        let provider = provider("https://example.com/api/codex");
        let request_url = ModelsClient::<StaticBodyTransport>::request_url(&provider, "0.1.0");
        let client = ModelsClient::new(transport, provider, Arc::new(DummyAuth));

        let error = client
            .list_models(request_url, HeaderMap::new())
            .await
            .expect_err("unknown truncation mode should fail");
        let message = error.to_string();

        assert!(message.contains("Data error at line"));
        assert!(message.contains("diagnostic omitted"));
        assert!(message.contains(&format!("body length: {body_len} bytes")));
        assert!(!message.contains(marker));
        assert!(message.len() < 512);
    }
}
