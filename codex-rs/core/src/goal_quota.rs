//! Passive provider quota acquisition for goal accounting.

use codex_api::SharedAuthProvider;
use codex_backend_client::Client;
use codex_http_client::HttpClientFactory;
use codex_login::CodexAuth;
use codex_model_provider::SharedModelProvider;
use codex_model_provider_info::ModelProviderInfo;
use codex_protocol::goal::GoalQuotaSnapshot;
use sha1::Digest;
use sha1::Sha1;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use tokio::sync::Mutex;

const CACHE_TTL: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct GoalQuotaProvider {
    client: Client,
    runtime_provider: Option<SharedModelProvider>,
    runtime_factory: Option<HttpClientFactory>,
    source: String,
    scope_hint: String,
    usage_url: Option<String>,
    pool_scope: String,
    cache: Arc<Mutex<Option<CachedSnapshot>>>,
}

#[derive(Clone)]
struct CachedSnapshot {
    fetched: Instant,
    identity: String,
    result: Result<Vec<GoalQuotaSnapshot>, String>,
}

impl GoalQuotaProvider {
    pub fn new(
        provider: &ModelProviderInfo,
        auth: &CodexAuth,
        http_client_factory: HttpClientFactory,
    ) -> Option<Self> {
        Self::new_with_base_url(
            provider,
            provider.base_url.clone()?,
            auth,
            http_client_factory,
        )
    }

    pub fn new_with_base_url(
        provider: &ModelProviderInfo,
        base_url: String,
        auth: &CodexAuth,
        http_client_factory: HttpClientFactory,
    ) -> Option<Self> {
        Some(Self {
            client: Client::from_auth(base_url.clone(), auth, http_client_factory),
            source: if provider.usage_url.is_some() {
                "provider-key".to_string()
            } else {
                "chatgpt-account".to_string()
            },
            scope_hint: auth
                .get_account_id()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            usage_url: provider.usage_url.as_ref().map(|url| url.to_string()),
            pool_scope: format!("pool@{}", normalize_endpoint(&base_url)),
            cache: Arc::new(Mutex::new(None)),
            runtime_provider: None,
            runtime_factory: None,
        })
    }

    pub fn new_with_auth_provider(
        provider: &ModelProviderInfo,
        base_url: String,
        auth: SharedAuthProvider,
        scope_hint: String,
        http_client_factory: HttpClientFactory,
    ) -> Self {
        Self {
            client: Client::new(base_url.clone(), http_client_factory).with_auth_provider(auth),
            runtime_provider: None,
            runtime_factory: None,
            source: if provider.usage_url.is_some() {
                "provider-key"
            } else {
                "chatgpt-account"
            }
            .to_string(),
            scope_hint,
            usage_url: provider.usage_url.as_ref().map(|url| url.to_string()),
            pool_scope: format!("pool@{}", normalize_endpoint(&base_url)),
            cache: Arc::new(Mutex::new(None)),
        }
    }

