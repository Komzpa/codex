use super::*;
use codex_http_client::OutboundProxyPolicy;
use codex_model_provider::BearerAuthProvider;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

fn provider(server: &MockServer) -> GoalQuotaProvider {
    let mut info = ModelProviderInfo {
        base_url: Some(server.uri()),
        usage_url: Some(format!("{}/quota", server.uri()).into()),
        ..Default::default()
    };
    info.name = "fixture".to_string();
    GoalQuotaProvider::new_with_auth_provider(
        &info,
        server.uri(),
        Arc::new(BearerAuthProvider::new("fixture-key".to_string())),
        "acct-fixture".to_string(),
        HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
    )
}

fn body(account: &str, used_percent: u64) -> serde_json::Value {
    serde_json::json!({
        "account_id": account,
        "plan_type": "plus",
        "rate_limit": {
            "allowed": true,
            "limit_reached": false,
            "primary_window": {
                "used_percent": used_percent,
                "limit_window_seconds": 300,
                "reset_after_seconds": 60,
                "reset_at": 2_000_000_000
            }
        }
    })
}

#[tokio::test]
async fn fresh_cache_reuses_one_passive_read() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/quota"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-codex-primary-used-percent", "12")
                .set_body_json(body("acct-a", 12)),
        )
        .expect(1)
        .mount(&server)
        .await;
    let quota = provider(&server);

    let first = quota.snapshot_many().await.unwrap();
    let second = quota.snapshot_many().await.unwrap();
    assert_eq!(first, second);
    assert_eq!(first[0].source, "provider-key");
    assert_eq!(first.len(), 2);
}

#[tokio::test]
async fn expired_cache_refreshes_key_and_pool_independently() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/quota"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-codex-primary-used-percent", "12")
                .set_body_json(body("acct-a", 12)),
        )
        .mount(&server)
        .await;
    let quota = provider(&server);
    let initial = quota.snapshot_many().await.unwrap();
    let initial_key = initial
        .iter()
        .find(|snapshot| snapshot.source == "provider-key")
        .unwrap();
    assert_eq!(initial_key.scope_id, "acct-a");

    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/quota"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-codex-primary-used-percent", "78")
                .set_body_json(body("acct-b", 78)),
        )
        .mount(&server)
        .await;
    {
        let mut cache = quota.cache.lock().await;
        cache.as_mut().unwrap().fetched = Instant::now() - CACHE_TTL;
    }
    let refreshed = quota.snapshot_many().await.unwrap();
    assert_eq!(
        refreshed
            .iter()
            .find(|snapshot| snapshot.source == "provider-key")
            .unwrap()
            .scope_id,
        "acct-b"
    );
    assert_eq!(
        refreshed
            .iter()
            .find(|snapshot| snapshot.source == "provider-pool")
            .unwrap()
            .limits[0]
            .primary
            .as_ref()
            .unwrap()
            .used_percent,
        78.0
    );
}

#[tokio::test]
async fn changed_identity_invalidates_cache_even_before_ttl() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/quota"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body("acct-a", 12)))
        .mount(&server)
        .await;
    let quota = provider(&server);
    let _ = quota.snapshot_many().await.unwrap();
    {
        let mut cache = quota.cache.lock().await;
        cache.as_mut().unwrap().identity = "different-account".to_string();
    }

    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/quota"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body("acct-b", 34)))
        .expect(1)
        .mount(&server)
        .await;
    let refreshed = quota.snapshot_many().await.unwrap();
    assert_eq!(
        refreshed
            .iter()
            .find(|snapshot| snapshot.source == "provider-key")
            .unwrap()
            .scope_id,
        "acct-b"
    );
}

#[tokio::test]
async fn failed_read_returns_error_without_fabricated_quota() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/quota"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let quota = provider(&server);
    let result = quota.snapshot_many().await;
    assert!(result.is_err());
}

#[tokio::test]
async fn response_observation_is_available_without_passive_usage_url() {
    let server = MockServer::start().await;
    let info = ModelProviderInfo {
        base_url: Some(server.uri()),
        usage_url: None,
        ..Default::default()
    };
    let quota = GoalQuotaProvider::new_with_auth_provider(
        &info,
        server.uri(),
        Arc::new(BearerAuthProvider::new("fixture-key".to_string())),
        "acct-fixture".to_string(),
        HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
    );
    let limit = serde_json::from_value(serde_json::json!({
        "limit_id": "codex",
        "primary": {"used_percent": 44.0, "window_minutes": 15}
    }))
    .unwrap();
    quota.observe_response(limit).await;
    let snapshots = quota.snapshot_many().await.unwrap();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].source, "provider-pool");
    assert_eq!(
        snapshots[0].limits[0]
            .primary
            .as_ref()
            .unwrap()
            .used_percent,
        44.0
    );
}

#[tokio::test]
async fn empty_headers_do_not_create_fabricated_pool_snapshot() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/quota"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "account_id": "acct-a", "plan_type": "plus"
        })))
        .mount(&server)
        .await;
    let quota = provider(&server);
    assert!(quota.snapshot_many().await.unwrap().is_empty());
}