    /// Construct a provider that resolves the effective endpoint and auth on every refresh.
    /// This keeps quota observations aligned with account/provider switches in the session.
    pub fn new_with_model_provider(
        provider: SharedModelProvider,
        http_client_factory: HttpClientFactory,
    ) -> Self {
        let info = provider.info().clone();
        let base_url = info.base_url.clone().unwrap_or_default();
        Self {
            client: Client::new(base_url.clone(), http_client_factory.clone()),
            runtime_provider: Some(provider),
            runtime_factory: Some(http_client_factory),
            source: if info.usage_url.is_some() {
                "provider-key".to_string()
            } else {
                "chatgpt-account".to_string()
            },
            scope_hint: "unknown".to_string(),
            usage_url: info.usage_url.as_ref().map(|url| url.as_str().to_string()),
            pool_scope: format!("pool@{}", normalize_endpoint(&base_url)),
            cache: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn snapshot(&self) -> GoalQuotaSnapshotResult {
        Ok(self.snapshot_many().await?.into_iter().next())
    }

    /// Drops the cached observation after an auth/provider identity change.
    pub async fn invalidate_cache(&self) {
        self.cache.lock().await.take();
    }

    /// Refresh the observed scope without postponing the usage refresh for other
    /// sessions' key spend.
    pub async fn observe_response(&self, limit: codex_protocol::protocol::RateLimitSnapshot) {
        let identity = self.current_identity().await;
        if !has_quota_data(&limit) {
            return;
        }
        let scope_id = self.response_scope_id(&identity);
        let mut cache = self.cache.lock().await;
        if cache
            .as_ref()
            .is_none_or(|cached| cached.identity != identity)
        {
            *cache = Some(CachedSnapshot {
                fetched: if self.passive_usage_enabled() {
                    Instant::now() - CACHE_TTL
                } else {
                    Instant::now()
                },
                identity: identity.clone(),
                result: Ok(vec![GoalQuotaSnapshot {
                    captured_at: now_unix_seconds(),
                    source: "provider-pool".to_string(),
                    scope_id,
                    limits: vec![limit],
                }]),
            });
            return;
        }
        let cached = cache.as_mut().expect("cache initialized");
        if !self.passive_usage_enabled() {
            cached.fetched = Instant::now();
        }
        if cached.result.is_err() {
            cached.result = Ok(Vec::new());
        }
        let snapshots = cached
            .result
            .as_mut()
            .expect("observation replaces failed read");
        let snapshot = snapshots
            .iter_mut()
            .find(|snapshot| snapshot.source == "provider-pool");
        let Some(snapshot) = snapshot else {
            snapshots.push(GoalQuotaSnapshot {
                captured_at: now_unix_seconds(),
                source: "provider-pool".to_string(),
                scope_id,
                limits: vec![limit],
            });
            return;
        };
        if let Some(existing) = snapshot
            .limits
            .iter_mut()
            .find(|existing| existing.limit_id == limit.limit_id)
        {
            *existing = limit;
        } else {
            snapshot.limits.push(limit);
        }
        snapshot.captured_at = now_unix_seconds();
    }

    /// Returns current key and pool observations for extension consumers.
    pub async fn snapshot_many(&self) -> Result<Vec<GoalQuotaSnapshot>, String> {
        let identity = self.current_identity().await;
        {
            let cache = self.cache.lock().await;
            if let Some(cached) = cache
                .as_ref()
                .filter(|cached| cached.fetched.elapsed() < CACHE_TTL)
                .filter(|cached| cached.identity == identity)
            {
                return cached.result.clone();
            }
        }

        let mut result = if self.passive_usage_enabled() {
            self.fetch_many().await
        } else {
            Ok(Vec::new())
        };
        if let Some(cached) = self
            .cache
            .lock()
            .await
            .as_ref()
            .filter(|cached| cached.identity == identity)
            && let Ok(previous) = &cached.result
        {
            for observed in previous.iter().filter(|snapshot| {
                snapshot.source == "provider-pool"
                    && now_unix_seconds().saturating_sub(snapshot.captured_at)
                        < CACHE_TTL.as_secs() as i64
            }) {
                if result.is_err() {
                    result = Ok(Vec::new());
                }
                let snapshots = result.as_mut().expect("fresh observation");
                if !snapshots.iter().any(|snapshot| {
                    snapshot.source == observed.source && snapshot.scope_id == observed.scope_id
                }) {
                    snapshots.push(observed.clone());
                }
            }
        }
        let mut cache = self.cache.lock().await;
        *cache = Some(CachedSnapshot {
            fetched: Instant::now(),
            identity,
            result: result.clone(),
        });
        result
    }

    async fn fetch_many(&self) -> Result<Vec<GoalQuotaSnapshot>, String> {
        let (client, usage_url, source, scope_hint, _pool_scope) = if let (
            Some(provider),
            Some(factory),
        ) =
            (&self.runtime_provider, &self.runtime_factory)
        {
            let info = provider.info().clone();
            let base_url = provider
                .runtime_base_url()
                .await
                .map_err(|err| err.to_string())?
                .or(info.base_url.clone())
                .ok_or_else(|| "provider has no base URL".to_string())?;
            let base_url = if info.usage_url.is_none() {
                base_url.trim_end_matches("/codex").to_string()
            } else {
                base_url
            };
            let auth = provider.api_auth().await.map_err(|err| err.to_string())?;
            let client = Client::new(base_url.clone(), factory.clone()).with_auth_provider(auth);
            let account = provider
                .auth()
                .await
                .and_then(|auth| auth.get_account_id())
                .unwrap_or_else(|| "unknown".to_string());
            (
                client,
                info.usage_url.as_ref().map(|url| url.as_str().to_string()),
                if info.usage_url.is_some() {
                    "provider-key"
                } else {
                    "chatgpt-account"
                }
                .to_string(),
                account,
                format!("pool@{}", normalize_endpoint(&base_url)),
            )
        } else {
            (
                self.client.clone(),
                self.usage_url.clone(),
                self.source.clone(),
                self.scope_hint.clone(),
                self.pool_scope.clone(),
            )
        };
        let limits = match usage_url.as_deref() {
            Some(url) => self
                .client_for(&client)
                .get_rate_limits_at(url)
                .await
                .map_err(|err| err.to_string())?,
            None => self
                .client_for(&client)
                .get_rate_limits_with_reset_credits()
                .await
                .map_err(|err| err.to_string())?,
        };
        let scope_id = limits.account_id.or(limits.user_id).unwrap_or(scope_hint);
        let mut snapshots = Vec::new();
        let key_limits: Vec<_> = limits
            .rate_limits
            .into_iter()
            .filter(has_quota_data)
            .collect();
        let pool_limits: Vec<_> = limits
            .header_rate_limits
            .into_iter()
            .filter(has_quota_data)
            .collect();
        if !key_limits.is_empty() {
            snapshots.push(GoalQuotaSnapshot {
                captured_at: now_unix_seconds(),
                source,
                scope_id,
                limits: key_limits,
            });
        }
        if !pool_limits.is_empty() {
            snapshots.push(GoalQuotaSnapshot {
                captured_at: now_unix_seconds(),
                source: "provider-pool".to_string(),
                scope_id: self.response_scope_id(&self.current_identity().await),
                limits: pool_limits,
            });
        }
        Ok(snapshots)
    }

    fn passive_usage_enabled(&self) -> bool {
        self.runtime_provider
            .as_ref()
            .map_or(self.usage_url.is_some(), |provider| {
                let info = provider.info();
                info.usage_url.is_some() || (info.is_openai() && info.base_url.is_none())
            })
    }

    fn client_for(&self, client: &Client) -> Client {
        client.clone()
    }

    fn response_scope_id(&self, identity: &str) -> String {
        format!("{}#{}", self.pool_scope, normalize_endpoint(identity))
    }

    async fn current_identity(&self) -> String {
        if let Some(provider) = &self.runtime_provider {
            let info = provider.info();
            let auth = provider.auth().await;
            let account = auth
                .as_ref()
                .and_then(CodexAuth::get_account_id)
                .unwrap_or_else(|| "unknown".to_string());
            let mode = auth
                .as_ref()
                .map(|value| format!("{:?}", value.auth_mode()))
                .unwrap_or_else(|| "none".to_string());
            let auth_fingerprint = match provider.api_auth().await {
                Ok(auth_provider) => {
                    let mut hasher = Sha1::new();
                    for (name, value) in auth_provider.to_auth_headers().iter() {
                        hasher.update(name.as_str().as_bytes());
                        hasher.update(value.as_bytes());
                    }
                    format!("{:x}", hasher.finalize())
                }
                Err(_) => "auth-error".to_string(),
            };
            return format!(
                "{}|{}|{}|{}",
                normalize_endpoint(info.base_url.as_deref().unwrap_or("")),
                account,
                mode,
                auth_fingerprint
            );
        }
        self.scope_hint.clone()
    }
}

fn now_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn has_quota_data(limit: &codex_protocol::protocol::RateLimitSnapshot) -> bool {
    limit.primary.is_some()
        || limit.secondary.is_some()
        || limit.credits.is_some()
        || limit.individual_limit.is_some()
        || limit.spend_control_reached.is_some()
        || limit.plan_type.is_some()
        || limit.rate_limit_reached_type.is_some()
}

fn normalize_endpoint(base_url: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(base_url.trim_end_matches('/').as_bytes());
    format!("{:x}", hasher.finalize())
}

pub type GoalQuotaSnapshotResult = Result<Option<GoalQuotaSnapshot>, String>;

#[cfg(test)]
#[path = "goal_quota_tests.rs"]
mod tests;
